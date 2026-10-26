use super::*;
fn bindings() -> Vec<FundingBinding> {
    (6..=9)
        .map(|index| {
            let mut script = crate::wallet::tests::terms().wallet_lock;
            script.args = ckb_jsonrpc_types::JsonBytes::from_vec(vec![index as u8; 20]);
            FundingBinding {
                offckb_index: index,
                lock_script: script,
                address: format!("wallet-{index}"),
            }
        })
        .collect()
}
fn config(root: &Path) -> crate::rpc::Connection {
    crate::rpc::Connection {
        root: root.to_path_buf(),
        rpc_url: "http://127.0.0.1:1".into(),
        registration_token: None,
    }
}
fn options(user: Option<u8>, account: Option<String>) -> crate::ClientOptions {
    crate::ClientOptions {
        selector: crate::Selector { user, account },
        round: None,
        connection: crate::ConnectionOptions::default(),
    }
}
#[tokio::test]
async fn migration_is_offline_idempotent_and_never_rewrites_legacy_files() {
    let temp = tempfile::tempdir().unwrap();
    let old = path(temp.path(), "default");
    let dir = AccountDir::lock(&old).unwrap();
    atomic_write(&old.join("root-key"), b"legacy-key").unwrap();
    drop(dir);
    list(temp.path(), true).unwrap();
    assert!(!base(temp.path()).join("accounts.json").exists());
    init_users(&config(temp.path()), bindings()).await.unwrap();
    let before = fs::read(base(temp.path()).join("accounts.json")).unwrap();
    let registry = UsersRegistry::load(temp.path()).unwrap().unwrap();
    assert_eq!(registry.legacy_current.as_deref(), Some("default"));
    assert!(
        registry
            .accounts
            .iter()
            .find(|e| e.id == "default")
            .unwrap()
            .user
            .is_none()
    );
    let mut identities = Vec::new();
    for user in &registry.users {
        let dir = path(temp.path(), user.current_account.as_ref().unwrap());
        let value: serde_json::Value =
            serde_json::from_slice(&read_private(&dir.join("account.json")).unwrap()).unwrap();
        assert_eq!(value["user"], user.number);
        assert_eq!(value["registration"], "identity_saved");
        identities.push(value["tenant_id"].clone());
    }
    identities.sort_by_key(|v| v.to_string());
    identities.dedup();
    assert_eq!(identities.len(), 4);
    init_users(&config(temp.path()), bindings()).await.unwrap();
    assert_eq!(
        fs::read(base(temp.path()).join("accounts.json")).unwrap(),
        before
    );
    assert_eq!(fs::read(old.join("root-key")).unwrap(), b"legacy-key");
    let mut wrong = bindings();
    wrong[0].address = "another".into();
    assert!(init_users(&config(temp.path()), wrong).await.is_err());
}
#[tokio::test]
async fn partial_identity_is_not_replaced_and_retry_completes_saved_identity_marker() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = config(temp.path());
    init_users(&cfg, bindings()).await.unwrap();
    let mut registry = UsersRegistry::load(temp.path()).unwrap().unwrap();
    let id = registry.users[0].current_account.clone().unwrap();
    registry.users[0].initialized = false;
    registry.save(temp.path()).unwrap();
    let before = fs::read(path(temp.path(), &id).join("root-key")).unwrap();
    init_users(&cfg, bindings()).await.unwrap();
    assert_eq!(
        fs::read(path(temp.path(), &id).join("root-key")).unwrap(),
        before
    );
    fs::remove_file(path(temp.path(), &id).join("account.json")).unwrap();
    assert!(init_users(&cfg, bindings()).await.is_err());
    assert_eq!(
        fs::read(path(temp.path(), &id).join("root-key")).unwrap(),
        before
    );
}
#[tokio::test]
async fn user_and_account_locks_block_competitors_and_queries_do_not_save() {
    let temp = tempfile::tempdir().unwrap();
    init_users(&config(temp.path()), bindings()).await.unwrap();
    let before = fs::read(base(temp.path()).join("accounts.json")).unwrap();
    let (dir, selected) = choose(temp.path(), &options(Some(1), None), true, false, true).unwrap();
    assert!(choose(temp.path(), &options(Some(1), None), true, false, true).is_err());
    assert!(choose(temp.path(), &options(Some(1), None), false, false, false).is_err());
    assert!(
        choose(
            temp.path(),
            &options(None, Some(selected.id.clone())),
            false,
            false,
            false
        )
        .is_err()
    );
    let other = choose(temp.path(), &options(Some(2), None), true, false, true).unwrap();
    assert_ne!(dir.path, other.0.path);
    assert!(check_historical_close(temp.path(), &other.1).is_ok());
    drop(other);
    list(temp.path(), true).unwrap();
    assert_eq!(
        fs::read(base(temp.path()).join("accounts.json")).unwrap(),
        before
    );
    drop(dir);
    drop(selected);
    let history = choose(
        temp.path(),
        &options(
            None,
            UsersRegistry::load(temp.path()).unwrap().unwrap().users[0]
                .current_account
                .clone(),
        ),
        false,
        false,
        false,
    )
    .unwrap();
    assert!(check_historical_close(temp.path(), &history.1).is_err());
}
#[tokio::test]
async fn archive_requires_exact_closed_evidence_and_next_round_preserves_owner() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = config(temp.path());
    init_users(&cfg, bindings()).await.unwrap();
    let (dir, selected) =
        choose(temp.path(), &options(Some(1), None), false, false, false).unwrap();
    let id = selected.id.clone();
    let channel = fiber_json_types::Hash256([3; 32]);
    let hash = ckb_types::H256::from([4; 32]);
    atomic_write(&dir.path.join("channel.json"), &serde_json::to_vec(&serde_json::json!({"stage":"closing","close_hash":hash,"opening":{"channel_id":channel}})).unwrap()).unwrap();
    drop(dir);
    assert!(archive_verified(temp.path(), &selected, channel, &hash).is_err());
    assert_eq!(
        UsersRegistry::load(temp.path()).unwrap().unwrap().users[0]
            .current_account
            .as_deref(),
        Some(id.as_str())
    );
    atomic_write(&path(temp.path(),&id).join("channel.json"), &serde_json::to_vec(&serde_json::json!({"stage":"closed","close_hash":hash,"opening":{"channel_id":channel}})).unwrap()).unwrap();
    archive_verified(temp.path(), &selected, channel, &hash).unwrap();
    archive_verified(temp.path(), &selected, channel, &hash).unwrap();
    drop(selected);
    let before = fs::read(base(temp.path()).join("accounts.json")).unwrap();
    assert!(choose(temp.path(), &options(Some(1), None), false, false, false).is_err());
    assert_eq!(
        fs::read(base(temp.path()).join("accounts.json")).unwrap(),
        before
    );
    init_users(&cfg, bindings()).await.unwrap();
    let (dir, next) = choose(temp.path(), &options(Some(1), None), true, false, true).unwrap();
    assert_ne!(next.id, id);
    crate::account::init_local(&dir, &cfg).await.unwrap();
    let registry = UsersRegistry::load(temp.path()).unwrap().unwrap();
    assert!(
        registry
            .accounts
            .iter()
            .filter(|e| e.id == id || e.id == next.id)
            .all(|e| e.user == Some(1))
    );
    assert_eq!(next.funding.as_ref().unwrap().offckb_index, 6);
    let next_id = next.id.clone();
    drop(dir);
    drop(next);
    let before = fs::read(base(temp.path()).join("accounts.json")).unwrap();
    let mut historical = options(Some(1), None);
    historical.round = Some(1);
    let (dir, old) = choose(temp.path(), &historical, false, false, false).unwrap();
    assert_eq!(old.id, id);
    assert!(check_historical_close(temp.path(), &old).is_ok());
    assert!(choose(temp.path(), &options(Some(1), None), false, false, false).is_err());
    assert!(choose(temp.path(), &historical, true, false, true).is_err());
    drop(dir);
    archive_verified(temp.path(), &old, channel, &hash).unwrap();
    assert_eq!(
        fs::read(base(temp.path()).join("accounts.json")).unwrap(),
        before
    );
    drop(old);
    historical.round = Some(2);
    assert!(choose(temp.path(), &historical, false, false, false).is_err());
    historical.round = Some(3);
    assert!(choose(temp.path(), &historical, false, false, false).is_err());
    historical.round = Some(0);
    assert!(choose(temp.path(), &historical, false, false, false).is_err());
    let (_, current) = choose(temp.path(), &options(Some(1), None), false, false, false).unwrap();
    assert_eq!(current.id, next_id);
    let mut other = options(Some(2), None);
    other.round = Some(1);
    assert!(choose(temp.path(), &other, false, false, false).is_err());
}
#[tokio::test]
async fn complete_account_directory_loss_or_owner_transfer_is_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = config(temp.path());
    init_users(&cfg, bindings()).await.unwrap();
    let mut registry = UsersRegistry::load(temp.path()).unwrap().unwrap();
    let first = registry.users[0].current_account.clone();
    let second = registry.users[1].current_account.clone();
    registry.users[0].current_account = second.clone();
    registry.users[1].current_account = first.clone();
    for entry in &mut registry.accounts {
        if Some(&entry.id) == first.as_ref() {
            entry.user = Some(2);
        }
        if Some(&entry.id) == second.as_ref() {
            entry.user = Some(1);
        }
    }
    registry.save(temp.path()).unwrap();
    assert!(init_users(&cfg, bindings()).await.is_err());
    let id = first.unwrap();
    fs::remove_dir_all(path(temp.path(), &id)).unwrap();
    assert!(choose(temp.path(), &options(Some(2), None), true, false, true).is_err());
    assert!(!path(temp.path(), &id).exists());
}
#[tokio::test]
async fn concurrent_users_wait_for_registry_and_preserve_all_reservations_and_archives() {
    use std::sync::{Arc, Barrier, mpsc};
    use std::time::Duration;

    let temp = tempfile::tempdir().unwrap();
    init_users(&config(temp.path()), bindings()).await.unwrap();
    let mut registry = UsersRegistry::load(temp.path()).unwrap().unwrap();
    for user in &mut registry.users {
        user.current_account = None;
    }
    for entry in &mut registry.accounts {
        entry.archived = true;
    }
    registry.save(temp.path()).unwrap();
    let before = fs::read(base(temp.path()).join("accounts.json")).unwrap();
    let registry_lock = AccountDir::lock_wait(&base(temp.path()).join(".registry")).unwrap();
    let start = Arc::new(Barrier::new(5));
    let (done_tx, done_rx) = mpsc::channel();
    let root = temp.path();
    let (early_result, while_waiting, selections) = std::thread::scope(|scope| {
        let workers: Vec<_> = (1..=4)
            .map(|number| {
                let start = Arc::clone(&start);
                let done = done_tx.clone();
                scope.spawn(move || {
                    start.wait();
                    let result = choose(root, &options(Some(number), None), true, false, true);
                    done.send(()).unwrap();
                    result
                })
            })
            .collect();
        start.wait();
        let early = done_rx.recv_timeout(Duration::from_millis(100));
        let while_waiting = fs::read(base(root).join("accounts.json"));
        drop(registry_lock);
        let selections: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap().unwrap())
            .collect();
        (early, while_waiting, selections)
    });
    assert_eq!(early_result, Err(mpsc::RecvTimeoutError::Timeout));
    assert_eq!(while_waiting.unwrap(), before);
    let registry = UsersRegistry::load(root).unwrap().unwrap();
    assert_eq!(registry.accounts.len(), 8);
    for (dir, selection) in &selections {
        let number = selection.user.unwrap();
        let user = registry.users.iter().find(|u| u.number == number).unwrap();
        assert_eq!(user.current_account.as_ref(), Some(&selection.id));
        assert_eq!(dir.path, path(root, &selection.id));
        assert_eq!(selection.funding.as_ref(), Some(&user.funding));
    }

    let start = Arc::new(Barrier::new(4));
    let channel = fiber_json_types::Hash256([3; 32]);
    let hash = ckb_types::H256::from([4; 32]);
    let archived: Vec<_> = std::thread::scope(|scope| {
        let workers: Vec<_> = selections
            .into_iter()
            .map(|(dir, selection)| {
                let start = Arc::clone(&start);
                let hash = hash.clone();
                scope.spawn(move || -> Result<_> {
                    start.wait();
                    atomic_write(
                        &dir.path.join("channel.json"),
                        &serde_json::to_vec(&serde_json::json!({
                            "stage": "closed", "close_hash": hash,
                            "opening": { "channel_id": channel }
                        }))?,
                    )?;
                    drop(dir);
                    archive_verified(root, &selection, channel, &hash)?;
                    Ok((selection.user.unwrap(), selection.id.clone()))
                })
            })
            .collect();
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap().unwrap())
            .collect()
    });
    let registry = UsersRegistry::load(root).unwrap().unwrap();
    assert_eq!(registry.accounts.len(), 8);
    assert!(registry.accounts.iter().all(|entry| entry.archived));
    for (number, id) in archived {
        let user = registry.users.iter().find(|u| u.number == number).unwrap();
        assert!(user.current_account.is_none());
        assert_eq!(user.last_closed_account.as_ref(), Some(&id));
    }
}
#[tokio::test]
async fn failed_archive_save_keeps_pointer_and_retry_archives_exactly_once() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    init_users(&config(temp.path()), bindings()).await.unwrap();
    let (dir, selection) =
        choose(temp.path(), &options(Some(1), None), false, false, false).unwrap();
    let channel = fiber_json_types::Hash256([3; 32]);
    let hash = ckb_types::H256::from([4; 32]);
    atomic_write(&dir.path.join("channel.json"),&serde_json::to_vec(&serde_json::json!({"stage":"closed","close_hash":hash,"opening":{"channel_id":channel}})).unwrap()).unwrap();
    drop(dir);
    let file = base(temp.path()).join("accounts.json");
    let before = fs::read(&file).unwrap();
    assert!(
        archive_with_write(temp.path(), &selection, channel, &hash, |path, bytes| {
            fs::set_permissions(path, fs::Permissions::from_mode(0o644))?;
            atomic_write(path, bytes)
        })
        .is_err()
    );
    assert_eq!(fs::read(&file).unwrap(), before);
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        UsersRegistry::load(temp.path()).unwrap().unwrap().users[0]
            .current_account
            .as_deref(),
        Some(selection.id.as_str())
    );
    archive_verified(temp.path(), &selection, channel, &hash).unwrap();
    let saved = fs::read(&file).unwrap();
    archive_verified(temp.path(), &selection, channel, &hash).unwrap();
    assert_eq!(fs::read(&file).unwrap(), saved);
}
