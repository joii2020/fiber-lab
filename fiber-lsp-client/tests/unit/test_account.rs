use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};

use super::*;

pub(crate) async fn run(
    config: &Connection,
    create: bool,
    selected: Option<&str>,
) -> Result<AccountStatus> {
    let dir = crate::accounts::tests::select(&config.root, selected, create)?;
    run_dir(config, create, dir).await
}

impl ClientAccount {
    pub(crate) async fn load(config: &Connection, selected: Option<&str>) -> Result<Self> {
        let dir = crate::accounts::tests::select(&config.root, selected, false)?;
        Self::load_dir(config, dir).await
    }
}

pub(crate) async fn test_registered(config: &Connection) -> ClientAccount {
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

pub(crate) fn save_test_state(dir: &AccountDir, state: &HostedSessionState) {
    save_session(dir, state).unwrap();
}

pub(crate) fn fixture() -> (tempfile::TempDir, Connection, TcpListener) {
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let config = Connection {
        root: root.path().to_path_buf(),
        rpc_url: format!("http://{}", listener.local_addr().unwrap()),
        registration_token: Some(Zeroizing::new("operator-test-token".into())),
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
    let (_root, mut config, listener) = fixture();
    config.registration_token = None;
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
    let saved: Value = created.client.dir.read_json().unwrap();
    assert_eq!(saved["rpc_url"], config.rpc_url);
    assert!(
        !serde_json::to_string(&saved)
            .unwrap()
            .contains("operator-test-token")
    );
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
    let dir = test_registered(&config).await.dir;
    let original = fs::read(dir.path.join("account.json")).unwrap();
    let original_url = config.rpc_url.clone();
    config.rpc_url = "http://another-lsp:80".into();
    let error = restore(&dir, &config).await.err().unwrap().to_string();
    assert!(error.contains("关联不一致"));
    config.rpc_url = original_url;
    atomic_write(&dir.path.join("root-key"), &[3; 32]).unwrap();
    let error = restore(&dir, &config).await.err().unwrap().to_string();
    assert!(error.contains("不匹配"));
    assert_eq!(fs::read(dir.path.join("account.json")).unwrap(), original);
}

#[tokio::test]
async fn missing_registration_token_keeps_the_initial_identity() {
    let (_root, mut config, _listener) = fixture();
    config.registration_token = None;
    fs::remove_file(config.root.join("configs/keys/operator-token")).unwrap();
    assert!(
        run(&config, true, None)
            .await
            .err()
            .unwrap()
            .to_string()
            .contains("--token")
    );
    let dir = crate::accounts::tests::select(&config.root, None, false).unwrap();
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

#[tokio::test]
async fn saved_account_connects_without_config_or_bootstrap_token_and_accepts_legacy_metadata() {
    let (_root, config, listener) = fixture();
    let account = test_registered(&config).await;
    // Older account files contain this field; it is no longer an input to connecting.
    let mut legacy: Value = account.dir.read_json().unwrap();
    legacy["rpc_auth_public_key"] = json!("legacy-biscuit-key");
    account.dir.save_json(&legacy).unwrap();
    fs::remove_file(config.root.join("configs/keys/operator-token")).unwrap();
    let before = fs::read(account.dir.path.join("account.json")).unwrap();
    let mut connection = Connection::connect(&crate::ConnectionOptions {
        rpc_url: config.rpc_url.clone(),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(connection.rpc_url, config.rpc_url);
    assert!(connection.registration_token.is_none());
    connection.root = config.root.clone();
    let server = serve(
        listener,
        vec![
            ("list_channels", json!({"channels": []})),
            ("list_channels", json!({"channels": []})),
        ],
    );
    let dir = account.dir;
    let restored = run_dir(&connection, false, dir).await.unwrap();
    assert_eq!(
        restored.client.session.tenant_token(),
        Some("user-test-token")
    );
    assert_eq!(
        fs::read(restored.client.dir.path.join("account.json")).unwrap(),
        before
    );
    server.join().unwrap();
}

#[tokio::test]
async fn explicit_rpc_override_cannot_rebind_a_saved_account() {
    let (_root, config, _listener) = fixture();
    let account = test_registered(&config).await;
    let before = fs::read(account.dir.path.join("account.json")).unwrap();
    let options = crate::ConnectionOptions {
        rpc_url: "https://another-lsp.example/rpc".into(),
        ..Default::default()
    };
    let mut connection = Connection::connect(&options).unwrap();
    connection.root = config.root.clone();
    assert!(
        restore(&account.dir, &connection)
            .await
            .err()
            .unwrap()
            .to_string()
            .contains("关联不一致")
    );
    assert_eq!(
        fs::read(account.dir.path.join("account.json")).unwrap(),
        before
    );
}

#[tokio::test]
async fn first_registration_binds_advertised_url_instead_of_offline_listen_address() {
    let (_root, config, listener) = fixture();
    let dir = AccountDir::lock(&config.account_dir()).unwrap();
    let (mut record, _) = initialize(&dir, &config).await.unwrap();
    record.rpc_url = "http://offline-listen-address:21716".into();
    dir.save_json(&record).unwrap();
    let (mut record, mut session) = restore(&dir, &config).await.unwrap();
    let server = serve(
        listener,
        vec![
            ("lsp_get_status", lsp_status()),
            ("lsp_list_tenants", json!({"tenants": []})),
            (
                "lsp_get_tenant_registry_nonce",
                json!({
                    "lsp_node_id": lsp_key(), "root_signer_pubkey": record.identity_public_key,
                    "nonce": format!("0x{}", "05".repeat(32))
                }),
            ),
            (
                "lsp_register_tenant",
                json!({"tenant": tenant(&record), "access_token": "user-test-token"}),
            ),
        ],
    );
    register(
        &dir,
        &config,
        &Rpc::new(&config.rpc_url).unwrap(),
        &mut record,
        &mut session,
    )
    .await
    .unwrap();
    let saved: Value = dir.read_json().unwrap();
    assert_eq!(saved["rpc_url"], config.rpc_url);
    assert!(
        !serde_json::to_string(&saved)
            .unwrap()
            .contains("operator-test-token")
    );
    server.join().unwrap();
}
