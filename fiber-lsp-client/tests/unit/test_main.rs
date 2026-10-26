use super::*;

fn parse_with_url<const N: usize>(args: [&str; N]) -> Result<Cli, clap::Error> {
    let mut args = args.to_vec();
    args.extend(["--rpc-url", "http://127.0.0.1:21716"]);
    Cli::try_parse_from(args)
}

#[test]
fn user_selection_is_required_exclusive_and_bounded() {
    for command in ["status", "create-channel", "close-channel"] {
        assert!(parse_with_url(["lsp-cli", command]).is_err());
        for user in ["0", "5"] {
            assert!(parse_with_url(["lsp-cli", command, "-u", user]).is_err());
        }
        assert!(parse_with_url(["lsp-cli", command, "-u", "1", "--account", "default"]).is_err());
        assert!(parse_with_url(["lsp-cli", command, "-u", "4"]).is_ok());
    }
    for old in [
        "watch",
        "resume",
        "payment-status",
        "invoice-status",
        "list-payments",
        "create-user",
        "open-channel",
        "archive-account",
        "list-accounts",
    ] {
        assert!(parse_with_url(["lsp-cli", old]).is_err());
    }
    assert!(Cli::try_parse_from(["lsp-cli", "accounts", "--history"]).is_ok());
    assert!(Cli::try_parse_from(["lsp-cli", "admin", "init-accounts"]).is_ok());
}
#[test]
fn restored_funding_amount_stays_optional() {
    let Some(Command::CreateChannel(options)) =
        parse_with_url(["lsp-cli", "create-channel", "-u", "1"])
            .unwrap()
            .command
    else {
        panic!()
    };
    assert!(options.funding_amount.is_none());
}
#[test]
fn historical_round_requires_fixed_account_and_recovery_preserves_round() {
    for command in ["status", "close-channel"] {
        assert!(parse_with_url(["lsp-cli", command, "--round", "1"]).is_err());
        assert!(parse_with_url(["lsp-cli", command, "-u", "1", "--round", "0"]).is_err());
        assert!(
            parse_with_url(["lsp-cli", command, "--account", "default", "--round", "1"]).is_err()
        );
        assert!(parse_with_url(["lsp-cli", command, "-u", "1", "--round", "1"]).is_ok());
    }
    assert!(parse_with_url(["lsp-cli", "create-channel", "-u", "1", "--round", "1"]).is_err());
    let Some(Command::Status(options)) =
        parse_with_url(["lsp-cli", "status", "-u", "2", "--round", "3"])
            .unwrap()
            .command
    else {
        panic!()
    };
    assert_eq!(options.recovery_selector(), "-u 2 --round 3");
    assert!(options.current_only().is_err());
}

#[test]
fn connection_arguments_accept_url_and_token_without_server_config() {
    for args in [
        vec!["lsp-cli", "status", "-u", "1"],
        vec!["lsp-cli", "create-channel", "-u", "1"],
        vec!["lsp-cli", "close-channel", "-u", "1"],
        vec!["lsp-cli", "pay", "invoice", "-u", "1"],
        vec!["lsp-cli", "create-invoice", "--amount", "1", "-u", "1"],
    ] {
        assert!(Cli::try_parse_from(&args).is_err());
        let mut url_args = args.clone();
        url_args.extend(["--rpc-url", "https://lsp.example/rpc"]);
        assert!(Cli::try_parse_from(url_args).is_ok());
        let mut token_args = args.clone();
        token_args.extend([
            "--rpc-url",
            "https://lsp.example/rpc",
            "--token",
            "registration-token",
        ]);
        assert!(Cli::try_parse_from(&token_args).is_ok());
        token_args.extend(["--token-file", "token.txt"]);
        assert!(Cli::try_parse_from(token_args).is_err());
        let mut file_args = args;
        file_args.extend([
            "--rpc-url",
            "https://lsp.example/rpc",
            "--token-file",
            "token.txt",
        ]);
        assert!(Cli::try_parse_from(file_args).is_ok());
    }
}

#[test]
fn offline_account_initialization_takes_a_url_without_server_configuration() {
    let Some(Command::Admin {
        command: AdminCommand::InitAccounts(options),
    }) = Cli::try_parse_from(["lsp-cli", "admin", "init-accounts"])
        .unwrap()
        .command
    else {
        panic!()
    };
    assert_eq!(options.rpc_url, "http://127.0.0.1:21716");
    assert!(
        Cli::try_parse_from([
            "lsp-cli",
            "admin",
            "init-accounts",
            "--rpc-url",
            "https://lsp.example/rpc"
        ])
        .is_ok()
    );
    assert!(
        Cli::try_parse_from(["lsp-cli", "admin", "init-accounts", "--config", "node3.yml"])
            .is_err()
    );
}
