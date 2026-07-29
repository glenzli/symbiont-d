use std::{io::ErrorKind, path::PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::fs;

use crate::asset::ImageAttachment;

const FILE_HEADER: &str = "# symbiont-d memory\n\n";
const ENTRY_PREFIX: &str = "<!-- symbiont-d:entry ";
const ENTRY_HEADER_END: &str = " -->";
const ENTRY_END: &str = "<!-- symbiont-d:end -->";
const ESCAPED_ENTRY_END: &str = "<!-- symbiont-d:end-escaped -->";

#[derive(Clone)]
pub struct MemoryStore {
    path: PathBuf,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryRole {
    User,
    Assistant,
    Memory,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MemoryEntry {
    pub role: MemoryRole,
    pub at: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<MessagePart>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<MessageMetadata>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum MessagePart {
    Markdown { text: String },
    Image { asset: ImageAttachment },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageMetadata {
    pub runs: Vec<MessageRunMetadata>,
    pub total_tokens: u64,
    pub duration_ms: u64,
    pub tool_calls: u64,
    #[serde(default)]
    pub pcp_tool_calls: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageRunMetadata {
    pub model: String,
    pub display_name: String,
    pub effort: String,
    pub lane: String,
    pub total_tokens: u64,
    pub duration_ms: u64,
}

#[derive(Deserialize, Serialize)]
struct EntryHeader {
    role: MemoryRole,
    at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    metadata: Option<MessageMetadata>,
}

impl MemoryStore {
    pub async fn open(path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .await
                .with_context(|| format!("create memory directory {}", parent.display()))?;
        }

        match fs::metadata(&path).await {
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {
                fs::write(&path, FILE_HEADER)
                    .await
                    .with_context(|| format!("initialize memory file {}", path.display()))?;
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("inspect memory file {}", path.display()));
            }
        }

        Ok(Self { path })
    }

    pub async fn all_entries(&self) -> Result<Vec<MemoryEntry>> {
        Ok(parse_entries(&self.read_all().await?))
    }

    pub fn source_uri(&self) -> String {
        format!("file://{}", self.path.display())
    }

    async fn read_all(&self) -> Result<String> {
        fs::read_to_string(&self.path)
            .await
            .with_context(|| format!("read memory file {}", self.path.display()))
    }
}

fn parse_entries(content: &str) -> Vec<MemoryEntry> {
    let mut entries = Vec::new();
    let mut cursor = content;

    while let Some(prefix_at) = cursor.find(ENTRY_PREFIX) {
        cursor = &cursor[prefix_at + ENTRY_PREFIX.len()..];
        let Some(header_end) = cursor.find(ENTRY_HEADER_END) else {
            break;
        };
        let header_text = &cursor[..header_end];
        cursor = &cursor[header_end + ENTRY_HEADER_END.len()..];
        cursor = cursor.strip_prefix('\n').unwrap_or(cursor);

        let Some(entry_end) = cursor.find(ENTRY_END) else {
            break;
        };
        let body = cursor[..entry_end].trim_end_matches('\n');
        cursor = &cursor[entry_end + ENTRY_END.len()..];

        if let Ok(header) = serde_json::from_str::<EntryHeader>(header_text) {
            entries.push(MemoryEntry {
                role: header.role,
                at: header.at,
                content: body.replace(ESCAPED_ENTRY_END, ENTRY_END),
                revision_id: None,
                parts: vec![MessagePart::Markdown {
                    text: body.replace(ESCAPED_ENTRY_END, ENTRY_END),
                }],
                metadata: header.metadata,
            });
        }
    }

    entries
}

#[cfg(test)]
#[path = "memory/tests.rs"]
mod tests;
