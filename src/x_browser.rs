//! Isolated, read-only browser inspection of one public X post.
use std::{path::PathBuf, sync::Arc, time::Duration};

use anyhow::{Context, Result, ensure};
use reqwest::Url;
use serde_json::{Value, json};
use tokio::{process::Command, time::timeout};

use crate::permission::{
    PermissionBroker, PermissionDecision, PermissionRequestDraft, PermissionResolutionSource,
};

const MAX_OUTPUT_BYTES: usize = 900_000;

#[derive(Clone)]
pub struct XBrowser {
    permissions: Arc<PermissionBroker>,
    script: PathBuf,
}

impl XBrowser {
    pub fn new(permissions: Arc<PermissionBroker>, workspace: &std::path::Path) -> Self {
        Self {
            permissions,
            script: workspace.join("scripts/x-browser-inspector.mjs"),
        }
    }

    pub async fn inspect(&self, value: &str, purpose: &str, origin: &str) -> Result<Value> {
        let url = validated_post_url(value)?;
        let purpose = purpose.trim();
        ensure!(
            !purpose.is_empty() && purpose.chars().count() <= 300,
            "browser purpose must contain 1–300 characters"
        );
        let resolution = self
            .permissions
            .request(PermissionRequestDraft {
                kind: "networkAccess".to_owned(),
                source: "symbiont".to_owned(),
                origin: origin.to_owned(),
                title: "允许用隔离浏览器读取 X 帖子".to_owned(),
                reason: Some(purpose.to_owned()),
                command: None,
                cwd: None,
                host: url.host_str().map(str::to_owned),
                protocol: Some("https".to_owned()),
                details: json!({"url": url.as_str(), "readOnly": true, "isolatedProfile": true}),
                allow_accept: true,
                allow_session: true,
                allow_cancel: false,
                session_key: Some("browser-read:x-posts".to_owned()),
                timeout: None,
            })
            .await;
        ensure!(
            matches!(
                resolution.decision,
                PermissionDecision::Accept | PermissionDecision::AcceptForSession
            ) && resolution.source != PermissionResolutionSource::BackgroundPolicy,
            "browser access to X was declined by the Host ({:?})",
            resolution.source
        );

        let output = timeout(
            Duration::from_secs(40),
            Command::new("node")
                .arg(&self.script)
                .arg(url.as_str())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .context("X browser inspection timed out")?
        .context("start isolated X browser inspector")?;
        ensure!(
            output.stdout.len() <= MAX_OUTPUT_BYTES,
            "X browser result exceeded size limit"
        );
        ensure!(output.status.success(), "X browser inspection failed");
        let result: Value =
            serde_json::from_slice(&output.stdout).context("decode X browser result")?;
        ensure!(
            result["requestedUrl"] == url.as_str(),
            "X browser returned a different target"
        );
        Ok(result)
    }
}

fn validated_post_url(value: &str) -> Result<Url> {
    ensure!(
        !value.is_empty() && value.len() <= 2_048,
        "X post URL length is invalid"
    );
    let mut url = Url::parse(value).context("parse X post URL")?;
    ensure!(url.scheme() == "https", "X post URL must use HTTPS");
    ensure!(
        url.username().is_empty() && url.password().is_none(),
        "X post URL cannot contain credentials"
    );
    ensure!(url.port().is_none(), "X post URL cannot specify a port");
    ensure!(
        matches!(
            url.host_str(),
            Some("x.com" | "www.x.com" | "twitter.com" | "www.twitter.com")
        ),
        "X post URL must be on x.com or twitter.com"
    );
    let parts: Vec<String> = url
        .path_segments()
        .into_iter()
        .flatten()
        .map(str::to_owned)
        .collect();
    let media_suffix = parts.len() == 5
        && matches!(parts[3].as_str(), "photo" | "video")
        && parts[4].len() <= 2
        && parts[4].bytes().all(|b| b.is_ascii_digit());
    ensure!(
        (parts.len() == 3 || media_suffix)
            && parts[1] == "status"
            && !parts[0].is_empty()
            && parts[0]
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
            && parts[2].len() >= 8
            && parts[2].bytes().all(|b| b.is_ascii_digit()),
        "X browser accepts exact post URLs only"
    );
    if media_suffix {
        url.set_path(&format!("/{}/status/{}", parts[0], parts[2]));
    }
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::validated_post_url;

    #[test]
    fn accepts_exact_posts_only() {
        assert!(validated_post_url("https://x.com/example/status/1234567890123456789").is_ok());
        assert_eq!(
            validated_post_url("https://x.com/example/status/1234567890123456789/photo/1?s=20")
                .unwrap()
                .as_str(),
            "https://x.com/example/status/1234567890123456789"
        );
        for value in [
            "http://x.com/example/status/1234567890",
            "https://x.com.evil.test/example/status/1234567890",
            "https://user@x.com/example/status/1234567890",
            "https://x.com:8443/example/status/1234567890",
            "https://x.com/example",
            "https://x.com/example/status/1234567890/photo/invalid",
            "https://x.com/example/status/not-a-post",
        ] {
            assert!(validated_post_url(value).is_err(), "{value}");
        }
    }
}
