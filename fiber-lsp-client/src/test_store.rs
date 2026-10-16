use super::*;
use fiber_lsp_sdk::{RootKey, RootSigner};

#[tokio::test]
async fn identity_and_store_survive_reopen() {
    let temp = tempfile::tempdir().unwrap();
    let dir = AccountDir::lock(&temp.path().join("account")).unwrap();
    let store = FileSignerStore::new(&dir);
    let created = RootSigner::create_random(store.clone()).await.unwrap();
    let identity = created.root_signer.identity_public_key();
    let backup = created.root_key_backup.expose_secret();
    store.put(b"key", b"value").await.unwrap();
    let reopened = FileSignerStore::open(&dir).unwrap();
    assert_eq!(reopened.get(b"key").await.unwrap(), Some(b"value".to_vec()));
    let root = RootSigner::open(RootKey::import(backup).unwrap(), reopened.clone())
        .await
        .unwrap();
    assert_eq!(root.identity_public_key(), identity);
    assert!(
        RootSigner::open(RootKey::import([1; 32]).unwrap(), reopened)
            .await
            .is_err()
    );
    assert_eq!(
        fs::metadata(&dir.path).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(dir.path.join("signer.bin"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[tokio::test]
async fn conditional_writes_do_not_overwrite_and_only_one_contender_wins() {
    let temp = tempfile::tempdir().unwrap();
    let dir = AccountDir::lock(&temp.path().join("account")).unwrap();
    let store = FileSignerStore::new(&dir);
    assert!(store.insert_if_absent(b"key", b"first").await.unwrap());
    assert!(!store.insert_if_absent(b"key", b"other").await.unwrap());
    assert!(
        !store
            .compare_and_swap(b"key", None, b"other")
            .await
            .unwrap()
    );
    assert!(
        !store
            .compare_and_swap(b"key", Some(b"wrong"), b"other")
            .await
            .unwrap()
    );
    let (a, b) = tokio::join!(
        store.compare_and_swap(b"key", Some(b"first"), b"a"),
        store.compare_and_swap(b"key", Some(b"first"), b"b"),
    );
    assert_ne!(a.unwrap(), b.unwrap());
    let reopened = FileSignerStore::open(&dir).unwrap();
    assert_eq!(
        reopened.get(b"key").await.unwrap(),
        store.get(b"key").await.unwrap()
    );
}

#[test]
fn corrupt_snapshot_and_partial_initialization_are_preserved() {
    let temp = tempfile::tempdir().unwrap();
    let dir = AccountDir::lock(&temp.path().join("account")).unwrap();
    atomic_write(&dir.path.join("signer.bin"), b"invalid snapshot").unwrap();
    assert!(
        FileSignerStore::open(&dir)
            .err()
            .unwrap()
            .to_string()
            .contains("损坏")
    );
    assert!(
        dir.is_initialized()
            .unwrap_err()
            .to_string()
            .contains("root-key")
    );
    assert_eq!(
        fs::read(dir.path.join("signer.bin")).unwrap(),
        b"invalid snapshot"
    );
}

#[test]
fn account_lock_excludes_other_openers_and_releases_on_drop() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("account");
    let first = AccountDir::lock(&path).unwrap();
    assert!(AccountDir::lock(&path).is_err());
    drop(first);
    assert!(AccountDir::lock(&path).is_ok());
}

#[tokio::test]
async fn failed_write_stops_further_operations_and_preserves_snapshot() {
    let temp = tempfile::tempdir().unwrap();
    let dir = AccountDir::lock(&temp.path().join("account")).unwrap();
    let store = FileSignerStore::new(&dir);
    store.put(b"key", b"original").await.unwrap();
    let path = dir.path.join("signer.bin");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(store.put(b"key", b"new").await.is_err());
    assert!(store.get(b"key").await.is_err());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        FileSignerStore::open(&dir)
            .unwrap()
            .get(b"key")
            .await
            .unwrap(),
        Some(b"original".to_vec())
    );
}
