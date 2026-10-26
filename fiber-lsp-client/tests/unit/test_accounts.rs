use super::*;

fn options(id: &str) -> crate::ClientOptions {
    crate::ClientOptions {
        selector: crate::Selector {
            user: None,
            account: Some(id.into()),
        },
        round: None,
        connection: crate::ConnectionOptions::default(),
    }
}

#[test]
fn legacy_selection_and_verified_archive_preserve_paths() {
    let temp = tempfile::tempdir().unwrap();
    let legacy = path(temp.path(), "default");
    let dir = AccountDir::lock(&legacy).unwrap();
    atomic_write(&legacy.join("root-key"), b"original-key").unwrap();
    drop(dir);
    list_legacy(temp.path()).unwrap();
    assert!(!base(temp.path()).join("accounts.json").exists());

    let (dir, selection) = choose(temp.path(), &options("default"), false, false, false).unwrap();
    assert_eq!(dir.path, legacy);
    let channel = fiber_json_types::Hash256([3; 32]);
    let hash = ckb_types::H256::from([4; 32]);
    atomic_write(
        &dir.path.join("channel.json"),
        &serde_json::to_vec(&serde_json::json!({
            "stage": "closed", "close_hash": hash,
            "opening": { "channel_id": channel }
        }))
        .unwrap(),
    )
    .unwrap();
    drop(dir);
    archive_verified(temp.path(), &selection, channel, &hash).unwrap();
    let before = fs::read(base(temp.path()).join("accounts.json")).unwrap();
    archive_verified(temp.path(), &selection, channel, &hash).unwrap();
    assert_eq!(
        fs::read(base(temp.path()).join("accounts.json")).unwrap(),
        before
    );
    let registry = Registry::load(temp.path()).unwrap();
    assert!(registry.current.is_none());
    assert!(registry.accounts[0].archived);
    let (dir, _) = choose(temp.path(), &options("default"), false, false, false).unwrap();
    assert_eq!(dir.path, legacy);
    assert_eq!(fs::read(legacy.join("root-key")).unwrap(), b"original-key");
    assert!(choose(temp.path(), &options("../../other"), false, false, false).is_err());
}

#[test]
fn legacy_lock_blocks_archive_and_missing_directory_is_not_recreated() {
    let temp = tempfile::tempdir().unwrap();
    let legacy = path(temp.path(), "default");
    let dir = AccountDir::lock(&legacy).unwrap();
    atomic_write(&legacy.join("root-key"), b"original-key").unwrap();
    drop(dir);
    let (dir, selection) = choose(temp.path(), &options("default"), false, false, false).unwrap();
    assert!(
        archive_verified(
            temp.path(),
            &selection,
            fiber_json_types::Hash256([3; 32]),
            &ckb_types::H256::from([4; 32])
        )
        .is_err()
    );
    assert!(choose(temp.path(), &options("default"), false, false, false).is_err());
    // Keep the historical entry after removing its directory.
    atomic_write(
        &base(temp.path()).join("accounts.json"),
        br#"{"version":1,"current":"default","accounts":[{"id":"default","archived":false}]}"#,
    )
    .unwrap();
    drop(dir);
    fs::remove_dir_all(&legacy).unwrap();
    assert!(choose(temp.path(), &options("default"), false, false, false).is_err());
    assert!(!legacy.exists());
}

#[test]
fn invalid_registry_is_rejected_without_modifying_data() {
    let temp = tempfile::tempdir().unwrap();
    drop(AccountDir::lock(&path(temp.path(), "default")).unwrap());
    let file = base(temp.path()).join("accounts.json");
    let invalid =
        br#"{"version":1,"current":null,"accounts":[{"id":"../outside","archived":true}]}"#;
    atomic_write(&file, invalid).unwrap();
    assert!(list_legacy(temp.path()).is_err());
    assert!(choose(temp.path(), &options("default"), false, false, false).is_err());
    assert_eq!(fs::read(file).unwrap(), invalid);
}
