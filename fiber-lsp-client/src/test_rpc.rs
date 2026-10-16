use super::*;

#[test]
fn rejects_mismatched_response_and_redacts_server_errors() {
    assert!(
        decode_response::<Value>(br#"{"jsonrpc":"2.0","id":2,"result":{}}"#, 1, "test").is_err()
    );
    let error = decode_response::<Value>(
        br#"{"jsonrpc":"2.0","id":1,"error":{"code":-1,"message":"secret-token","data":"secret-token"}}"#,
        1,
        "test",
    ).unwrap_err();
    assert!(error.to_string().contains("code -1"));
    assert!(!error.to_string().contains("secret-token"));
    let error = decode_response::<u64>(
        br#"{"jsonrpc":"2.0","id":1,"result":"secret-token"}"#,
        1,
        "test",
    )
    .unwrap_err();
    assert!(!error.to_string().contains("secret-token"));
}

#[test]
fn missing_config_fields_name_the_file_and_field() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.yml");
    let error = Config::from_path(root.path(), &path)
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains(path.to_str().unwrap()));
    assert!(error.contains("--config"));
    fs::write(&path, "rpc: {}\n").unwrap();
    let error = Config::from_path(root.path(), &path)
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains(path.to_str().unwrap()));
    assert!(error.contains("rpc.listening_addr"));
}

#[test]
fn default_config_needs_home_only_without_xdg() {
    assert_eq!(
        node3_config(Some("/data".into()), None).unwrap(),
        PathBuf::from("/data/offckb-nodejs/devnet/fiber/nodes/3/config.yml")
    );
    for xdg in [None, Some("".into())] {
        assert_eq!(
            node3_config(xdg, Some("/home/test".into())).unwrap(),
            PathBuf::from("/home/test/.local/share/offckb-nodejs/devnet/fiber/nodes/3/config.yml")
        );
    }
    assert!(
        node3_config(None, None)
            .unwrap_err()
            .to_string()
            .contains("--config")
    );
}

#[test]
fn external_config_keeps_account_and_token_in_project() {
    let root = tempfile::tempdir().unwrap();
    let node = tempfile::tempdir().unwrap();
    let path = node.path().join("config.yml");
    fs::write(
        &path,
        "services: [fiber, lsp]\nrpc:\n  listening_addr: 127.0.0.1:21716\n  biscuit_public_key: test-key\n  biscuit_private_key_path: /unused/key\n",
    )
    .unwrap();
    let config = Config::from_path(root.path(), &path).unwrap();
    assert_eq!(config.rpc_url, "http://127.0.0.1:21716");
    assert_eq!(config.rpc_auth_public_key, "test-key");
    assert_eq!(
        config.account_dir(),
        root.path().join(".state/fiber-lsp-client/default")
    );
    fs::create_dir_all(root.path().join("configs/keys")).unwrap();
    fs::write(
        root.path().join("configs/keys/operator-token"),
        "test-token",
    )
    .unwrap();
    assert_eq!(config.operator_token().unwrap().as_str(), "test-token");
}
