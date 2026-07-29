use std::{io::ErrorKind, path::PathBuf};

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use tokio::{fs, sync::RwLock};

const PROFILE_VERSION: u32 = 1;
const MAX_ORIENTATION_CHARS: usize = 32_000;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupStatus {
    Unconfigured,
    Calibrating,
    Ready,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CalibrationMode {
    Description,
    Guided,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileSnapshot {
    pub status: SetupStatus,
    pub mode: Option<CalibrationMode>,
    pub orientation: String,
    pub updated_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProfileState {
    version: u32,
    status: SetupStatus,
    mode: Option<CalibrationMode>,
    updated_at: Option<String>,
}

pub struct ProfileStore {
    state_path: PathBuf,
    orientation_path: PathBuf,
    snapshot: RwLock<ProfileSnapshot>,
}

impl ProfileStore {
    pub async fn open(state_path: PathBuf, orientation_path: PathBuf) -> Result<Self> {
        let state = match fs::read_to_string(&state_path).await {
            Ok(content) => toml::from_str::<ProfileState>(&content)
                .with_context(|| format!("parse profile state {}", state_path.display()))?,
            Err(error) if error.kind() == ErrorKind::NotFound => ProfileState::default(),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("read profile state {}", state_path.display()));
            }
        };
        let orientation = match fs::read_to_string(&orientation_path).await {
            Ok(content) => content.trim().to_owned(),
            Err(error) if error.kind() == ErrorKind::NotFound => String::new(),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("read orientation document {}", orientation_path.display())
                });
            }
        };
        let mut snapshot = ProfileSnapshot {
            status: state.status,
            mode: state.mode,
            orientation,
            updated_at: state.updated_at,
        };
        if snapshot.status == SetupStatus::Ready && snapshot.orientation.is_empty() {
            snapshot.status = SetupStatus::Unconfigured;
            snapshot.mode = None;
            snapshot.updated_at = Some(now());
        }

        let store = Self {
            state_path,
            orientation_path,
            snapshot: RwLock::new(snapshot),
        };
        store.persist_state().await?;
        Ok(store)
    }

    pub async fn snapshot(&self) -> ProfileSnapshot {
        self.snapshot.read().await.clone()
    }

    pub async fn begin(&self, mode: CalibrationMode) -> Result<ProfileSnapshot> {
        let mut snapshot = self.snapshot.write().await;
        if snapshot.status == SetupStatus::Ready {
            anyhow::bail!("symbiont-d is already initialized");
        }
        snapshot.status = SetupStatus::Calibrating;
        snapshot.mode = Some(mode);
        snapshot.updated_at = Some(now());
        let result = snapshot.clone();
        drop(snapshot);
        self.persist_state().await?;
        Ok(result)
    }

    pub async fn complete(&self, orientation: &str) -> Result<ProfileSnapshot> {
        let orientation = normalize_orientation(orientation)?;
        let mut snapshot = self.snapshot.write().await;
        if snapshot.status != SetupStatus::Calibrating {
            anyhow::bail!("orientation can only be completed during calibration");
        }
        persist_text(&self.orientation_path, &orientation).await?;
        snapshot.status = SetupStatus::Ready;
        snapshot.orientation = orientation;
        snapshot.updated_at = Some(now());
        let result = snapshot.clone();
        drop(snapshot);
        self.persist_state().await?;
        Ok(result)
    }

    pub async fn update_orientation(&self, orientation: &str) -> Result<ProfileSnapshot> {
        let orientation = normalize_orientation(orientation)?;
        let mut snapshot = self.snapshot.write().await;
        if snapshot.status != SetupStatus::Ready {
            anyhow::bail!("orientation cannot be edited before initialization is complete");
        }
        persist_text(&self.orientation_path, &orientation).await?;
        snapshot.orientation = orientation;
        snapshot.updated_at = Some(now());
        let result = snapshot.clone();
        drop(snapshot);
        self.persist_state().await?;
        Ok(result)
    }

    async fn persist_state(&self) -> Result<()> {
        let snapshot = self.snapshot.read().await;
        let state = ProfileState {
            version: PROFILE_VERSION,
            status: snapshot.status,
            mode: snapshot.mode,
            updated_at: snapshot.updated_at.clone(),
        };
        let content = toml::to_string_pretty(&state).context("encode profile state")?;
        drop(snapshot);
        persist_text(&self.state_path, &content).await
    }
}

impl Default for ProfileState {
    fn default() -> Self {
        Self {
            version: PROFILE_VERSION,
            status: SetupStatus::Unconfigured,
            mode: None,
            updated_at: None,
        }
    }
}

fn normalize_orientation(orientation: &str) -> Result<String> {
    let orientation = orientation.trim();
    if orientation.is_empty() {
        anyhow::bail!("orientation cannot be empty");
    }
    if orientation.chars().count() > MAX_ORIENTATION_CHARS {
        anyhow::bail!("orientation exceeds {MAX_ORIENTATION_CHARS} characters");
    }
    Ok(format!("{orientation}\n"))
}

async fn persist_text(path: &PathBuf, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .with_context(|| format!("create profile directory {}", parent.display()))?;
    }
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, content)
        .await
        .with_context(|| format!("write {}", temporary.display()))?;
    fs::rename(&temporary, path)
        .await
        .with_context(|| format!("replace {}", path.display()))
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[cfg(test)]
#[path = "profile/tests.rs"]
mod tests;
