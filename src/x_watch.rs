//! User-owned X account watch rules and progress from explicit browser checks.

use std::{io::ErrorKind, path::PathBuf};

use anyhow::{Context, Result, ensure};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use tokio::{fs, sync::Mutex};

const MAX_WATCHES: usize = 30;
const MAX_RECENT: usize = 20;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WatchDelivery {
    #[default]
    Digest,
    Important,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchPost {
    pub id: String,
    pub url: String,
    pub text: String,
    pub posted_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct WatchRecord {
    id: String,
    handle: String,
    focus: String,
    delivery: WatchDelivery,
    enabled: bool,
    cursor: Option<String>,
    last_checked_at: Option<String>,
    #[serde(default)]
    last_new_count: usize,
    #[serde(default)]
    recent_posts: Vec<WatchPost>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct WatchDocument {
    #[serde(default)]
    watches: Vec<WatchRecord>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchView {
    pub id: String,
    pub handle: String,
    pub focus: String,
    pub delivery: WatchDelivery,
    pub enabled: bool,
    pub status: &'static str,
    pub last_seen_post_id: Option<String>,
    pub last_checked_at: Option<String>,
    pub last_new_count: usize,
    pub recent_posts: Vec<WatchPost>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchSnapshot {
    pub watches: Vec<WatchView>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(
    tag = "action",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum WatchCommand {
    List,
    Add {
        handle: String,
        focus: String,
        #[serde(default)]
        delivery: WatchDelivery,
    },
    Update {
        key: String,
        focus: Option<String>,
        delivery: Option<WatchDelivery>,
    },
    Pause {
        key: String,
    },
    Resume {
        key: String,
    },
    Remove {
        key: String,
    },
    /// Only after the assistant has actually inspected this account in the
    /// user-connected browser. No background scheduler calls this action.
    RecordCheck {
        key: String,
        posts: Vec<WatchPost>,
    },
}

pub struct XWatchStore {
    path: PathBuf,
    document: Mutex<WatchDocument>,
}

impl XWatchStore {
    pub async fn open(path: PathBuf) -> Result<Self> {
        let document = match fs::read(&path).await {
            Ok(bytes) => serde_json::from_slice(&bytes).context("decode X account watches")?,
            Err(error) if error.kind() == ErrorKind::NotFound => WatchDocument::default(),
            Err(error) => return Err(error).context("read X account watches"),
        };
        Ok(Self {
            path,
            document: Mutex::new(document),
        })
    }

    pub async fn snapshot(&self) -> WatchSnapshot {
        let document = self.document.lock().await;
        WatchSnapshot {
            watches: document
                .watches
                .iter()
                .map(|watch| WatchView {
                    id: watch.id.clone(),
                    handle: watch.handle.clone(),
                    focus: watch.focus.clone(),
                    delivery: watch.delivery,
                    enabled: watch.enabled,
                    status: if !watch.enabled {
                        "paused"
                    } else if watch.last_checked_at.is_none() {
                        "waiting_first_check"
                    } else {
                        "ready"
                    },
                    last_seen_post_id: watch.cursor.clone(),
                    last_checked_at: watch.last_checked_at.clone(),
                    last_new_count: watch.last_new_count,
                    recent_posts: watch.recent_posts.clone(),
                })
                .collect(),
        }
    }

    pub async fn manage(&self, command: WatchCommand) -> Result<WatchSnapshot> {
        if !matches!(command, WatchCommand::List) {
            let mut stored = self.document.lock().await;
            let mut document = stored.clone();
            match command {
                WatchCommand::List => unreachable!(),
                WatchCommand::Add {
                    handle,
                    focus,
                    delivery,
                } => {
                    let handle = validated_handle(&handle)?;
                    validate_focus(&focus)?;
                    ensure!(
                        document.watches.len() < MAX_WATCHES,
                        "X watch limit reached"
                    );
                    ensure!(
                        !document
                            .watches
                            .iter()
                            .any(|item| item.handle.eq_ignore_ascii_case(&handle)),
                        "X account is already watched"
                    );
                    document.watches.push(WatchRecord {
                        id: new_id()?,
                        handle,
                        focus: focus.trim().to_owned(),
                        delivery,
                        enabled: true,
                        cursor: None,
                        last_checked_at: None,
                        last_new_count: 0,
                        recent_posts: Vec::new(),
                    });
                }
                WatchCommand::Update {
                    key,
                    focus,
                    delivery,
                } => {
                    let item = find_watch_mut(&mut document, &key)?;
                    if let Some(focus) = focus {
                        validate_focus(&focus)?;
                        item.focus = focus.trim().to_owned();
                    }
                    if let Some(delivery) = delivery {
                        item.delivery = delivery;
                    }
                }
                WatchCommand::Pause { key } => find_watch_mut(&mut document, &key)?.enabled = false,
                WatchCommand::Resume { key } => find_watch_mut(&mut document, &key)?.enabled = true,
                WatchCommand::Remove { key } => {
                    let before = document.watches.len();
                    document.watches.retain(|item| !matches_key(item, &key));
                    ensure!(before != document.watches.len(), "X watch not found");
                }
                WatchCommand::RecordCheck { key, posts } => {
                    let item = find_watch_mut(&mut document, &key)?;
                    ensure!(item.enabled, "X watch is paused");
                    ensure!(posts.len() <= MAX_RECENT, "too many X posts in one check");
                    let mut ids = std::collections::HashSet::new();
                    for post in &posts {
                        validate_post(post, &item.handle)?;
                        ensure!(ids.insert(&post.id), "duplicate X post");
                    }
                    let previous = item.cursor.clone();
                    item.last_new_count = if item.last_checked_at.is_some() {
                        posts
                            .iter()
                            .filter(|post| {
                                previous
                                    .as_deref()
                                    .is_none_or(|cursor| newer(&post.id, cursor))
                            })
                            .count()
                    } else {
                        0
                    };
                    for post in posts {
                        if item
                            .cursor
                            .as_deref()
                            .is_none_or(|cursor| newer(&post.id, cursor))
                        {
                            item.cursor = Some(post.id.clone());
                        }
                        if !item.recent_posts.iter().any(|old| old.id == post.id) {
                            item.recent_posts.push(post);
                        }
                    }
                    item.recent_posts.sort_by(|a, b| {
                        b.id.parse::<u128>()
                            .unwrap_or_default()
                            .cmp(&a.id.parse::<u128>().unwrap_or_default())
                    });
                    item.recent_posts.truncate(MAX_RECENT);
                    item.last_checked_at =
                        Some(Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true));
                }
            }
            persist(&self.path, &document).await?;
            *stored = document;
        }
        Ok(self.snapshot().await)
    }
}

fn matches_key(item: &WatchRecord, key: &str) -> bool {
    item.id == key
        || item
            .handle
            .eq_ignore_ascii_case(key.trim_start_matches('@'))
}

fn find_watch_mut<'a>(document: &'a mut WatchDocument, key: &str) -> Result<&'a mut WatchRecord> {
    document
        .watches
        .iter_mut()
        .find(|item| matches_key(item, key))
        .context("X watch not found")
}

fn validated_handle(value: &str) -> Result<String> {
    let handle = value.trim().trim_start_matches('@');
    ensure!(
        !handle.is_empty()
            && handle.len() <= 15
            && handle
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_'),
        "X handle must be 1–15 ASCII letters, digits or underscores"
    );
    Ok(handle.to_owned())
}

fn validate_focus(focus: &str) -> Result<()> {
    ensure!(
        !focus.trim().is_empty() && focus.chars().count() <= 300,
        "X watch focus must contain 1–300 characters"
    );
    Ok(())
}

fn validate_post(post: &WatchPost, handle: &str) -> Result<()> {
    ensure!(
        (8..=24).contains(&post.id.len()) && post.id.bytes().all(|b| b.is_ascii_digit()),
        "X post id is invalid"
    );
    ensure!(post.text.chars().count() <= 1600, "X post text is too long");
    ensure!(
        post.url
            .eq_ignore_ascii_case(&format!("https://x.com/{handle}/status/{}", post.id)),
        "X post URL must match the watched account and id"
    );
    if let Some(posted_at) = &post.posted_at {
        ensure!(
            posted_at.len() <= 80 && DateTime::parse_from_rfc3339(posted_at).is_ok(),
            "X post timestamp must be RFC 3339"
        );
    }
    Ok(())
}

fn newer(a: &str, b: &str) -> bool {
    match (a.parse::<u128>(), b.parse::<u128>()) {
        (Ok(a), Ok(b)) => a > b,
        _ => false,
    }
}

fn new_id() -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)?;
    Ok(format!(
        "watch_{}",
        bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    ))
}

async fn persist(path: &PathBuf, document: &WatchDocument) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await?;
    }
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(document)?).await?;
    fs::rename(temporary, path)
        .await
        .context("persist X account watches")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn watches_survive_restart_and_invalid_updates_are_atomic() {
        let root = std::env::temp_dir().join(format!("symbiont-x-watch-{}", new_id().unwrap()));
        let path = root.join("watches.json");
        let store = XWatchStore::open(path.clone()).await.unwrap();
        store
            .manage(WatchCommand::Add {
                handle: "@Example".into(),
                focus: "模型发布".into(),
                delivery: WatchDelivery::Digest,
            })
            .await
            .unwrap();
        assert!(
            store
                .manage(WatchCommand::Update {
                    key: "example".into(),
                    focus: Some("".into()),
                    delivery: Some(WatchDelivery::Important)
                })
                .await
                .is_err()
        );
        let view = &store.snapshot().await.watches[0];
        assert_eq!(view.focus, "模型发布");
        assert_eq!(view.delivery, WatchDelivery::Digest);
        store
            .manage(WatchCommand::Pause {
                key: "example".into(),
            })
            .await
            .unwrap();
        assert_eq!(
            XWatchStore::open(path)
                .await
                .unwrap()
                .snapshot()
                .await
                .watches[0]
                .status,
            "paused"
        );
        fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn browser_check_establishes_baseline_then_counts_new_posts() {
        let root = std::env::temp_dir().join(format!("symbiont-x-watch-{}", new_id().unwrap()));
        let path = root.join("watches.json");
        let store = XWatchStore::open(path.clone()).await.unwrap();
        store
            .manage(WatchCommand::Add {
                handle: "example".into(),
                focus: "发布".into(),
                delivery: WatchDelivery::Digest,
            })
            .await
            .unwrap();
        let post = |id: &str| WatchPost {
            id: id.into(),
            url: format!("https://x.com/example/status/{id}"),
            text: "post".into(),
            posted_at: None,
        };
        store
            .manage(WatchCommand::RecordCheck {
                key: "example".into(),
                posts: vec![post("12345678")],
            })
            .await
            .unwrap();
        assert_eq!(store.snapshot().await.watches[0].last_new_count, 0);
        store
            .manage(WatchCommand::RecordCheck {
                key: "example".into(),
                posts: vec![post("12345679"), post("12345678")],
            })
            .await
            .unwrap();
        let view = &store.snapshot().await.watches[0];
        assert_eq!(view.last_new_count, 1);
        assert_eq!(view.last_seen_post_id.as_deref(), Some("12345679"));
        assert!(
            store
                .manage(WatchCommand::RecordCheck {
                    key: "example".into(),
                    posts: vec![WatchPost {
                        url: "https://evil.example/".into(),
                        ..post("12345680")
                    }]
                })
                .await
                .is_err()
        );
        assert_eq!(
            store.snapshot().await.watches[0]
                .last_seen_post_id
                .as_deref(),
            Some("12345679")
        );
        fs::remove_dir_all(root).await.unwrap();
    }
}
