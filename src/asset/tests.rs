use std::{
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use super::AssetStore;
use serde_json::json;

const ONE_PIXEL_PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0,
    0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 8, 215, 99, 248, 207, 192, 240, 31, 0, 5,
    0, 1, 255, 137, 153, 61, 29, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

#[tokio::test]
async fn stores_images_by_content_hash() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("symbiont-assets-{nonce}"));
    let store = AssetStore::open(root.clone()).await.expect("open store");

    let first = store
        .save_image(Some("../pixel.png"), ONE_PIXEL_PNG)
        .await
        .expect("save image");
    let second = store
        .save_image(Some("again.png"), ONE_PIXEL_PNG)
        .await
        .expect("deduplicate image");
    assert_eq!(first.attachment.asset_id, second.attachment.asset_id);
    assert_eq!(first.attachment.filename, "pixel.png");
    assert_eq!((first.attachment.width, first.attachment.height), (1, 1));
    assert_eq!(
        store.read(&first.attachment.asset_id).await.unwrap().0,
        ONE_PIXEL_PNG
    );
    let local_path = store.local_path(&first.attachment.asset_id).await.unwrap();
    assert_eq!(
        local_path.file_name().and_then(|value| value.to_str()),
        Some(first.attachment.asset_id.as_str())
    );
    assert!(local_path.is_absolute());

    let _ = tokio::fs::remove_dir_all(root).await;
}

#[tokio::test]
async fn imports_generated_images_into_canonical_storage_with_source_metadata() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("symbiont-generated-assets-{nonce}"));
    let generated = root.join("codex-output.png");
    tokio::fs::create_dir_all(&root).await.unwrap();
    tokio::fs::write(&generated, ONE_PIXEL_PNG).await.unwrap();
    let store = AssetStore::open(root.join("assets"))
        .await
        .expect("open store");

    let saved = store
        .import_generated_image(
            Path::new(&generated),
            Some(json!({"codexItemId": "image-item-1"})),
        )
        .await
        .expect("import generated image");

    assert_eq!(saved.source_type(), "codex_image_generation");
    assert_eq!(
        saved.source_uri(),
        format!("file://{}", generated.display())
    );
    assert_eq!(
        saved.source_metadata().unwrap()["codexItemId"],
        "image-item-1"
    );
    assert!(saved.path.starts_with(root.join("assets")));
    assert_eq!(
        store.read(&saved.attachment.asset_id).await.unwrap().0,
        ONE_PIXEL_PNG
    );

    let _ = tokio::fs::remove_dir_all(root).await;
}
