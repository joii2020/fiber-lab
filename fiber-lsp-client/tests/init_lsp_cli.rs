use biscuit_auth::{Biscuit, KeyPair, PrivateKey, PublicKey};
use serde_yaml::Value;
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::PathBuf,
    process::{Command, Output},
    str::FromStr,
};

const CONFIG: &str = r#"services: [fiber, rpc, ckb]
fiber:
  announced_node_name: Node3
  listening_addr: /ip4/0.0.0.0/tcp/8346
  auto_accept_channel_ckb_funding_amount: 9900000000
rpc:
  listening_addr: 127.0.0.1:21716
  enabled_modules: [info, channel, peer, payment, invoice, graph]
ckb:
  rpc_url: http://127.0.0.1:8114
  secret_key_path: ckb/key
  udt_whitelist:
    - name: TEST
      script: {code_hash: '0x0123', hash_type: type, args: '0x'}
lsp:
  max_tenants: 42
custom: {keep: true}
"#;

struct Fixture {
    temp: tempfile::TempDir,
    config: PathBuf,
    private: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("node/config.yml");
        fs::create_dir(config.parent().unwrap()).unwrap();
        fs::write(&config, CONFIG).unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o640)).unwrap();
        let private = config.parent().unwrap().join("biscuit-private-key");
        Self {
            temp,
            config,
            private,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber-lsp-client"));
        command
            .current_dir(self.temp.path())
            .args(["admin", "init-lsp"]);
        command
    }

    fn run(&self) -> Output {
        self.command()
            .arg("--config")
            .arg(&self.config)
            .output()
            .unwrap()
    }

    fn document(&self) -> Value {
        serde_yaml::from_slice(&fs::read(&self.config).unwrap()).unwrap()
    }

    fn assert_rejected_without_changes(&self, expected: &str) {
        let config = fs::read(&self.config).unwrap();
        let key = fs::read(&self.private).ok();
        let result = self.run();
        assert!(!result.status.success());
        let error = String::from_utf8_lossy(&result.stderr);
        assert!(error.contains(expected), "{error}");
        assert_eq!(fs::read(&self.config).unwrap(), config);
        assert_eq!(fs::read(&self.private).ok(), key);
    }
}

#[test]
fn fresh_init_preserves_node_settings_and_creates_usable_keys_and_token() {
    let fixture = Fixture::new();
    let original: Value = serde_yaml::from_str(CONFIG).unwrap();
    let result = fixture.run();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let updated = fixture.document();
    assert_eq!(
        updated["services"],
        serde_yaml::from_str::<Value>("[fiber, rpc, ckb, lsp]").unwrap()
    );
    let modules = updated["rpc"]["enabled_modules"].as_sequence().unwrap();
    assert!(modules.starts_with(original["rpc"]["enabled_modules"].as_sequence().unwrap()));
    assert_eq!(
        modules[modules.len() - 2..],
        [Value::from("lsp"), Value::from("pubsub")]
    );
    assert_eq!(updated["fiber"]["announced_node_name"], "fiber-lab-lsp");
    for field in ["ckb", "lsp", "custom"] {
        assert_eq!(updated[field], original[field]);
    }
    assert_eq!(
        updated["fiber"]["listening_addr"],
        original["fiber"]["listening_addr"]
    );
    assert_eq!(
        updated["fiber"]["auto_accept_channel_ckb_funding_amount"],
        original["fiber"]["auto_accept_channel_ckb_funding_amount"]
    );
    assert_eq!(
        updated["rpc"]["listening_addr"],
        original["rpc"]["listening_addr"]
    );
    assert_eq!(updated["rpc"]["ws_listening_addr"], "127.0.0.1:21717");
    assert_eq!(
        updated["rpc"]["biscuit_private_key_path"].as_str().unwrap(),
        fixture.private.to_str().unwrap()
    );
    assert_eq!(
        fs::metadata(&fixture.private).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(&fixture.config).unwrap().permissions().mode() & 0o777,
        0o640
    );
    let private_text = fs::read_to_string(&fixture.private).unwrap();
    let private = PrivateKey::from_str(private_text.trim()).unwrap();
    let public =
        PublicKey::from_str(updated["rpc"]["biscuit_public_key"].as_str().unwrap()).unwrap();
    assert_eq!(KeyPair::from(&private).public(), public);
    assert!(!String::from_utf8_lossy(&result.stdout).contains(private_text.trim()));

    let token_path = fixture.temp.path().join("operator-token");
    let result = Command::new(env!("CARGO_BIN_EXE_fiber-lsp-client"))
        .current_dir(fixture.temp.path())
        .args(["admin", "create-token", "--config"])
        .arg(&fixture.config)
        .arg("--output")
        .arg(&token_path)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let token_text = fs::read_to_string(&token_path).unwrap();
    Biscuit::from_base64(&token_text, public).unwrap();

    assert!(fixture.run().status.success());
    assert_eq!(fixture.document(), updated);
    assert_eq!(fs::read_to_string(&fixture.private).unwrap(), private_text);
    assert_eq!(fs::read_to_string(&token_path).unwrap(), token_text);
}

