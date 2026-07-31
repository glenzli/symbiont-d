use anyhow::Result;
use serde_json::json;

use super::GeneratedImageOutput;
use crate::asset::{AssetStore, MAX_IMAGES_PER_MESSAGE, SavedImage};

pub async fn import_generated_images(
    assets: &AssetStore,
    outputs: &[GeneratedImageOutput],
) -> Result<Vec<SavedImage>> {
    if outputs.len() > MAX_IMAGES_PER_MESSAGE {
        anyhow::bail!(
            "Codex returned {} images; a message can contain at most {MAX_IMAGES_PER_MESSAGE}",
            outputs.len()
        );
    }
    let mut images = Vec::with_capacity(outputs.len());
    for output in outputs {
        images.push(
            assets
                .import_generated_image(
                    &output.saved_path,
                    Some(json!({
                        "codexItemId": output.item_id,
                        "revisedPrompt": output.revised_prompt
                    })),
                )
                .await?,
        );
    }
    Ok(images)
}
