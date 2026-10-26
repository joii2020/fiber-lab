use anyhow::{Context, Result, bail};
use fiber_json_types::{LspTenantStatus, Pubkey};
use fiber_lsp_sdk::{HostedSession, HostedSessionState, RootKey, RootSigner};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::{
    rpc::{Connection, Rpc},
    store::{AccountDir, FileSignerStore, atomic_write, read_private},
};

#[derive(Clone, Copy, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Registration {
    IdentitySaved,
    AwaitingResult,
    Registered,
}

#[derive(Deserialize, Serialize)]
struct AccountRecord {
    #[serde(default)]
    user: Option<u8>,
    format_version: u32,
    rpc_url: String,
    lsp_public_key: Option<Pubkey>,
    identity_public_key: Pubkey,
    tenant_id: String,
    registration: Registration,
    tenant: Option<LspTenantStatus>,
    session: HostedSessionState,
}

pub struct AccountStatus {
    pub outcome: &'static str,
    pub identity: Pubkey,
    pub tenant_id: String,
    pub client: ClientAccount,
    pub channels: Vec<fiber_json_types::Channel>,
}

pub async fn run_dir(config: &Connection, create: bool, dir: AccountDir) -> Result<AccountStatus> {
    let id = dir.path.file_name().unwrap().to_string_lossy();
    let recovery = registration_recovery(config, &id)?;
    let (mut record, mut session) = if dir.is_initialized()? {
        restore(&dir, config).await?
    } else if create {
        initialize(&dir, config).await?
    } else {
        bail!("账户不存在；请先执行 ./lsp-cli create-channel -u N");
    };
    let rpc = Rpc::new(&config.rpc_url)?;
    let outcome = if record.registration == Registration::Registered {
        if create {
            "账户已存在，已恢复原账户"
        } else {
            "已恢复原账户"
        }
    } else {
        if !create {
            bail!("账户身份已保存，但注册尚未完成；{recovery} 核对注册进度。原数据已保留");
        }
        register(&dir, config, &rpc, &mut record, &mut session).await?;
        "用户注册成功，账户已保存"
    };
    let token = session
        .tenant_token()
        .context("账户缺少用户凭证；原数据已保留")?;
    let channels = if create {
        rpc.list_channels(token).await
    } else {
        rpc.list_all_channels(token).await
    }
    .with_context(|| {
        format!("查询通道失败；已保存的账户保持不变，可再次执行 ./lsp-cli status --account {id}")
    })?;
    Ok(AccountStatus {
        outcome,
        identity: record.identity_public_key,
        tenant_id: record.tenant_id,
        client: ClientAccount {
            dir,
            session,
            lsp_public_key: record.lsp_public_key.unwrap(),
            invoice_public_key: record
                .tenant
                .as_ref()
                .context("缺少租户发票公钥")?
                .invoice_pubkey,
        },
        channels: channels.channels,
    })
}

/// Holds the existing account lock for the complete lifecycle command.
pub struct ClientAccount {
    pub dir: AccountDir,
    pub session: HostedSession<FileSignerStore>,
    pub lsp_public_key: Pubkey,
    pub invoice_public_key: Pubkey,
}

impl ClientAccount {
    pub async fn load_dir(config: &Connection, dir: AccountDir) -> Result<Self> {
        let _timing = crate::timings::Span::new("cli.account_restore");
        let id = dir.path.file_name().unwrap().to_string_lossy();
        let recovery = registration_recovery(config, &id)?;
        let (record, session) = restore(&dir, config).await?;
        if record.registration != Registration::Registered {
            bail!("账户注册尚未完成；{recovery}；原数据已保留");
        }
        Ok(Self {
            dir,
            session,
            lsp_public_key: record.lsp_public_key.unwrap(),
            invoice_public_key: record
                .tenant
                .as_ref()
                .context("缺少租户发票公钥")?
                .invoice_pubkey,
        })
    }

    pub fn save(&self) -> Result<()> {
        save_session(&self.dir, self.session.state())
    }
}

fn save_session(dir: &AccountDir, state: &HostedSessionState) -> Result<()> {
    let mut record: AccountRecord = dir.read_json()?;
    record.session = state.clone();
    dir.save_json(&record)
}

