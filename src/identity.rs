use std::{io::ErrorKind, path::PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::{fs, sync::RwLock};

use crate::asset::ImageAttachment;

/// Local presentation preferences. This is deliberately separate from the
/// model-facing profile and durable conversation memory.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IdentitySnapshot {
    /// The visible symbiont-d persona. Kept as `avatar` for local backwards
    /// compatibility with the first presentation preference format.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<ImageAttachment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_avatar: Option<ImageAttachment>,
}

#[derive(Clone, Copy)]
pub enum AvatarSlot {
    Symbiont,
    User,
}

pub struct IdentityStore {
    path: PathBuf,
    snapshot: RwLock<IdentitySnapshot>,
}

impl IdentityStore {
    pub async fn open(path: PathBuf) -> Result<Self> {
        let snapshot = match fs::read_to_string(&path).await {
            Ok(content) => toml::from_str(&content)
                .with_context(|| format!("parse identity settings {}", path.display()))?,
            Err(error) if error.kind() == ErrorKind::NotFound => IdentitySnapshot::default(),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("read identity settings {}", path.display()));
            }
        };
        let store = Self {
            path,
            snapshot: RwLock::new(snapshot),
        };
        store.persist().await?;
        Ok(store)
    }

    pub async fn snapshot(&self) -> IdentitySnapshot {
        self.snapshot.read().await.clone()
    }

    pub async fn set_avatar(
        &self,
        slot: AvatarSlot,
        avatar: Option<ImageAttachment>,
    ) -> Result<IdentitySnapshot> {
        let mut snapshot = self.snapshot.write().await;
        let mut next = snapshot.clone();
        match slot {
            AvatarSlot::Symbiont => next.avatar = avatar,
            AvatarSlot::User => next.user_avatar = avatar,
        }
        persist(&self.path, &next).await?;
        *snapshot = next.clone();
        Ok(next)
    }

    async fn persist(&self) -> Result<()> {
        persist(&self.path, &self.snapshot().await).await
    }
}

async fn persist(path: &PathBuf, snapshot: &IdentitySnapshot) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .with_context(|| format!("create identity settings directory {}", parent.display()))?;
    }
    let content = toml::to_string_pretty(snapshot).context("encode identity settings")?;
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, content)
        .await
        .with_context(|| format!("write identity settings {}", temporary.display()))?;
    fs::rename(&temporary, path)
        .await
        .with_context(|| format!("replace identity settings {}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn avatar() -> ImageAttachment {
        ImageAttachment {
            asset_id: "img_0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef.png"
                .to_owned(),
            url: "/api/assets/img_0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef.png"
                .to_owned(),
            filename: "avatar.png".to_owned(),
            mime_type: "image/png".to_owned(),
            byte_size: 42,
            width: 32,
            height: 32,
            sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                .to_owned(),
        }
    }

    #[tokio::test]
    async fn persists_a_local_avatar_without_profile_state() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("symbiont-identity-{nonce}.toml"));
        let store = IdentityStore::open(path.clone())
            .await
            .expect("open identity");

        assert!(store.snapshot().await.avatar.is_none());
        store
            .set_avatar(AvatarSlot::Symbiont, Some(avatar()))
            .await
            .expect("save symbiont avatar");
        store
            .set_avatar(AvatarSlot::User, Some(avatar()))
            .await
            .expect("save user avatar");
        drop(store);

        let reopened = IdentityStore::open(path.clone())
            .await
            .expect("reopen identity");
        assert_eq!(
            reopened.snapshot().await.avatar.expect("avatar").filename,
            "avatar.png"
        );
        assert_eq!(
            reopened
                .snapshot()
                .await
                .user_avatar
                .expect("user avatar")
                .filename,
            "avatar.png"
        );
        reopened
            .set_avatar(AvatarSlot::Symbiont, None)
            .await
            .expect("clear symbiont avatar");
        reopened
            .set_avatar(AvatarSlot::User, None)
            .await
            .expect("clear user avatar");
        let snapshot = reopened.snapshot().await;
        assert!(snapshot.avatar.is_none());
        assert!(snapshot.user_avatar.is_none());

        std::fs::remove_file(path).expect("remove identity settings");
    }
}
