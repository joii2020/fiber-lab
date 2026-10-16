use super::*;

#[test]
fn legacy_archive_and_new_default_preserve_paths_and_isolate_accounts() {
    let temp = tempfile::tempdir().unwrap();
    let legacy = path(temp.path(), "default");
    let dir = AccountDir::lock(&legacy).unwrap();
    atomic_write(&legacy.join("root-key"), b"original-key").unwrap();
    drop(dir);
    list_legacy(temp.path()).unwrap();
    assert!(!base(temp.path()).join("accounts.json").exists());
    assert_eq!(archive(temp.path(), None).unwrap(), "default");
    assert!(select(temp.path(), None, false).is_err());
    assert_eq!(archive(temp.path(), Some("default")).unwrap(), "default");
    let new = select(temp.path(), None, true).unwrap();
    let current = Registry::load(temp.path()).unwrap().current.unwrap();
    assert_ne!(new.path, legacy);
    let original = select(temp.path(), Some("default"), false).unwrap();
    assert_eq!(original.path, legacy);
    assert_eq!(fs::read(legacy.join("root-key")).unwrap(), b"original-key");
    assert_eq!(
        Registry::load(temp.path()).unwrap().current.unwrap(),
        current
    );
    assert!(select(temp.path(), Some("../../other"), true).is_err());
}

#[test]
fn locked_account_blocks_archive_and_missing_directory_is_not_recreated() {
    let temp = tempfile::tempdir().unwrap();
    let dir = select(temp.path(), None, true).unwrap();
    let id = Registry::load(temp.path()).unwrap().current.unwrap();
    assert!(archive(temp.path(), None).is_err());
    assert!(select(temp.path(), Some(&id), false).is_err());
    drop(dir);
    let account_path = path(temp.path(), &id);
    fs::remove_dir_all(&account_path).unwrap();
    assert!(select(temp.path(), None, true).is_err());
    assert!(!account_path.exists());
}

#[test]
fn invalid_registry_is_rejected_without_modifying_data() {
    let temp = tempfile::tempdir().unwrap();
    drop(select(temp.path(), None, true).unwrap());
    let file = base(temp.path()).join("accounts.json");
    let invalid =
        br#"{"version":1,"current":null,"accounts":[{"id":"../outside","archived":true}]}"#;
    atomic_write(&file, invalid).unwrap();
    assert!(list_legacy(temp.path()).is_err());
    assert!(archive(temp.path(), None).is_err());
    assert!(select(temp.path(), None, true).is_err());
    assert_eq!(fs::read(file).unwrap(), invalid);
}
