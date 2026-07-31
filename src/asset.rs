use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use imagesize::blob_size;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::fs;

pub const MAX_IMAGE_BYTES: usize = 15 * 1024 * 1024;
pub const MAX_IMAGES_PER_MESSAGE: usize = 4;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageAttachment {
    pub asset_id: String,
    pub url: String,
    pub filename: String,
    pub mime_type: String,
    pub byte_size: usize,
    pub width: u32,
    pub height: u32,
    pub sha256: String,
}

#[derive(Clone, Debug)]
pub struct SavedImage {
    pub attachment: ImageAttachment,
    pub path: PathBuf,
    source: ImageSource,
}

#[derive(Clone, Debug)]
struct ImageSource {
    source_type: String,
    uri: String,
    metadata: Option<Value>,
}

#[derive(Clone)]
pub struct AssetStore {
    root: PathBuf,
}

impl AssetStore {
    pub async fn open(root: PathBuf) -> Result<Self> {
        fs::create_dir_all(&root)
            .await
            .with_context(|| format!("create asset directory {}", root.display()))?;
        Ok(Self { root })
    }

    pub async fn save_image(&self, filename: Option<&str>, bytes: &[u8]) -> Result<SavedImage> {
        if bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES {
            anyhow::bail!("image must contain 1-{MAX_IMAGE_BYTES} bytes");
        }
        let kind = infer::get(bytes).context("attachment is not a recognized image")?;
        let (mime_type, extension) = match kind.mime_type() {
            "image/jpeg" => ("image/jpeg", "jpg"),
            "image/png" => ("image/png", "png"),
            "image/webp" => ("image/webp", "webp"),
            "image/gif" => ("image/gif", "gif"),
            other => anyhow::bail!("unsupported image type: {other}"),
        };
        let dimensions = blob_size(bytes).context("read image dimensions")?;
        let digest = format!("{:x}", Sha256::digest(bytes));
        let asset_id = format!("img_{digest}.{extension}");
        let path = self.root.join(&asset_id);
        if fs::metadata(&path).await.is_err() {
            fs::write(&path, bytes)
                .await
                .with_context(|| format!("persist image asset {}", path.display()))?;
        }
        let filename = safe_filename(filename).unwrap_or_else(|| asset_id.clone());
        Ok(SavedImage {
            attachment: ImageAttachment {
                url: format!("/api/assets/{asset_id}"),
                asset_id,
                filename,
                mime_type: mime_type.to_owned(),
                byte_size: bytes.len(),
                width: dimensions.width as u32,
                height: dimensions.height as u32,
                sha256: digest,
            },
            source: ImageSource {
                source_type: "local_image".to_owned(),
                uri: format!("file://{}", path.display()),
                metadata: None,
            },
            path,
        })
    }

    pub async fn import_generated_image(
        &self,
        path: &Path,
        metadata: Option<Value>,
    ) -> Result<SavedImage> {
        if !path.is_absolute() {
            anyhow::bail!("generated image path must be absolute");
        }
        let file_metadata = fs::metadata(path)
            .await
            .with_context(|| format!("inspect generated image {}", path.display()))?;
        if file_metadata.len() == 0 || file_metadata.len() > MAX_IMAGE_BYTES as u64 {
            anyhow::bail!("generated image must contain 1-{MAX_IMAGE_BYTES} bytes");
        }
        let bytes = fs::read(path)
            .await
            .with_context(|| format!("read generated image {}", path.display()))?;
        let filename = path.file_name().and_then(|value| value.to_str());
        let mut saved = self.save_image(filename, &bytes).await?;
        saved.source = ImageSource {
            source_type: "codex_image_generation".to_owned(),
            uri: format!("file://{}", path.display()),
            metadata,
        };
        Ok(saved)
    }

    pub async fn read(&self, asset_id: &str) -> Result<(Vec<u8>, &'static str)> {
        let mime_type = validate_asset_id(asset_id)?;
        let bytes = fs::read(self.root.join(asset_id))
            .await
            .with_context(|| format!("read image asset {asset_id}"))?;
        Ok((bytes, mime_type))
    }

    pub async fn local_path(&self, asset_id: &str) -> Result<PathBuf> {
        validate_asset_id(asset_id)?;
        fs::canonicalize(self.root.join(asset_id))
            .await
            .with_context(|| format!("resolve image asset {asset_id}"))
    }
}

impl SavedImage {
    pub fn source_type(&self) -> &str {
        &self.source.source_type
    }

    pub fn source_uri(&self) -> &str {
        &self.source.uri
    }

    pub fn source_metadata(&self) -> Option<Value> {
        self.source.metadata.clone()
    }
}

fn safe_filename(filename: Option<&str>) -> Option<String> {
    let filename = filename?;
    let basename = Path::new(filename).file_name()?.to_string_lossy();
    let cleaned = basename
        .chars()
        .filter(|character| !character.is_control())
        .take(180)
        .collect::<String>();
    (!cleaned.trim().is_empty()).then(|| cleaned.trim().to_owned())
}

fn validate_asset_id(asset_id: &str) -> Result<&'static str> {
    let (digest, extension) = asset_id
        .strip_prefix("img_")
        .and_then(|value| value.rsplit_once('.'))
        .context("invalid image asset id")?;
    if digest.len() != 64
        || !digest
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        anyhow::bail!("invalid image asset id");
    }
    match extension {
        "jpg" => Ok("image/jpeg"),
        "png" => Ok("image/png"),
        "webp" => Ok("image/webp"),
        "gif" => Ok("image/gif"),
        _ => anyhow::bail!("invalid image asset extension"),
    }
}

#[cfg(test)]
#[path = "asset/tests.rs"]
mod tests;