#[async_trait::async_trait]
impl fiber_lsp_sdk::OpeningPersistence for AccountDir {
    async fn save_session(
        &self,
        state: &HostedSessionState,
    ) -> std::result::Result<(), fiber_lsp_sdk::SessionError> {
        save_session(self, state)
            .map_err(|e| fiber_lsp_sdk::SessionError::Invalid(format!("保存 session 失败：{e:#}")))
    }
}

async fn initialize(
    dir: &AccountDir,
    config: &Connection,
) -> Result<(AccountRecord, HostedSession<FileSignerStore>)> {
    let created = RootSigner::create_random(FileSignerStore::new(dir))
        .await
        .context("创建 SDK 身份失败；已写入的文件保留，不会重新生成身份")?;
    let backup = Zeroizing::new(created.root_key_backup.expose_secret());
    atomic_write(&dir.path.join("root-key"), backup.as_ref())
        .context("保存根密钥备份失败；尚未提交注册，原文件已保留")?;
    let session = HostedSession::new(created.root_signer);
    let record = AccountRecord {
        user: crate::accounts::owner_for(
            &config.root,
            &dir.path.file_name().unwrap().to_string_lossy(),
        )?,
        format_version: 1,
        rpc_url: config.rpc_url.clone(),
        lsp_public_key: None,
        identity_public_key: Pubkey(session.identity_public_key().serialize()),
        tenant_id: session.tenant_id().as_str().to_owned(),
        registration: Registration::IdentitySaved,
        tenant: None,
        session: session.state().clone(),
    };
    dir.save_json(&record)
        .context("保存初始账户失败；尚未提交注册，原文件已保留")?;
    Ok((record, session))
}

async fn restore(
    dir: &AccountDir,
    config: &Connection,
) -> Result<(AccountRecord, HostedSession<FileSignerStore>)> {
    let record: AccountRecord = dir.read_json()?;
    let owner = crate::accounts::owner_for(
        &config.root,
        &dir.path.file_name().unwrap().to_string_lossy(),
    )?;
    if record.user != owner {
        bail!("账户永久用户归属与清单不一致；拒绝转移身份或租户");
    }
    if record.format_version != 1 {
        bail!("account.json 的格式版本不支持；原文件已保留");
    }
    // Offline initialization has not yet established a remote LSP identity.
    // The first registration may use its advertised URL instead of a local listen address.
    if record.rpc_url != config.rpc_url
        && !(record.registration == Registration::IdentitySaved && record.lsp_public_key.is_none())
    {
        bail!("RPC 地址与保存的 LSP 关联不一致；原账户已保留，不能用于另一套服务");
    }
    let bytes = Zeroizing::new(read_private(&dir.path.join("root-key"))?);
    let secret =
        Zeroizing::new(<[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| {
            anyhow::anyhow!("root-key 损坏：根密钥备份必须为 32 字节；原文件已保留")
        })?);
    let root_key = RootKey::import(*secret).context("root-key 不是有效根密钥；原文件已保留")?;
    let root = RootSigner::open(root_key, FileSignerStore::open(dir)?)
        .await
        .context("无法恢复 SDK：根密钥与 SDK 存储不匹配，或 SDK 数据损坏；原文件已保留")?;
    let session = HostedSession::new(root).with_state(record.session.clone());
    if record.identity_public_key != Pubkey(session.identity_public_key().serialize())
        || record.tenant_id != session.tenant_id().as_str()
    {
        bail!("account.json 的身份公钥或 tenant_id 与 SDK 不匹配；原文件已保留");
    }
    let has_token = session
        .tenant_token()
        .is_some_and(|token| !token.trim().is_empty());
    if record.registration == Registration::Registered {
        if !has_token || record.lsp_public_key.is_none() || record.tenant.is_none() {
            bail!(
                "已注册账户缺少用户凭证或 LSP/用户节点记录；原文件已保留。当前接口无法找回丢失的用户凭证"
            );
        }
        validate_tenant(record.tenant.as_ref().unwrap(), &record)?;
    } else if session.tenant_token().is_some() || record.tenant.is_some() {
        bail!("account.json 的注册进度与会话状态不一致；原文件已保留");
    } else if record.registration == Registration::AwaitingResult && record.lsp_public_key.is_none()
    {
        bail!("等待注册结果的账户缺少目标 LSP 公钥；原文件已保留");
    }
    Ok((record, session))
}

