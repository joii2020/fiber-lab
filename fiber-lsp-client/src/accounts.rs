//! Local account selection. Archiving changes metadata, never signer paths.
use crate::store::{AccountDir, atomic_write, read_private};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Registry {
    version: u32,
    current: Option<String>,
    accounts: Vec<Entry>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    id: String,
    archived: bool,
}

pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn base(root: &Path) -> PathBuf {
    root.join(".state/fiber-lsp-client")
}
fn path(root: &Path, id: &str) -> PathBuf {
    if id == "default" {
        base(root).join(id)
    } else {
        base(root).join("accounts").join(id)
    }
}

impl Registry {
    fn load(root: &Path) -> Result<Self> {
        let file = base(root).join("accounts.json");
        if !file.try_exists()? {
            let legacy = base(root).join("default");
            let exists = legacy.try_exists()?
                && fs::read_dir(&legacy)?
                    .any(|e| e.map(|e| e.file_name() != ".lock").unwrap_or(true));
            return Ok(Self {
                version: 1,
                current: exists.then(|| "default".into()),
                accounts: if exists {
                    vec![Entry {
                        id: "default".into(),
                        archived: false,
                    }]
                } else {
                    vec![]
                },
            });
        }
        let registry: Self =
            serde_json::from_slice(&read_private(&file)?).context("账户清单损坏；原数据已保留")?;
        ensure!(registry.version == 1, "账户清单版本不支持；原数据已保留");
        for (i, entry) in registry.accounts.iter().enumerate() {
            ensure!(
                entry.id == "default"
                    || entry
                        .id
                        .strip_prefix("account-")
                        .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit())),
                "账户 ID 无效；原数据已保留"
            );
            ensure!(
                !registry.accounts[..i].iter().any(|e| e.id == entry.id),
                "账户 ID 重复；原数据已保留"
            );
            ensure!(
                entry.archived == (registry.current.as_deref() != Some(&entry.id)),
                "账户默认 / 归档标记不一致；原数据已保留"
            );
        }
        ensure!(
            registry
                .current
                .as_ref()
                .is_none_or(|id| registry.accounts.iter().any(|e| &e.id == id)),
            "默认账户不在清单中；原数据已保留"
        );
        Ok(registry)
    }
    fn save(&self, root: &Path) -> Result<()> {
        atomic_write(
            &base(root).join("accounts.json"),
            &serde_json::to_vec_pretty(self)?,
        )
    }
}

/// Lock order is registry -> account. The registry lock is released before RPC.
pub fn select(root: &Path, requested: Option<&str>, create: bool) -> Result<AccountDir> {
    let _registry_lock = AccountDir::lock(&base(root).join(".registry"))?;
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

pub fn archive(root: &Path, requested: Option<&str>) -> Result<String> {
    let _registry_lock = AccountDir::lock(&base(root).join(".registry"))?;
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

pub fn list(root: &Path) -> Result<()> {
    let registry = Registry::load(root)?;
    println!("账户 ID\t状态\ttenant_id\t本地阶段（不代表远端状态）");
    for entry in registry.accounts {
        let dir = path(root, &entry.id);
        let summary = (|| -> Result<(String, String)> {
            let account: serde_json::Value =
                serde_json::from_slice(&read_private(&dir.join("account.json"))?)?;
            let mut stage = account["registration"]
                .as_str()
                .unwrap_or("未知")
                .to_owned();
            if dir.join("channel.json").try_exists()? {
                let channel: serde_json::Value =
                    serde_json::from_slice(&read_private(&dir.join("channel.json"))?)?;
                stage = channel["stage"].as_str().unwrap_or("未知").to_owned();
            }
            Ok((
                account["tenant_id"]
                    .as_str()
                    .unwrap_or("尚未保存")
                    .to_owned(),
                stage,
            ))
        })();
        let (tenant, stage) =
            summary.unwrap_or_else(|_| ("未知".into(), "记录缺失或损坏，待恢复".into()));
        println!(
            "{}\t{}\t{tenant}\t{stage}",
            entry.id,
            if entry.archived {
                "已归档"
            } else {
                "当前"
            }
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_archive_and_new_default_preserve_paths_and_isolate_accounts() {
        let temp = tempfile::tempdir().unwrap();
        let legacy = path(temp.path(), "default");
        let dir = AccountDir::lock(&legacy).unwrap();
        atomic_write(&legacy.join("root-key"), b"original-key").unwrap();
        drop(dir);
        list(temp.path()).unwrap();
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
        assert!(list(temp.path()).is_err());
        assert!(archive(temp.path(), None).is_err());
        assert!(select(temp.path(), None, true).is_err());
        assert_eq!(fs::read(file).unwrap(), invalid);
    }
}
