use super::*;

#[test]
fn init_lsp_defaults_funding_and_preserves_explicit_configuration() {
    for amount in [
        None,
        Some("null"),
        Some("0"),
        Some("10000000000"),
        Some("100000000000"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yml");
        let setting = amount.map_or(String::new(), |a| {
            format!("  auto_accept_channel_ckb_funding_amount: {a}\n")
        });
        fs::write(&path, format!("services: [fiber]\nfiber:\n  announced_node_name: original\n{setting}rpc:\n  enabled_modules: [channel]\n")).unwrap();
        let options = InitLsp {
            config: Some(path.clone()),
        };
        let initialized = init_lsp(&options).unwrap();
        let key = fs::read(&initialized.private_key).unwrap();
        init_lsp(&options).unwrap();
        assert_eq!(fs::read(&initialized.private_key).unwrap(), key);
        let document: Value = serde_yaml::from_slice(&fs::read(path).unwrap()).unwrap();
        let expected = match amount {
            None | Some("null") => 1000 * crate::wallet::CKB,
            Some(a) => a.parse().unwrap(),
        };
        assert_eq!(
            document["fiber"]["auto_accept_channel_ckb_funding_amount"].as_u64(),
            Some(expected)
        );
    }
}

#[test]
fn default_output_matches_client_operator_token_location() {
    let options = CreateToken {
        config: None,
        private_key_file: None,
        output: None,
    };
    let client_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    assert_eq!(
        output_path(&options).unwrap(),
        client_root.join("configs/keys/operator-token")
    );
}
