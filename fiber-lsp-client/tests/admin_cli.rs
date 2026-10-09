use biscuit_auth::{Biscuit, KeyPair};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::{Command, Output},
};

struct Fixture {
    temp: tempfile::TempDir,
    key: KeyPair,
    config: PathBuf,
    private: PathBuf,
    output: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let key = KeyPair::new();
        let config = temp.path().join("config.yml");
        let private = temp.path().join("private-key");
        let output = temp.path().join("nested/operator-token");
        fs::write(
            &private,
            format!("{}\n", key.private().to_prefixed_string()),
        )
        .unwrap();
        fs::set_permissions(&private, fs::Permissions::from_mode(0o600)).unwrap();
        // Relative private path resolves from cwd, not the configuration directory.
        fs::write(
            &config,
            format!(
                "rpc:\n  biscuit_public_key: {}\n  biscuit_private_key_path: private-key\n",
                key.public()
            ),
        )
        .unwrap();
        Self {
            temp,
            key,
            config,
            private,
            output,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber-lsp-client"));
        command
            .current_dir(self.temp.path())
            .args(["admin", "create-token"]);
        command
    }

    fn run(&self) -> Output {
        self.command()
            .arg("--config")
            .arg(&self.config)
            .arg("--output")
            .arg(&self.output)
            .output()
            .unwrap()
    }

    fn assert_rejected(&self, expected: &str) {
        let result = self.run();
        assert!(!result.status.success());
        let error = String::from_utf8(result.stderr).unwrap();
        assert!(error.contains(expected), "{error}");
        assert!(!error.contains(&self.key.private().to_prefixed_string()));
    }

    fn verify(&self, path: &Path) {
        let text = fs::read_to_string(path).unwrap();
        let token = Biscuit::from_base64(&text, self.key.public()).unwrap();
        assert_eq!(token.block_count(), 1);
        let source = token.print_block_source(0).unwrap();
        let mut facts: Vec<_> = source
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect();
        facts.sort();
        assert_eq!(
            facts,
            ["read(\"lsp\");", "read(\"node\");", "write(\"lsp\");"]
        );
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(Biscuit::from_base64(text, KeyPair::new().public()).is_err());
    }
}

#[test]
fn offline_signing_and_repeated_creation_preserve_existing_data() {
    let fixture = Fixture::new();
    let account = fixture
        .temp
        .path()
        .join(".state/fiber-lsp-client/default/account.json");
    fs::create_dir_all(account.parent().unwrap()).unwrap();
    fs::write(&account, b"existing account").unwrap();
    let result = fixture.run();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    fixture.verify(&fixture.output);
    let text = fs::read_to_string(&fixture.output).unwrap();
    let stdout = String::from_utf8(result.stdout).unwrap();
    assert!(stdout.contains("read(\"node\")"));
    assert!(stdout.contains(&fixture.config.display().to_string()));
    assert!(stdout.contains(&fixture.output.display().to_string()));
    assert!(!stdout.contains(&text));
    fixture.assert_rejected("目标可能已存在");
    assert_eq!(fs::read_to_string(&fixture.output).unwrap(), text);
    assert_eq!(fs::read(&account).unwrap(), b"existing account");
}

#[test]
fn defaults_follow_xdg_and_home_without_client_configuration() {
    let fixture = Fixture::new();
    for (xdg, base) in [
        (Some("data"), "data"),
        (None, "home/.local/share"),
        (Some(""), "home/.local/share"),
    ] {
        let config = fixture
            .temp
            .path()
            .join(base)
            .join("offckb-nodejs/devnet/fiber/nodes/3/config.yml");
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        fs::copy(&fixture.config, &config).unwrap();
        let out = fixture
            .temp
            .path()
            .join(format!("token-{}", xdg.unwrap_or("unset")));
        let mut command = fixture.command();
        command
            .env("HOME", fixture.temp.path().join("home"))
            .env_remove("XDG_DATA_HOME");
        if let Some(xdg) = xdg {
            command.env(
                "XDG_DATA_HOME",
                if xdg.is_empty() {
                    PathBuf::new()
                } else {
                    fixture.temp.path().join(xdg)
                },
            );
        }
        let result = command.arg("--output").arg(&out).output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        fixture.verify(&out);
        assert!(
            String::from_utf8(result.stdout)
                .unwrap()
                .contains(&config.display().to_string())
        );
    }
}

#[test]
fn overrides_use_cwd_and_do_not_fallback_from_bad_config() {
    let fixture = Fixture::new();
    fs::write(
        &fixture.config,
        format!(
            "rpc:\n  biscuit_public_key: {}\n  biscuit_private_key_path: nonexistent\n",
            fixture.key.public()
        ),
    )
    .unwrap();
    let result = fixture
        .command()
        .args([
            "--config",
            "config.yml",
            "--private-key-file",
            "private-key",
            "--output",
            "override-token",
        ])
        .output()
        .unwrap();
    assert!(result.status.success());
    fixture.verify(&fixture.temp.path().join("override-token"));
    fs::remove_file(&fixture.config).unwrap();
    fixture.assert_rejected("无法读取配置");
    assert!(!fixture.output.exists());
}

