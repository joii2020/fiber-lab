use super::*;

impl Registry {
    fn save(&self, root: &Path) -> Result<()> {
        atomic_write(
            &base(root).join("accounts.json"),
            &serde_json::to_vec_pretty(self)?,
        )
    }
}

/// Lock order is registry -> account. The registry lock is released before RPC.
pub(crate) fn select(root: &Path, requested: Option<&str>, create: bool) -> Result<AccountDir> {
    let _registry_lock = AccountDir::lock_wait(&base(root).join(".registry"))?;
    let mut registry = Registry::load(root)?;
    let mut newly_allocated = false;
    let id = if let Some(id) = requested {
        ensure!(
            registry.accounts.iter().any(|e| e.id == id),
            "未知账户 {id}；请执行 ./lsp-cli list-accounts"
        );
        id.to_owned()
    } else if let Some(id) = &registry.current {
        id.clone()
    } else {
        ensure!(
            create,
            "当前没有默认账户；请执行 ./lsp-cli create-user，或用 --account ID 选择历史账户"
        );
        let id = format!(
            "account-{:x}",
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        );
        ensure!(!path(root, &id).try_exists()?, "新账户目录已存在；请重试");
        newly_allocated = true;
        registry.accounts.push(Entry {
            id: id.clone(),
            archived: false,
        });
        registry.current = Some(id.clone());
        id
    };
    let account_path = path(root, &id);
    // Existing entries must never silently recreate a missing directory.
    ensure!(
        account_path.try_exists()? || (create && requested.is_none() && newly_allocated),
        "账户 {id} 目录缺失；原清单已保留"
    );
    let dir = AccountDir::lock(&account_path)?;
    registry.save(root)?;
    println!("账户 ID：{id}");
    Ok(dir)
}

pub(crate) fn archive(root: &Path, requested: Option<&str>) -> Result<String> {
    let _registry_lock = AccountDir::lock_wait(&base(root).join(".registry"))?;
    let mut registry = Registry::load(root)?;
    let id = requested
        .or(registry.current.as_deref())
        .context("当前没有默认账户；请执行 ./lsp-cli list-accounts")?
        .to_owned();
    let entry = registry
        .accounts
        .iter_mut()
        .find(|e| e.id == id)
        .context("未知账户；请执行 ./lsp-cli list-accounts")?;
    ensure!(path(root, &id).try_exists()?, "账户目录缺失；原清单已保留");
    let _account_lock = AccountDir::lock(&path(root, &id))?;
    entry.archived = true;
    if registry.current.as_deref() == Some(&id) {
        registry.current = None;
    }
    registry.save(root)?;
    Ok(id)
}

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
