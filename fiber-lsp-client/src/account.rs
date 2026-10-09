use anyhow::{Context, Result, bail};
use fiber_json_types::{LspTenantStatus, Pubkey};
use fiber_lsp_sdk::{HostedSession, HostedSessionState, RootKey, RootSigner};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::{
    rpc::{Config, Rpc},
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
    format_version: u32,
    rpc_url: String,
    rpc_auth_public_key: String,
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

pub async fn run(config: &Config, create: bool, selected: Option<&str>) -> Result<AccountStatus> {
    let dir = crate::accounts::select(&config.root, selected, create)?;
    let id = dir.path.file_name().unwrap().to_string_lossy();
    let (mut record, mut session) = if dir.is_initialized()? {
        restore(&dir, config).await?
    } else if create && selected.is_none() {
        initialize(&dir, config).await?
    } else {
        bail!("账户不存在；请先执行 ./lsp-cli create-user");
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
            bail!(
                "账户身份已保存，但注册尚未完成；请执行 ./lsp-cli create-user --account {id} 核对注册进度。原数据已保留"
            );
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
    pub async fn load(config: &Config, selected: Option<&str>) -> Result<Self> {
        let dir = crate::accounts::select(&config.root, selected, false)?;
        let id = dir.path.file_name().unwrap().to_string_lossy();
        let (record, session) = restore(&dir, config).await?;
        if record.registration != Registration::Registered {
            bail!("账户注册尚未完成；请先执行 ./lsp-cli create-user --account {id}；原数据已保留");
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
    config: &Config,
) -> Result<(AccountRecord, HostedSession<FileSignerStore>)> {
    let created = RootSigner::create_random(FileSignerStore::new(dir))
        .await
        .context("创建 SDK 身份失败；已写入的文件保留，不会重新生成身份")?;
    let backup = Zeroizing::new(created.root_key_backup.expose_secret());
    atomic_write(&dir.path.join("root-key"), backup.as_ref())
        .context("保存根密钥备份失败；尚未提交注册，原文件已保留")?;
    let session = HostedSession::new(created.root_signer);
    let record = AccountRecord {
        format_version: 1,
        rpc_url: config.rpc_url.clone(),
        rpc_auth_public_key: config.rpc_auth_public_key.clone(),
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
    config: &Config,
) -> Result<(AccountRecord, HostedSession<FileSignerStore>)> {
    let record: AccountRecord = dir.read_json()?;
    if record.format_version != 1 {
        bail!("account.json 的格式版本不支持；原文件已保留");
    }
    if record.rpc_url != config.rpc_url || record.rpc_auth_public_key != config.rpc_auth_public_key
    {
        bail!(
            "当前配置的 RPC 地址或 Biscuit 公钥与保存的 LSP 关联不一致；原账户已保留，不能用于另一套服务"
        );
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
    config: &Config,
    rpc: &Rpc,
    record: &mut AccountRecord,
    session: &mut HostedSession<FileSignerStore>,
) -> Result<()> {
    let operator = config.operator_token()?;
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
        "注册结果待核对；本地身份与进度已保留，未自动重发。再次执行 ./lsp-cli create-user 会先核对服务端记录",
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

#[cfg(test)]
pub async fn test_registered(config: &Config) -> ClientAccount {
    let dir = AccountDir::lock(&config.account_dir()).unwrap();
    let (mut record, mut session) = initialize(&dir, config).await.unwrap();
    let lsp_public_key = Pubkey(RootKey::import([2; 32]).unwrap().public_key().serialize());
    let tenant = serde_json::from_value(serde_json::json!({
        "tenant_id": record.tenant_id, "root_signer_pubkey": record.identity_public_key,
        "invoice_pubkey": record.identity_public_key, "private_channel_id": null,
        "created_at": "0x1", "runtime_status": "cold", "channel_online": false,
    }))
    .unwrap();
    session
        .finish_registration(fiber_json_types::RegisterLspTenantResult {
            tenant,
            access_token: "user-test-token".into(),
        })
        .unwrap();
    record.lsp_public_key = Some(lsp_public_key);
    record.tenant = serde_json::from_value(serde_json::json!({
        "tenant_id": record.tenant_id, "root_signer_pubkey": record.identity_public_key,
        "invoice_pubkey": record.identity_public_key, "private_channel_id": null,
        "created_at": "0x1", "runtime_status": "cold", "channel_online": false,
    }))
    .unwrap();
    record.registration = Registration::Registered;
    record.session = session.state().clone();
    dir.save_json(&record).unwrap();
    ClientAccount {
        dir,
        session,
        lsp_public_key,
        invoice_public_key: record.identity_public_key,
    }
}

#[cfg(test)]
pub(crate) fn save_test_state(dir: &AccountDir, state: &HostedSessionState) {
    save_session(dir, state).unwrap();
}

#[cfg(test)]
pub(crate) mod tests {
    use std::{
        fs,
        io::{Read, Write},
        net::TcpListener,
        thread,
        time::{Duration, Instant},
    };

    use serde_json::{Value, json};

    use super::*;

    pub(crate) fn fixture() -> (tempfile::TempDir, Config, TcpListener) {
        let root = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let config = Config {
            root: root.path().to_path_buf(),
            rpc_url: format!("http://{}", listener.local_addr().unwrap()),
            rpc_auth_public_key: "test-biscuit-key".to_owned(),
        };
        fs::create_dir_all(root.path().join("configs/keys")).unwrap();
        fs::write(
            root.path().join("configs/keys/operator-token"),
            "operator-test-token",
        )
        .unwrap();
        (root, config, listener)
    }

    fn lsp_key() -> Pubkey {
        Pubkey(RootKey::import([2; 32]).unwrap().public_key().serialize())
    }

    fn lsp_status() -> Value {
        json!({"public_node_id": lsp_key(), "tenant_store_root": "/test", "registered_tenants": "0x0", "active_tenants": "0x0"})
    }

    fn tenant(record: &AccountRecord) -> Value {
        json!({
            "tenant_id": record.tenant_id, "root_signer_pubkey": record.identity_public_key,
            "invoice_pubkey": record.identity_public_key, "private_channel_id": null,
            "created_at": "0x1", "runtime_status": "cold", "channel_online": false,
        })
    }

    // A bounded HTTP fixture checks both the RPC method sequence and credential choice.
    pub(crate) fn serve(
        listener: TcpListener,
        steps: Vec<(&'static str, Value)>,
    ) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            listener.set_nonblocking(true).unwrap();
            for (method, result) in steps {
                let deadline = Instant::now() + Duration::from_secs(3);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(Instant::now() < deadline, "missing RPC: {method}");
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("accept: {error}"),
                    }
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut bytes = Vec::new();
                let (headers, request) = loop {
                    let mut chunk = [0; 1024];
                    let n = stream.read(&mut chunk).unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&chunk[..n]);
                    if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                        let headers = String::from_utf8(bytes[..end].to_vec())
                            .unwrap()
                            .to_lowercase();
                        let length: usize = headers
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length: "))
                            .unwrap()
                            .parse()
                            .unwrap();
                        if bytes.len() >= end + 4 + length {
                            let request: Value =
                                serde_json::from_slice(&bytes[end + 4..end + 4 + length]).unwrap();
                            break (headers, request);
                        }
                    }
                };
                assert_eq!(request["method"], method);
                if matches!(
                    method,
                    "get_block_hash" | "get_transaction" | "get_live_cell" | "get_cells"
                ) {
                    assert!(!headers.contains("authorization:"));
                } else {
                    let expected_token = if method.starts_with("lsp_") {
                        "operator-test-token"
                    } else {
                        "user-test-token"
                    };
                    assert!(headers.contains(&format!("authorization: bearer {expected_token}")));
                }
                let result = if let Some(expected) = result.get("_expect_params") {
                    assert_eq!(&request["params"], expected);
                    result["_reply"].clone()
                } else {
                    result
                };
                let body = if let Some(error) = result.get("error") {
                    json!({"jsonrpc": "2.0", "id": request["id"], "error": error})
                } else {
                    json!({"jsonrpc": "2.0", "id": request["id"], "result": result})
                }
                .to_string();
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
            }
        })
    }

    #[tokio::test]
    async fn interrupted_identity_registration_and_restore_use_the_same_account() {
        let (_root, config, listener) = fixture();
        let dir = AccountDir::lock(&config.account_dir()).unwrap();
        let (mut record, _) = initialize(&dir, &config).await.unwrap();
        record.registration = Registration::AwaitingResult;
        record.lsp_public_key = Some(lsp_key());
        dir.save_json(&record).unwrap();
        let identity = record.identity_public_key;
        let id = record.tenant_id.clone();
        let server = serve(
            listener,
            vec![
                ("lsp_get_status", lsp_status()),
                ("lsp_list_tenants", json!({"tenants": []})),
                (
                    "lsp_get_tenant_registry_nonce",
                    json!({"lsp_node_id": lsp_key(), "root_signer_pubkey": identity, "nonce": format!("0x{}", "05".repeat(32))}),
                ),
                (
                    "lsp_register_tenant",
                    json!({"tenant": tenant(&record), "access_token": "user-test-token"}),
                ),
                ("list_channels", json!({"channels": []})),
                ("list_channels", json!({"channels": []})),
                ("list_channels", json!({"channels": []})),
                ("list_channels", json!({"channels": []})),
            ],
        );
        drop(dir);
        let created = run(&config, true, None).await.unwrap();
        assert_eq!(created.identity, identity);
        assert_eq!(created.tenant_id, id);
        assert!(created.channels.is_empty());
        drop(created);
        fs::remove_file(config.root.join("configs/keys/operator-token")).unwrap();
        let status = run(&config, false, None).await.unwrap();
        assert_eq!(status.identity, identity);
        assert_eq!(status.tenant_id, id);
        drop(status);
        let repeated = run(&config, true, None).await.unwrap();
        assert_eq!(repeated.tenant_id, id);
        server.join().unwrap();
    }

    #[tokio::test]
    async fn pending_registration_with_server_record_does_not_request_a_nonce() {
        let (_root, config, listener) = fixture();
        let dir = AccountDir::lock(&config.account_dir()).unwrap();
        let (mut record, _) = initialize(&dir, &config).await.unwrap();
        record.lsp_public_key = Some(lsp_key());
        record.registration = Registration::AwaitingResult;
        dir.save_json(&record).unwrap();
        let original = fs::read(dir.path.join("account.json")).unwrap();
        let server = serve(
            listener,
            vec![
                ("lsp_get_status", lsp_status()),
                ("lsp_list_tenants", json!({"tenants": [tenant(&record)]})),
            ],
        );
        drop(dir);
        let error = run(&config, true, None).await.err().unwrap().to_string();
        assert!(error.contains("不提供凭证找回"));
        assert_eq!(
            fs::read(config.account_dir().join("account.json")).unwrap(),
            original
        );
        server.join().unwrap();
    }

    #[tokio::test]
    async fn malformed_registration_response_retains_pending_progress() {
        let (_root, config, listener) = fixture();
        let dir = AccountDir::lock(&config.account_dir()).unwrap();
        let (record, _) = initialize(&dir, &config).await.unwrap();
        let root_before = fs::read(dir.path.join("root-key")).unwrap();
        let server = serve(
            listener,
            vec![
                ("lsp_get_status", lsp_status()),
                ("lsp_list_tenants", json!({"tenants": []})),
                (
                    "lsp_get_tenant_registry_nonce",
                    json!({"lsp_node_id": lsp_key(), "root_signer_pubkey": record.identity_public_key, "nonce": format!("0x{}", "05".repeat(32))}),
                ),
                (
                    "lsp_register_tenant",
                    json!({"access_token": "secret-response-token"}),
                ),
            ],
        );
        drop(dir);
        let error = run(&config, true, None).await.err().unwrap();
        assert!(error.to_string().contains("结果待核对"));
        assert!(!format!("{error:#}").contains("secret-response-token"));
        let dir = AccountDir::lock(&config.account_dir()).unwrap();
        let saved: AccountRecord = dir.read_json().unwrap();
        assert!(saved.registration == Registration::AwaitingResult);
        assert!(saved.session.tenant_token.is_none());
        assert_eq!(saved.identity_public_key, record.identity_public_key);
        assert_eq!(fs::read(dir.path.join("root-key")).unwrap(), root_before);
        server.join().unwrap();
    }

    #[tokio::test]
    async fn configuration_and_identity_mismatch_leave_files_unchanged() {
        let (_root, mut config, _listener) = fixture();
        let dir = AccountDir::lock(&config.account_dir()).unwrap();
        initialize(&dir, &config).await.unwrap();
        let original = fs::read(dir.path.join("account.json")).unwrap();
        config.rpc_auth_public_key = "another-key".to_owned();
        let error = restore(&dir, &config).await.err().unwrap().to_string();
        assert!(error.contains("关联不一致"));
        config.rpc_auth_public_key = "test-biscuit-key".to_owned();
        atomic_write(&dir.path.join("root-key"), &[3; 32]).unwrap();
        let error = restore(&dir, &config).await.err().unwrap().to_string();
        assert!(error.contains("不匹配"));
        assert_eq!(fs::read(dir.path.join("account.json")).unwrap(), original);
    }

    #[tokio::test]
    async fn missing_operator_token_keeps_the_initial_identity() {
        let (_root, config, _listener) = fixture();
        fs::remove_file(config.root.join("configs/keys/operator-token")).unwrap();
        assert!(
            run(&config, true, None)
                .await
                .err()
                .unwrap()
                .to_string()
                .contains("operator-token")
        );
        let dir = crate::accounts::select(&config.root, None, false).unwrap();
        let account_path = dir.path.clone();
        let record: AccountRecord = dir.read_json().unwrap();
        assert!(record.registration == Registration::IdentitySaved);
        let root_before = fs::read(dir.path.join("root-key")).unwrap();
        drop(dir);
        assert!(run(&config, true, None).await.is_err());
        assert_eq!(
            fs::read(account_path.join("root-key")).unwrap(),
            root_before
        );
        let error = run(&config, false, None).await.err().unwrap().to_string();
        assert!(error.contains("注册尚未完成"));
    }

    #[tokio::test]
    async fn rpc_failures_preserve_registered_account_without_operator_token() {
        let (_root, config, listener) = fixture();
        let dir = AccountDir::lock(&config.account_dir()).unwrap();
        let (mut record, mut session) = initialize(&dir, &config).await.unwrap();
        let result = serde_json::from_value(
            json!({"tenant": tenant(&record), "access_token": "user-test-token"}),
        )
        .unwrap();
        session.finish_registration(result).unwrap();
        record.session = session.state().clone();
        record.lsp_public_key = Some(lsp_key());
        record.registration = Registration::Registered;
        record.tenant = Some(serde_json::from_value(tenant(&record)).unwrap());
        dir.save_json(&record).unwrap();
        let before: Vec<_> = crate::store::ACCOUNT_FILES
            .iter()
            .map(|name| fs::read(dir.path.join(name)).unwrap())
            .collect();
        drop(dir);
        fs::remove_file(config.root.join("configs/keys/operator-token")).unwrap();
        let server = serve(
            listener,
            vec![(
                "list_channels",
                json!({"error": {"code": -32999, "message": "user-test-token is invalid"}}),
            )],
        );
        let error = run(&config, false, None).await.err().unwrap();
        assert!(format!("{error:#}").contains("JSON-RPC 错误"));
        assert!(!format!("{error:#}").contains("user-test-token"));
        server.join().unwrap();
        let error = run(&config, false, None).await.err().unwrap();
        assert!(format!("{error:#}").contains("网络错误"));
        for (name, bytes) in crate::store::ACCOUNT_FILES.iter().zip(before) {
            assert_eq!(fs::read(config.account_dir().join(name)).unwrap(), bytes);
        }
    }
}
