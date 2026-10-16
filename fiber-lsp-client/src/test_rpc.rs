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
fn connection_initialization_needs_only_a_url_and_keeps_accounts_local() {
    let root = tempfile::tempdir().unwrap();
    let connection = Connection::new(
        root.path().to_path_buf(),
        "https://lsp.example/rpc".into(),
        None,
    )
    .unwrap();
    assert!(connection.registration_token.is_none());
    assert_eq!(
        connection.account_dir(),
        root.path().join(".state/fiber-lsp-client/default")
    );
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn fresh_connection_uses_only_url_and_explicit_token() {
    let root = tempfile::tempdir().unwrap();
    for url in [
        "file:///tmp/lsp",
        "http://user:secret@localhost",
        "not-a-url",
    ] {
        let options = crate::ConnectionOptions {
            rpc_url: url.into(),
            ..Default::default()
        };
        assert!(Connection::connect(&options).is_err());
    }
    let path = root.path().join("registration-token");
    fs::write(&path, "  bootstrap-secret\n").unwrap();
    let options = crate::ConnectionOptions {
        rpc_url: "https://lsp.example/rpc".into(),
        token_file: Some(path),
        ..Default::default()
    };
    let connection = Connection::connect(&options).unwrap();
    assert_eq!(connection.rpc_url, "https://lsp.example/rpc");
    assert_eq!(
        connection.registration_token().unwrap().as_str(),
        "bootstrap-secret"
    );
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    let empty = crate::ConnectionOptions {
        token: Some("  ".into()),
        ..Default::default()
    };
    assert!(empty.read_token().is_err());
}

#[test]
fn registration_uses_default_token_lazily_and_explicit_token_takes_priority() {
    let root = tempfile::tempdir().unwrap();
    let mut connection = Connection {
        root: root.path().to_path_buf(),
        rpc_url: "https://lsp.example/rpc".into(),
        registration_token: None,
    };
    assert!(connection.registration_token().is_err());
    let path = root.path().join("configs/keys/operator-token");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "  generated-token\n").unwrap();
    assert_eq!(
        connection.registration_token().unwrap().as_str(),
        "generated-token"
    );
    connection.registration_token = Some(Zeroizing::new("override-token".into()));
    fs::write(&path, "").unwrap();
    assert_eq!(
        connection.registration_token().unwrap().as_str(),
        "override-token"
    );
    connection.registration_token = None;
    assert!(connection.registration_token().is_err());
}
