use std::process::Command;

#[test]
fn client_commands_reject_config_and_removed_export_command() {
    for args in [
        vec!["status", "-u", "1"],
        vec!["create-channel", "-u", "1"],
        vec!["close-channel", "-u", "1"],
        vec!["pay", "invoice", "-u", "1"],
        vec!["create-invoice", "--amount", "1", "-u", "1"],
    ] {
        let result = Command::new(env!("CARGO_BIN_EXE_fiber-lsp-client"))
            .args(args)
            .args(["--config", "server.yml"])
            .env_remove("HOME")
            .env_remove("XDG_DATA_HOME")
            .output()
            .unwrap();
        let error = String::from_utf8(result.stderr).unwrap();
        assert!(!result.status.success());
        assert!(error.contains("unexpected argument '--config'"), "{error}");
    }
    let result = Command::new(env!("CARGO_BIN_EXE_fiber-lsp-client"))
        .args(["admin", "export-client-config"])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(
        String::from_utf8(result.stderr)
            .unwrap()
            .contains("unrecognized subcommand")
    );
}

#[test]
fn lifecycle_help_and_amount_validation_need_no_configuration() {
    for name in ["create-channel", "close-channel", "pay", "create-invoice"] {
        let output = Command::new(env!("CARGO_BIN_EXE_fiber-lsp-client"))
            .args([name, "--help"])
            .env_remove("HOME")
            .env_remove("XDG_DATA_HOME")
            .output()
            .unwrap();
        assert!(output.status.success());
    }
    for name in [
        "watch",
        "resume",
        "payment-status",
        "invoice-status",
        "list-payments",
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
            .args(["create-channel", "--funding-amount", amount])
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
