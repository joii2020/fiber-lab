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
    println!("账户 ID\t状态\ttenant_id\t当前状态");
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
            if stage == "identity_saved" {
                stage = "unregistered".to_owned();
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
#[path = "test_accounts.rs"]
mod tests;

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
    historical: bool,
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
        !current_only || (options.user.is_some() && options.round.is_none()),
        "本命令只允许 -u N 当前账户"
    );
    ensure!(
        options.round.is_none() || (options.user.is_some() && !create),
        "历史轮次必须使用 -u N，且不能用于新开通"
    );
    ensure!(options.round != Some(0), "轮次必须从 1 开始");
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
        let id = if let Some(round) = options.round {
            let entry = registry
                .accounts
                .iter()
                .filter(|e| e.user == Some(number))
                .nth((round - 1) as usize)
                .context("未知历史轮次；请执行 accounts --history")?;
            ensure!(
                entry.archived,
                "该轮次仍在使用；请直接使用 -u N 操作当前轮次"
            );
            entry.id.clone()
        } else {
            user.current_account
                .as_ref()
                .or_else(|| {
                    last_closed
                        .then_some(user.last_closed_account.as_ref())
                        .flatten()
                })
                .context("账户空闲，无当前轮次；历史恢复请用 accounts --history 和 -u N --round R")?
                .clone()
        };
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
    if let Some(number) = owner {
        println!("账户：{number}");
        if let Some(round) = options.round {
            println!("历史轮次：{round}");
        }
    } else {
        println!("旧身份未绑定：{id}");
    }
    if let Some(binding) = &funding {
        println!("出资钱包：offckb #{}", binding.offckb_index);
    }
    Ok((
        dir,
        Selection {
            id,
            user: owner,
            funding,
            historical: options.account.is_some() || options.round.is_some(),
            _user_lock: user_lock,
            _opening_lock: opening_lock,
        },
    ))
}

pub fn check_historical_close(root: &Path, selection: &Selection) -> Result<()> {
    if selection.historical
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
        if !selection.historical {
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
    println!("本轮已归档；固定账户、身份、租户归属及历史保留");
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
    println!("账户\toffckb\ttenant_id\t当前状态\t历史数");
    for user in &registry.users {
        let (tenant, stage) = user
            .current_account
            .as_ref()
            .map(|id| summary(&path(root, id)))
            .unwrap_or_else(|| ("无当前租户".into(), "空闲（最近一轮已关闭）".into()));
        println!(
            "{}\t#{}\t{tenant}\t{stage}\t{}",
            user.number,
            user.funding.offckb_index,
            registry
                .accounts
                .iter()
                .filter(|e| e.user == Some(user.number) && e.archived)
                .count()
        );
    }
    if history {
        println!("账户\t轮次\t状态\ttenant_id\t当前状态");
        let mut rounds = [0; 4];
        for entry in &registry.accounts {
            let (tenant, stage) = summary(&path(root, &entry.id));
            let round = if let Some(number) = entry.user {
                let count = &mut rounds[usize::from(number) - 1];
                *count += 1;
                count.to_string()
            } else {
                entry.id.clone()
            };
            println!(
                "{}\t{}\t{}\t{tenant}\t{stage}",
                entry
                    .user
                    .map_or_else(|| "旧身份未绑定".into(), |u| u.to_string()),
                round,
                if entry.archived { "历史" } else { "当前" }
            );
        }
    }
    Ok(())
}
fn summary(dir: &Path) -> (String, String) {
    let read = || -> Result<(String, String)> {
        let value: serde_json::Value =
            serde_json::from_slice(&read_private(&dir.join("account.json"))?)?;
        let mut stage = if dir.join("channel.json").try_exists()? {
            let channel: serde_json::Value =
                serde_json::from_slice(&read_private(&dir.join("channel.json"))?)?;
            channel["stage"].as_str().unwrap_or("未知").to_owned()
        } else {
            value["registration"].as_str().unwrap_or("未知").to_owned()
        };
        if stage == "identity_saved" {
            stage = "unregistered".to_owned();
        }
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
#[path = "test_accounts_users.rs"]
mod user_tests;
