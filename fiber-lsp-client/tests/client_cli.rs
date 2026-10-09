use std::{fs, process::Command};

#[test]
fn client_commands_read_offckb_config_from_xdg_or_home() {
    let temp = tempfile::tempdir().unwrap();
    for (xdg, base) in [
        (Some("data"), "data"),
        (None, "home/.local/share"),
        (Some(""), "home/.local/share"),
    ] {
        let config = temp
            .path()
            .join(base)
            .join("offckb-nodejs/devnet/fiber/nodes/3/config.yml");
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        // Fail during configuration loading, before touching the real account.
        fs::write(&config, "rpc: {}\n").unwrap();
        for subcommand in ["create-user", "status", "open-channel", "close-channel"] {
            let mut command = Command::new(env!("CARGO_BIN_EXE_fiber-lsp-client"));
            command
                .current_dir(temp.path())
                .env("HOME", temp.path().join("home"))
                .env_remove("XDG_DATA_HOME")
                .arg(subcommand);
            if let Some(xdg) = xdg {
                command.env(
                    "XDG_DATA_HOME",
                    if xdg.is_empty() {
                        std::path::PathBuf::new()
                    } else {
                        temp.path().join(xdg)
                    },
                );
            }
            let result = command.output().unwrap();
            let error = String::from_utf8(result.stderr).unwrap();
            assert!(!result.status.success());
            assert!(error.contains(config.to_str().unwrap()), "{error}");
            assert!(error.contains("rpc.listening_addr"), "{error}");
        }
    }
}

#[test]
fn explicit_config_is_relative_to_cwd_and_needs_no_home() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("custom.yml");
    fs::write(&config, "rpc: {}\n").unwrap();
    for subcommand in ["create-user", "status", "open-channel", "close-channel"] {
        let result = Command::new(env!("CARGO_BIN_EXE_fiber-lsp-client"))
            .current_dir(temp.path())
            .env_remove("HOME")
            .env_remove("XDG_DATA_HOME")
            .args([subcommand, "--config", "custom.yml"])
            .output()
            .unwrap();
        let error = String::from_utf8(result.stderr).unwrap();
        assert!(!result.status.success());
        assert!(error.contains(config.to_str().unwrap()), "{error}");
        assert!(error.contains("rpc.listening_addr"), "{error}");
    }
}

#[test]
fn lifecycle_help_and_amount_validation_need_no_configuration() {
    for name in [
        "open-channel",
        "close-channel",
        "pay",
        "create-invoice",
        "watch",
        "resume",
        "payment-status",
        "invoice-status",
        "list-payments",
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_fiber-lsp-client"))
            .args([name, "--help"])
            .env_remove("HOME")
            .env_remove("XDG_DATA_HOME")
            .output()
            .unwrap();
        assert!(output.status.success());
    }
    for name in [
        "openchannel",
        "closechannel",
        "closeChannel",
        "listChannel",
        "list-channel",
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_fiber-lsp-client"))
            .args([name, "--help"])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(
            String::from_utf8(output.stderr)
                .unwrap()
                .contains("unrecognized subcommand")
        );
    }
    for amount in [
        "0",
        "-1",
        "1e3",
        "NaN",
        "1.000000001",
        "18446744073709551616",
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_fiber-lsp-client"))
            .args(["open-channel", "--funding-amount", amount])
            .env_remove("HOME")
            .env_remove("XDG_DATA_HOME")
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(
            !String::from_utf8(output.stderr)
                .unwrap()
                .contains("未设置 HOME")
        );
    }
}
