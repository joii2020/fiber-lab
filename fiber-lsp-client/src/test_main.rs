use super::*;
#[test]
fn user_selection_is_required_exclusive_and_bounded() {
    for command in ["status", "create-channel", "close-channel"] {
        assert!(Cli::try_parse_from(["lsp-cli", command]).is_err());
        for user in ["0", "5"] {
            assert!(Cli::try_parse_from(["lsp-cli", command, "-u", user]).is_err());
        }
        assert!(
            Cli::try_parse_from(["lsp-cli", command, "-u", "1", "--account", "default"]).is_err()
        );
        assert!(Cli::try_parse_from(["lsp-cli", command, "-u", "4"]).is_ok());
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
        assert!(Cli::try_parse_from(["lsp-cli", old]).is_err());
    }
    assert!(Cli::try_parse_from(["lsp-cli", "accounts", "--history"]).is_ok());
    assert!(Cli::try_parse_from(["lsp-cli", "admin", "init-accounts"]).is_ok());
}
#[test]
fn restored_funding_amount_stays_optional() {
    let Some(Command::CreateChannel(options)) =
        Cli::try_parse_from(["lsp-cli", "create-channel", "-u", "1"])
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
        assert!(Cli::try_parse_from(["lsp-cli", command, "--round", "1"]).is_err());
        assert!(Cli::try_parse_from(["lsp-cli", command, "-u", "1", "--round", "0"]).is_err());
        assert!(
            Cli::try_parse_from(["lsp-cli", command, "--account", "default", "--round", "1"])
                .is_err()
        );
        assert!(Cli::try_parse_from(["lsp-cli", command, "-u", "1", "--round", "1"]).is_ok());
    }
    assert!(Cli::try_parse_from(["lsp-cli", "create-channel", "-u", "1", "--round", "1"]).is_err());
    let Some(Command::Status(options)) =
        Cli::try_parse_from(["lsp-cli", "status", "-u", "2", "--round", "3"])
            .unwrap()
            .command
    else {
        panic!()
    };
    assert_eq!(options.recovery_selector(), "-u 2 --round 3");
    assert!(options.current_only().is_err());
}