#[test]
fn existing_relative_private_path_is_reused_and_made_absolute() {
    let fixture = Fixture::new();
    let key = KeyPair::new();
    let custom = fixture.temp.path().join("custom-key");
    let private_text = key.private().to_prefixed_string();
    fs::write(&custom, &private_text).unwrap();
    fs::set_permissions(&custom, fs::Permissions::from_mode(0o600)).unwrap();
    let mut config = fixture.document();
    config["services"]
        .as_sequence_mut()
        .unwrap()
        .push("LSP".into());
    config["rpc"]["enabled_modules"]
        .as_sequence_mut()
        .unwrap()
        .extend(["lsp".into(), "pubsub".into()]);
    config["rpc"]["biscuit_public_key"] = key.public().to_string().into();
    config["rpc"]["biscuit_private_key_path"] = "custom-key".into();
    config["rpc"]["ws_listening_addr"] = "127.0.0.1:21800".into();
    fs::write(&fixture.config, serde_yaml::to_string(&config).unwrap()).unwrap();
    assert!(fixture.run().status.success());
    let updated = fixture.document();
    assert_eq!(
        updated["rpc"]["biscuit_private_key_path"].as_str().unwrap(),
        custom.to_str().unwrap()
    );
    assert_eq!(updated["services"], config["services"]);
    assert_eq!(updated["rpc"]["ws_listening_addr"], "127.0.0.1:21800");
    assert_eq!(
        updated["rpc"]["enabled_modules"],
        config["rpc"]["enabled_modules"]
    );
    assert_eq!(fs::read_to_string(custom).unwrap(), private_text);
    assert!(!fixture.private.exists());
}

#[test]
fn orphaned_private_key_is_reused_after_interrupted_initialization() {
    let fixture = Fixture::new();
    let key = KeyPair::new();
    let text = key.private().to_prefixed_string();
    fs::write(&fixture.private, &text).unwrap();
    fs::set_permissions(&fixture.private, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(fixture.run().status.success());
    assert_eq!(
        fixture.document()["rpc"]["biscuit_public_key"],
        key.public().to_string()
    );
    assert_eq!(fs::read_to_string(&fixture.private).unwrap(), text);
}

#[test]
fn invalid_yaml_and_fields_do_not_create_a_key_or_leak_input() {
    let fixture = Fixture::new();
    for text in [
        "rpc: [secret-invalid-yaml",
        "[]",
        "services: fiber",
        "services: [1]",
        "services: [fiber]\nfiber: []",
        "services: [fiber]\nfiber: {}\nrpc: []",
        "services: [fiber]\nfiber: {}\nrpc: {enabled_modules: info}",
        "services: [fiber]\nfiber: {}\nrpc: {enabled_modules: [], biscuit_public_key: secret-invalid-public}",
        "services: [fiber]\nfiber: {}\nrpc: {enabled_modules: [], biscuit_private_key_path: [secret-path]}",
    ] {
        fs::write(&fixture.config, text).unwrap();
        let result = fixture.run();
        assert!(!result.status.success());
        assert!(!String::from_utf8_lossy(&result.stderr).contains("secret-"));
        assert_eq!(fs::read_to_string(&fixture.config).unwrap(), text);
        assert!(!fixture.private.exists());
    }
}

#[test]
fn missing_mismatched_or_unsafe_keys_never_rotate_existing_identity() {
    let fixture = Fixture::new();
    assert!(fixture.run().status.success());
    fs::remove_file(&fixture.private).unwrap();
    fixture.assert_rejected_without_changes("私钥文件缺失");
    fs::write(
        &fixture.private,
        KeyPair::new().private().to_prefixed_string(),
    )
    .unwrap();
    fs::set_permissions(&fixture.private, fs::Permissions::from_mode(0o600)).unwrap();
    fixture.assert_rejected_without_changes("不匹配");
    fs::write(&fixture.private, "secret-invalid-key").unwrap();
    fixture.assert_rejected_without_changes("私钥格式无效");
    assert!(!String::from_utf8_lossy(&fixture.run().stderr).contains("secret-invalid-key"));
    fs::set_permissions(&fixture.private, fs::Permissions::from_mode(0o644)).unwrap();
    fixture.assert_rejected_without_changes("chmod 600");
    let real = fixture.private.with_extension("real");
    fs::rename(&fixture.private, &real).unwrap();
    symlink(&real, &fixture.private).unwrap();
    fixture.assert_rejected_without_changes("符号链接");
    fs::remove_file(&real).unwrap();
    fixture.assert_rejected_without_changes("符号链接");
}

#[test]
fn default_path_uses_xdg_and_config_symlinks_are_rejected() {
    let fixture = Fixture::new();
    let data = fixture.temp.path().join("data");
    let config = data.join("offckb-nodejs/devnet/fiber/nodes/3/config.yml");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    fs::copy(&fixture.config, &config).unwrap();
    let result = fixture
        .command()
        .env("XDG_DATA_HOME", &data)
        .env_remove("HOME")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        config
            .parent()
            .unwrap()
            .join("biscuit-private-key")
            .exists()
    );
    let target = fixture.config.with_extension("real");
    fs::rename(&fixture.config, &target).unwrap();
    symlink(&target, &fixture.config).unwrap();
    fixture.assert_rejected_without_changes("无法读取配置");
    assert!(!fixture.private.exists());
}
