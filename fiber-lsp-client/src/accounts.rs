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
    #[cfg(test)]
    fn save(&self, root: &Path) -> Result<()> {
        atomic_write(
            &base(root).join("accounts.json"),
            &serde_json::to_vec_pretty(self)?,
        )
    }
}

/// Lock order is registry -> account. The registry lock is released before RPC.
#[cfg(test)]
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

#[cfg(test)]
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

fn list_legacy(root: &Path) -> Result<()> {
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
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FundingBinding {
    pub offckb_index: usize,
    pub lock_script: ckb_jsonrpc_types::Script,
    pub address: String,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct UserEntry {
    number: u8,
    funding: FundingBinding,
    current_account: Option<String>,
    last_closed_account: Option<String>,
    initialized: bool,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct OwnedEntry {
    id: String,
    user: Option<u8>,
    archived: bool,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct UsersRegistry {
    version: u32,
    legacy_current: Option<String>,
    users: Vec<UserEntry>,
    accounts: Vec<OwnedEntry>,
}
impl UsersRegistry {
    fn load(root: &Path) -> Result<Option<Self>> {
        let file = base(root).join("accounts.json");
        if !file.try_exists()? {
            Registry::load(root)?;
            return Ok(None);
        }
        let bytes = read_private(&file)?;
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).context("账户清单损坏；原数据已保留")?;
        match value["version"].as_u64() {
            Some(1) => {
                Registry::load(root)?;
                Ok(None)
            }
            Some(2) => {
                let registry: Self =
                    serde_json::from_slice(&bytes).context("v2 账户清单损坏；原数据已保留")?;
                registry.validate(root)?;
                Ok(Some(registry))
            }
            _ => anyhow::bail!("账户清单版本不支持；原数据已保留"),
        }
    }
    fn validate(&self, _root: &Path) -> Result<()> {
        ensure!(
            self.version == 2 && self.users.len() == 4,
            "固定用户清单损坏"
        );
        for (i, entry) in self.accounts.iter().enumerate() {
            ensure!(
                entry.id == "default"
                    || entry
                        .id
                        .strip_prefix("account-")
                        .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit())),
                "账户 ID 无效"
            );
            ensure!(
                !self.accounts[..i].iter().any(|e| e.id == entry.id),
                "账户 ID 重复"
            );
            ensure!(
                entry.user.is_none_or(|u| (1..=4).contains(&u)),
                "账户用户归属无效"
            );
        }
        for number in 1..=4 {
            let matches: Vec<_> = self.users.iter().filter(|u| u.number == number).collect();
            ensure!(matches.len() == 1, "固定用户编号缺失或重复");
            let user = matches[0];
            ensure!(
                user.funding.offckb_index == usize::from(number) + 5
                    && !user.funding.address.is_empty(),
                "固定用户钱包绑定损坏"
            );
            crate::wallet::validate_default_lock(&user.funding.lock_script)?;
            ensure!(
                !self.users.iter().any(|other| other.number != number
                    && other.funding.lock_script == user.funding.lock_script),
                "用户钱包重复"
            );
            for (id, closed) in [
                (&user.current_account, false),
                (&user.last_closed_account, true),
            ] {
                if let Some(id) = id {
                    let entry = self
                        .accounts
                        .iter()
                        .find(|e| &e.id == id)
                        .context("用户指针不在清单中")?;
                    ensure!(
                        entry.user == Some(number) && entry.archived == closed,
                        "用户指针归属或归档标记不一致"
                    );
                }
            }
            ensure!(
                user.initialized || user.current_account.is_some(),
                "未完成初始化缺少预留身份"
            );
        }
        for entry in &self.accounts {
            if let Some(number) = entry.user {
                ensure!(
                    entry.archived
                        || self.users.iter().any(|u| u.number == number
                            && u.current_account.as_deref() == Some(&entry.id)),
                    "未归档身份不是所属用户当前轮次"
                );
            }
        }
        ensure!(
            self.legacy_current.as_ref().is_none_or(|id| self
                .accounts
                .iter()
                .any(|e| &e.id == id && e.user.is_none())),
            "旧默认账户指针无效"
        );
        Ok(())
    }
    fn save(&self, root: &Path) -> Result<()> {
        self.validate(root)?;
        atomic_write(
            &base(root).join("accounts.json"),
            &serde_json::to_vec_pretty(self)?,
        )
    }
}
fn next_id(root: &Path) -> Result<String> {
    let id = format!(
        "account-{:x}",
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    );
    ensure!(!path(root, &id).try_exists()?, "新身份目录已存在；请重试");
    Ok(id)
}
/// User lock remains held until the caller completes RPC and archival.
pub struct Selection {
    pub id: String,
    pub user: Option<u8>,
    pub funding: Option<FundingBinding>,
    _user_lock: Option<AccountDir>,
    _opening_lock: Option<AccountDir>,
}
pub fn choose(
    root: &Path,
    options: &crate::ClientOptions,
    create: bool,
    last_closed: bool,
    current_only: bool,
) -> Result<(AccountDir, Selection)> {
    ensure!(
        options.user.is_some() != options.account.is_some(),
        "必须选择 -u N 或 --account ID，且两者互斥"
    );
    ensure!(
        !current_only || options.user.is_some(),
        "本命令只允许 -u N 当前账户"
    );
    let user_lock = options
        .user
        .map(|u| AccountDir::lock(&base(root).join(format!(".user-{u}"))))
        .transpose()?;
    let opening_lock = create
        .then(|| AccountDir::lock(&base(root).join(".opening")))
        .transpose()?;
    let _registry_lock = AccountDir::lock(&base(root).join(".registry"))?;
    let loaded = UsersRegistry::load(root)?;
    let mut newly_reserved = false;
    let mut reserved_dir = None;
    let (id, owner, funding) = if let Some(number) = options.user {
        let mut registry = loaded.context("用户尚未初始化；执行 ./lsp-cli admin init-accounts")?;
        let user = registry
            .users
            .iter_mut()
            .find(|u| u.number == number)
            .context("用户编号必须为 1～4")?;
        ensure!(
            user.initialized,
            "用户初始化未完成；重试 admin init-accounts"
        );
        if create && user.current_account.is_none() {
            let id = next_id(root)?;
            newly_reserved = true;
            reserved_dir = Some(AccountDir::lock(&path(root, &id))?);
            user.current_account = Some(id.clone());
            registry.accounts.push(OwnedEntry {
                id,
                user: Some(number),
                archived: false,
            });
            registry.save(root)?;
        }
        let user = registry.users.iter().find(|u| u.number == number).unwrap();
        let id = user
            .current_account
            .as_ref()
            .or_else(|| {
                last_closed
                    .then_some(user.last_closed_account.as_ref())
                    .flatten()
            })
            .context("用户空闲，无当前账户；历史恢复请用 accounts --history 和 --account ID")?
            .clone();
        (id, Some(number), Some(user.funding.clone()))
    } else {
        let id = options.account.as_ref().unwrap().clone();
        if let Some(registry) = loaded {
            let entry = registry
                .accounts
                .iter()
                .find(|e| e.id == id)
                .context("未知历史账户")?;
            let funding = entry
                .user
                .and_then(|n| registry.users.iter().find(|u| u.number == n))
                .map(|u| u.funding.clone());
            (id, entry.user, funding)
        } else {
            let legacy = Registry::load(root)?;
            ensure!(legacy.accounts.iter().any(|e| e.id == id), "未知历史账户");
            (id, None, None)
        }
    };
    let account_path = path(root, &id);
    ensure!(
        account_path.try_exists()? || newly_reserved,
        "账户 {id} 目录缺失；原数据已保留"
    );
    let dir = match reserved_dir {
        Some(dir) => dir,
        None => AccountDir::lock(&account_path)?,
    };
    println!(
        "用户：{}\n账户 ID：{id}",
        owner.map_or_else(|| "旧账户未绑定".into(), |u| u.to_string())
    );
    if let Some(binding) = &funding {
        println!("出资钱包：offckb #{}", binding.offckb_index);
    }
    Ok((
        dir,
        Selection {
            id,
            user: owner,
            funding,
            _user_lock: user_lock,
            _opening_lock: opening_lock,
        },
    ))
}

pub fn check_historical_close(root: &Path, selection: &Selection) -> Result<()> {
    if selection._user_lock.is_none()
        && let Some(registry) = UsersRegistry::load(root)?
    {
        ensure!(
            !registry
                .users
                .iter()
                .any(|u| u.current_account.as_deref() == Some(&selection.id)),
            "该账户仍是用户当前轮次；请通过 -u N 关闭并归档"
        );
    }
    Ok(())
}
/// Caller drops the account lock first, retaining its user lock. Reacquire registry -> account.
pub fn archive_verified(
    root: &Path,
    selection: &Selection,
    channel: fiber_json_types::Hash256,
    hash: &ckb_types::H256,
) -> Result<()> {
    archive_with_write(root, selection, channel, hash, atomic_write)
}
fn archive_with_write(
    root: &Path,
    selection: &Selection,
    channel: fiber_json_types::Hash256,
    hash: &ckb_types::H256,
    write: impl FnOnce(&Path, &[u8]) -> Result<()>,
) -> Result<()> {
    let _registry_lock = AccountDir::lock(&base(root).join(".registry"))?;
    let _account_lock = AccountDir::lock(&path(root, &selection.id))?;
    let value: serde_json::Value = serde_json::from_slice(&read_private(
        &path(root, &selection.id).join("channel.json"),
    )?)?;
    ensure!(
        value["stage"] == "closed"
            && value["close_hash"] == serde_json::to_value(hash)?
            && value["opening"]["channel_id"] == serde_json::to_value(channel)?,
        "关闭归档证据不一致"
    );
    if let Some(mut registry) = UsersRegistry::load(root)? {
        let entry = registry
            .accounts
            .iter_mut()
            .find(|e| e.id == selection.id)
            .context("归档账户不存在")?;
        ensure!(entry.user == selection.user, "账户永久归属发生变化");
        entry.archived = true;
        if selection._user_lock.is_some() {
            let user = registry
                .users
                .iter_mut()
                .find(|u| Some(u.number) == selection.user)
                .context("归档用户不存在")?;
            ensure!(
                user.current_account.as_deref() == Some(&selection.id)
                    || (user.current_account.is_none()
                        && user.last_closed_account.as_deref() == Some(&selection.id)),
                "用户当前轮次发生变化"
            );
            user.current_account = None;
            user.last_closed_account = Some(selection.id.clone());
        } else {
            ensure!(
                !registry
                    .users
                    .iter()
                    .any(|u| u.current_account.as_deref() == Some(&selection.id)),
                "历史入口不能归档当前轮次"
            );
        }
        registry.validate(root)?;
        write(
            &base(root).join("accounts.json"),
            &serde_json::to_vec_pretty(&registry)?,
        )?;
    } else {
        let mut registry = Registry::load(root)?;
        registry
            .accounts
            .iter_mut()
            .find(|e| e.id == selection.id)
            .context("归档账户不存在")?
            .archived = true;
        if registry.current.as_deref() == Some(&selection.id) {
            registry.current = None;
        }
        write(
            &base(root).join("accounts.json"),
            &serde_json::to_vec_pretty(&registry)?,
        )?;
    }
    println!("账户 {} 已归档；身份、租户归属及历史保留", selection.id);
    Ok(())
}

pub async fn init_users(config: &crate::rpc::Config, bindings: Vec<FundingBinding>) -> Result<()> {
    ensure!(
        bindings.len() == 4
            && bindings
                .iter()
                .enumerate()
                .all(|(i, b)| b.offckb_index == i + 6),
        "初始化钱包必须为 #6～#9"
    );
    let _user_locks = (1..=4)
        .map(|u| AccountDir::lock(&base(&config.root).join(format!(".user-{u}"))))
        .collect::<Result<Vec<_>>>()?;
    let _registry_lock = AccountDir::lock(&base(&config.root).join(".registry"))?;
    let mut registry = if let Some(registry) = UsersRegistry::load(&config.root)? {
        ensure!(
            registry
                .users
                .iter()
                .all(|u| bindings[usize::from(u.number) - 1] == u.funding),
            "固定钱包绑定与 offckb 不一致；拒绝更换"
        );
        registry
    } else {
        let legacy = Registry::load(&config.root)?;
        let users = bindings
            .into_iter()
            .enumerate()
            .map(|(i, funding)| {
                Ok(UserEntry {
                    number: (i + 1) as u8,
                    funding,
                    current_account: Some(next_id(&config.root)?),
                    last_closed_account: None,
                    initialized: false,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let mut accounts: Vec<_> = legacy
            .accounts
            .into_iter()
            .map(|e| OwnedEntry {
                id: e.id,
                user: None,
                archived: e.archived,
            })
            .collect();
        accounts.extend(users.iter().map(|u| OwnedEntry {
            id: u.current_account.clone().unwrap(),
            user: Some(u.number),
            archived: false,
        }));
        let registry = UsersRegistry {
            version: 2,
            legacy_current: legacy.current,
            users,
            accounts,
        };
        registry.save(&config.root)?;
        registry
    };
    for i in 0..registry.users.len() {
        if registry.users[i].initialized {
            if let Some(id) = &registry.users[i].current_account {
                ensure!(
                    path(&config.root, id).try_exists()?,
                    "已初始化账户目录缺失；原清单已保留"
                );
                let dir = AccountDir::lock(&path(&config.root, id))?;
                ensure!(dir.is_initialized()?, "已初始化身份缺失；拒绝重新生成");
                crate::account::init_local(&dir, config).await?;
            }
            println!("用户 {}：复用（含空闲和历史）", registry.users[i].number);
            continue;
        }
        let id = registry.users[i].current_account.as_ref().unwrap();
        let dir = AccountDir::lock(&path(&config.root, id))?;
        crate::account::init_local(&dir, config).await?;
        registry.users[i].initialized = true;
        registry.save(&config.root)?;
        println!("用户 {}：身份初始化完成", registry.users[i].number);
    }
    Ok(())
}

pub fn list(root: &Path, history: bool) -> Result<()> {
    let Some(registry) = UsersRegistry::load(root)? else {
        println!("固定用户尚未初始化；执行 ./lsp-cli admin init-accounts");
        if history {
            list_legacy(root)?;
        }
        return Ok(());
    };
    println!("用户\toffckb\t当前账户\ttenant_id\t本地阶段\t历史数");
    for user in &registry.users {
        let (tenant, stage) = user
            .current_account
            .as_ref()
            .map(|id| summary(&path(root, id)))
            .unwrap_or_else(|| ("无当前租户".into(), "空闲（最近一轮已关闭）".into()));
        println!(
            "{}\t#{}\t{}\t{tenant}\t{stage}\t{}",
            user.number,
            user.funding.offckb_index,
            user.current_account.as_deref().unwrap_or("无"),
            registry
                .accounts
                .iter()
                .filter(|e| e.user == Some(user.number) && e.archived)
                .count()
        );
    }
    if history {
        println!("账户 ID\t用户\t归档\ttenant_id\t本地阶段");
        for entry in &registry.accounts {
            let (tenant, stage) = summary(&path(root, &entry.id));
            println!(
                "{}\t{}\t{}\t{tenant}\t{stage}",
                entry.id,
                entry
                    .user
                    .map_or_else(|| "旧账户未绑定".into(), |u| u.to_string()),
                entry.archived
            );
        }
    }
    Ok(())
}
fn summary(dir: &Path) -> (String, String) {
    let read = || -> Result<(String, String)> {
        let value: serde_json::Value =
            serde_json::from_slice(&read_private(&dir.join("account.json"))?)?;
        let stage = if dir.join("channel.json").try_exists()? {
            let channel: serde_json::Value =
                serde_json::from_slice(&read_private(&dir.join("channel.json"))?)?;
            channel["stage"].as_str().unwrap_or("未知").to_owned()
        } else {
            value["registration"].as_str().unwrap_or("未知").to_owned()
        };
        Ok((
            value["tenant_id"].as_str().unwrap_or("未知").to_owned(),
            stage,
        ))
    };
    read().unwrap_or_else(|_| ("未知".into(), "记录缺失或损坏，待恢复".into()))
}

pub fn owner_for(root: &Path, id: &str) -> Result<Option<u8>> {
    Ok(UsersRegistry::load(root)?.and_then(|r| {
        r.accounts
            .into_iter()
            .find(|e| e.id == id)
            .and_then(|e| e.user)
    }))
}

pub fn funding_for(root: &Path, number: u8) -> Result<FundingBinding> {
    let registry =
        UsersRegistry::load(root)?.context("用户尚未初始化；执行 admin init-accounts")?;
    let user = registry
        .users
        .iter()
        .find(|u| u.number == number)
        .context("用户编号无效")?;
    ensure!(user.initialized, "初始化未完成；重试 admin init-accounts");
    Ok(user.funding.clone())
}

#[cfg(test)]
mod user_tests {
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
    fn config(root: &Path) -> crate::rpc::Config {
        crate::rpc::Config {
            root: root.to_path_buf(),
            rpc_url: "http://127.0.0.1:1".into(),
            rpc_auth_public_key: "test-key".into(),
        }
    }
    fn options(user: Option<u8>, account: Option<String>) -> crate::ClientOptions {
        crate::ClientOptions {
            selector: crate::Selector { user, account },
            config: None,
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
        let (dir, selected) =
            choose(temp.path(), &options(Some(1), None), false, false, false).unwrap();
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
        let other = choose(temp.path(), &options(Some(2), None), false, false, false).unwrap();
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
    async fn opening_lock_rejects_other_user_without_allocating_identity() {
        let temp = tempfile::tempdir().unwrap();
        init_users(&config(temp.path()), bindings()).await.unwrap();
        let mut registry = UsersRegistry::load(temp.path()).unwrap().unwrap();
        let id = registry.users[1].current_account.take().unwrap();
        registry
            .accounts
            .iter_mut()
            .find(|e| e.id == id)
            .unwrap()
            .archived = true;
        registry.save(temp.path()).unwrap();
        let before = fs::read(base(temp.path()).join("accounts.json")).unwrap();
        let _opening = AccountDir::lock(&base(temp.path()).join(".opening")).unwrap();
        assert!(choose(temp.path(), &options(Some(2), None), true, false, true).is_err());
        assert_eq!(
            fs::read(base(temp.path()).join("accounts.json")).unwrap(),
            before
        );
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
}
