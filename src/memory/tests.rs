use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use super::{
    ENTRY_END, ENTRY_HEADER_END, ENTRY_PREFIX, EntryHeader, MemoryEntry, MemoryRole, MemoryStore,
};

#[test]
fn serializes_message_identity_for_the_browser_contract() {
    let value = serde_json::to_value(MemoryEntry {
        role: MemoryRole::Assistant,
        at: "2026-07-29T10:00:01Z".to_owned(),
        content: "A message".to_owned(),
        revision_id: Some("rev_message".to_owned()),
        parts: Vec::new(),
        metadata: None,
        delivery_state: None,
    })
    .expect("serialize message");

    assert_eq!(value["revisionId"], "rev_message");
    assert!(value.get("revision_id").is_none());
}

#[tokio::test]
async fn imports_legacy_human_readable_entries() {
    let path = unique_memory_path("legacy-import");
    let headers = [
        EntryHeader {
            role: MemoryRole::User,
            at: "2026-07-29T10:00:00Z".to_owned(),
            metadata: None,
        },
        EntryHeader {
            role: MemoryRole::Assistant,
            at: "2026-07-29T10:00:01Z".to_owned(),
            metadata: None,
        },
    ];
    let content = format!(
        "# symbiont-d memory\n\n{ENTRY_PREFIX}{}{ENTRY_HEADER_END}\nRemember this\n{ENTRY_END}\n\n{ENTRY_PREFIX}{}{ENTRY_HEADER_END}\nI will\n{ENTRY_END}\n\n",
        serde_json::to_string(&headers[0]).unwrap(),
        serde_json::to_string(&headers[1]).unwrap(),
    );
    tokio::fs::write(&path, content).await.unwrap();
    let store = MemoryStore::open(path.clone()).await.unwrap();

    let entries = store.all_entries().await.unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].role, MemoryRole::User);
    assert_eq!(entries[0].content, "Remember this");
    assert_eq!(entries[1].role, MemoryRole::Assistant);
    assert_eq!(entries[1].content, "I will");

    std::fs::remove_file(path).unwrap();
}

fn unique_memory_path(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("symbiont-d-{label}-{nonce}.md"))
}