#[test]
fn invalid_configuration_and_keys_are_rejected_without_echoing_source() {
    let fixture = Fixture::new();
    let original = fs::read(&fixture.config).unwrap();
    for text in [
        "rpc: {}",
        "rpc: [secret-in-yaml",
        "rpc:\n  biscuit_public_key: secret-in-yaml\n",
    ] {
        fs::write(&fixture.config, text).unwrap();
        let result = fixture.run();
        assert!(!result.status.success());
        assert!(!String::from_utf8_lossy(&result.stderr).contains("secret-in-yaml"));
    }
    fs::write(
        &fixture.config,
        format!("rpc:\n  biscuit_public_key: {}\n", fixture.key.public()),
    )
    .unwrap();
    fixture.assert_rejected("--private-key-file");
    let result = fixture
        .command()
        .arg("--config")
        .arg(&fixture.config)
        .arg("--private-key-file")
        .arg(&fixture.private)
        .arg("--output")
        .arg(&fixture.output)
        .output()
        .unwrap();
    assert!(result.status.success());
    fs::remove_file(&fixture.output).unwrap();
    fs::write(&fixture.config, original).unwrap();
    fs::write(&fixture.private, "secret-invalid-private-key").unwrap();
    fixture.assert_rejected("私钥格式无效");
    assert!(!String::from_utf8_lossy(&fixture.run().stderr).contains("secret-invalid-private-key"));
    fs::write(
        &fixture.private,
        KeyPair::new().private().to_prefixed_string(),
    )
    .unwrap();
    fixture.assert_rejected("不匹配");
    assert!(!fixture.output.exists());
}

#[test]
fn unsafe_private_files_and_symlink_outputs_are_rejected() {
    let fixture = Fixture::new();
    fs::set_permissions(&fixture.private, fs::Permissions::from_mode(0o644)).unwrap();
    fixture.assert_rejected("chmod 600");
    assert_eq!(
        fs::metadata(&fixture.private).unwrap().permissions().mode() & 0o777,
        0o644
    );
    let real = fixture.temp.path().join("real-key");
    fs::rename(&fixture.private, &real).unwrap();
    fs::set_permissions(&real, fs::Permissions::from_mode(0o600)).unwrap();
    symlink(&real, &fixture.private).unwrap();
    fixture.assert_rejected("符号链接");
    fs::remove_file(&fixture.private).unwrap();
    fs::rename(&real, &fixture.private).unwrap();
    fs::create_dir_all(fixture.output.parent().unwrap()).unwrap();
    let target = fixture.temp.path().join("target");
    fs::write(&target, b"preserve").unwrap();
    symlink(&target, &fixture.output).unwrap();
    fixture.assert_rejected("目标可能已存在");
    assert_eq!(fs::read(&target).unwrap(), b"preserve");
    fs::remove_file(&target).unwrap();
    fixture.assert_rejected("目标可能已存在");
    assert!(
        fs::symlink_metadata(&fixture.output)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(!target.exists());
}

#[test]
fn simultaneous_creators_only_publish_one_complete_token() {
    let fixture = Fixture::new();
    let spawn = || {
        fixture
            .command()
            .arg("--config")
            .arg(&fixture.config)
            .arg("--output")
            .arg(&fixture.output)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap()
    };
    let mut first = spawn();
    let mut second = spawn();
    assert_ne!(
        first.wait().unwrap().success(),
        second.wait().unwrap().success()
    );
    fixture.verify(&fixture.output);
}

#[test]
fn help_requires_no_configuration_or_accounts() {
    let temp = tempfile::tempdir().unwrap();
    for args in [
        vec![],
        vec!["--help"],
        vec!["admin", "--help"],
        vec!["admin", "init-lsp", "--help"],
        vec!["admin", "create-token", "--help"],
        vec!["create-channel", "--help"],
        vec!["admin", "init-accounts", "--help"],
        vec!["status", "--help"],
    ] {
        let result = Command::new(env!("CARGO_BIN_EXE_fiber-lsp-client"))
            .current_dir(temp.path())
            .env_remove("HOME")
            .env_remove("XDG_DATA_HOME")
            .args(args)
            .output()
            .unwrap();
        assert!(result.status.success());
        assert!(!result.stdout.is_empty());
        assert!(result.stderr.is_empty());
    }
}

#[test]
fn absolute_private_path_is_independent_of_config_and_cwd() {
    let fixture = Fixture::new();
    fs::write(
        &fixture.config,
        format!(
            "rpc:\n  biscuit_public_key: {}\n  biscuit_private_key_path: {}\n",
            fixture.key.public(),
            fixture.private.display()
        ),
    )
    .unwrap();
    let configs = fixture.temp.path().join("config-directory");
    fs::create_dir(&configs).unwrap();
    let moved = configs.join("config.yml");
    fs::rename(&fixture.config, &moved).unwrap();
    let result = fixture
        .command()
        .current_dir(&configs)
        .arg("--config")
        .arg(&moved)
        .arg("--output")
        .arg(&fixture.output)
        .output()
        .unwrap();
    assert!(result.status.success());
    fixture.verify(&fixture.output);
}

#[test]
fn missing_and_non_regular_private_files_are_rejected() {
    let fixture = Fixture::new();
    fs::remove_file(&fixture.private).unwrap();
    fixture.assert_rejected("无法读取私钥");
    fs::create_dir(&fixture.private).unwrap();
    fixture.assert_rejected("普通文件");
    assert!(!fixture.output.exists());
}