fn validate_tenant(tenant: &LspTenantStatus, record: &AccountRecord) -> Result<()> {
    if tenant.tenant_id != record.tenant_id
        || tenant.root_signer_pubkey != Some(record.identity_public_key)
    {
        bail!("用户节点记录的 tenant_id 或公钥与本地身份不一致；原账户已保留");
    }
    Ok(())
}

async fn register(
    dir: &AccountDir,
    config: &Connection,
    rpc: &Rpc,
    record: &mut AccountRecord,
    session: &mut HostedSession<FileSignerStore>,
) -> Result<()> {
    let operator = config.registration_token()?;
    record.rpc_url = config.rpc_url.clone();
    let status = rpc.get_status(&operator).await?;
    match record.lsp_public_key {
        Some(saved) if saved != status.public_node_id => {
            bail!("当前 LSP 公钥与已保存的目标不一致；原账户已保留");
        }
        None => {
            record.lsp_public_key = Some(status.public_node_id);
            dir.save_json(record)
                .context("保存目标 LSP 身份失败；尚未提交注册")?;
        }
        _ => {}
    }
    let tenants = rpc.list_tenants(&operator).await?;
    for tenant in &tenants.tenants {
        if tenant.tenant_id == record.tenant_id
            || tenant.root_signer_pubkey == Some(record.identity_public_key)
        {
            validate_tenant(tenant, record)?;
            bail!(
                "服务端已存在用户 {}，但本地缺少用户凭证。当前 LSP RPC 不提供凭证找回接口；不能重新注册。原身份与进度已保留",
                record.tenant_id
            );
        }
    }
    // Even a pending attempt can proceed only after checking that the tenant is absent.
    let nonce = rpc
        .registry_nonce(&operator, record.identity_public_key)
        .await?;
    if Some(nonce.lsp_node_id) != record.lsp_public_key {
        bail!("注册随机数中的 LSP 公钥与已保存的目标不一致；未提交注册，原账户已保留");
    }
    let params = session
        .begin_registration(nonce)
        .context("SDK 拒绝注册随机数；未提交注册")?;
    record.registration = Registration::AwaitingResult;
    dir.save_json(record)
        .context("保存等待注册结果的进度失败；未提交注册")?;
    let result = rpc.register(&operator, params).await.context(
        "注册结果待核对；本地身份与进度已保留，未自动重发。再次执行 ./lsp-cli create-channel -u N 会先核对服务端记录",
    )?;
    validate_tenant(&result.tenant, record)
        .context("注册响应无效，结果待核对；本地身份与进度已保留")?;
    if result.access_token.trim().is_empty() {
        bail!("注册响应缺少有效用户凭证，结果待核对；本地身份与进度已保留");
    }
    record.tenant = Some(result.tenant.clone());
    session
        .finish_registration(result)
        .context("SDK 拒绝注册响应，结果待核对；原账户已保留")?;
    record.registration = Registration::Registered;
    record.session = session.state().clone();
    dir.save_json(record).context(
        "注册已返回成功，但保存用户凭证失败，结果待核对；请保留账户文件。当前接口无法找回丢失的凭证",
    )?;
    Ok(())
}

/// Offline identity initialization; partial files are never replaced.
pub async fn init_local(dir: &AccountDir, config: &Connection) -> Result<()> {
    if dir.is_initialized()? {
        restore(dir, config).await?;
    } else {
        initialize(dir, config).await?;
    }
    Ok(())
}

fn registration_recovery(config: &Connection, id: &str) -> Result<String> {
    Ok(crate::accounts::owner_for(&config.root, id)?.map_or_else(
        || "旧未绑定身份不能通过历史入口重新注册；保留资料核对原注册结果".into(),
        |u| format!("执行 ./lsp-cli create-channel -u {u}，继续使用原 RPC 地址；注册未完成时默认复用本项目管理凭证，可用 --token 或 --token-file 覆盖"),
    ))
}

pub async fn verify_local(dir: &AccountDir, config: &Connection) -> Result<()> {
    restore(dir, config).await?;
    Ok(())
}

#[cfg(test)]
#[path = "../tests/unit/test_account.rs"]
pub(crate) mod tests;
