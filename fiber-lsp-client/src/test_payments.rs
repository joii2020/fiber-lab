use super::*;
use crate::account::{
    test_registered,
    tests::{fixture, serve},
};
use fiber_types::Privkey;
use std::fs;

#[test]
fn receipt_timing_starts_only_for_its_own_inbound_tlc_and_never_restarts() {
    crate::timings::enable(true);
    let hash = Hash256([1; 32]);
    let mut receipt = ReceiptTiming::new(hash);
    receipt.observe_tlc(Hash256([2; 32]), true);
    receipt.observe_tlc(hash, false);
    assert!(receipt.processing.is_none());
    assert_eq!(crate::timings::calls("payment.wait_for_receipt"), 0);
    receipt.observe_tlc(hash, true);
    assert!(receipt.processing.is_some());
    assert!(receipt.waiting.is_none());
    assert_eq!(crate::timings::calls("payment.wait_for_receipt"), 1);
    receipt.observe_tlc(hash, true);
    assert_eq!(crate::timings::calls("payment.receipt_processing"), 0);
    receipt.finish(true);
    assert_eq!(crate::timings::calls("payment.receipt_processing"), 1);
    crate::timings::enable(false);
}

async fn operation(account: &mut ClientAccount, inbound: bool) -> Operation {
    // Bind a real SDK-derived channel key for the local record tests. Tests
    // which sign commitments use the SDK's own cryptographic fixtures.
    account.session.allocate_pending_channel().await.unwrap();
    let id = fiber_types::Hash256::from([4; 32]);
    let mut state = account.session.state().clone();
    state
        .bindings
        .insert(id, account.session.pending_channel_key_id().unwrap());
    // HostedSession owns the root; reload it with the persisted directory.
    crate::account::save_test_state(&account.dir, &state);
    let preimage = Hash256([5; 32]);
    let hash = Hash256(HashAlgorithm::CkbHash.hash(preimage.0));
    let expires = now_ms().unwrap() + 3_600_000;
    Operation {
        hash,
        channel_id: id.into(),
        inbound,
        amount: u128::from(CKB),
        authorization: authorization(
            hash,
            inbound,
            u128::from(CKB) + if inbound { 0 } else { 500_000 },
            HashAlgorithm::CkbHash,
            expires,
        )
        .unwrap(),
        invoice: None,
        preimage: inbound.then_some(preimage),
        expiry_seconds: Some(3600),
        release_started: false,
        received: false,
        succeeded: false,
        complete: false,
        pending_signature: None,
        start_versions: [1, 1],
    }
}
fn signed_invoice(account: &ClientAccount, op: &Operation) -> InvoiceResult {
    let mut parsed = CkbInvoice {
        currency: Currency::Fibd,
        amount: Some(op.amount),
        signature: None,
        data: fiber_types::InvoiceData {
            timestamp: now_ms().unwrap().into(),
            payment_hash: op.hash.into(),
            attrs: vec![
                fiber_types::Attribute::ExpiryTime(Duration::from_secs(3600)),
                fiber_types::Attribute::HashAlgorithm(HashAlgorithm::CkbHash),
                fiber_types::Attribute::TrampolineRouteHint(
                    secp256k1::PublicKey::from_slice(&account.lsp_public_key.0).unwrap(),
                ),
            ],
        },
    };
    // The test account's root backup is the registered invoice key.
    let key = crate::store::read_private(&account.dir.path.join("root-key")).unwrap();
    let key = secp256k1::SecretKey::from_slice(&key).unwrap();
    parsed
        .data
        .attrs
        .push(fiber_types::Attribute::PayeePublicKey(
            Privkey::from(key.secret_bytes()).pubkey().into(),
        ));
    parsed
        .update_signature(|message| secp256k1::SECP256K1.sign_ecdsa_recoverable(message, &key))
        .unwrap();
    InvoiceResult {
        invoice_address: parsed.to_string(),
        invoice: parsed.into(),
    }
}
async fn fixture_operation(config: &Connection, inbound: bool) -> (ClientAccount, Operation) {
    let mut account = test_registered(config).await;
    let op = operation(&mut account, inbound).await;
    drop(account);
    (ClientAccount::load(config, None).await.unwrap(), op)
}

#[tokio::test]
async fn lost_send_response_exits_without_retry_or_payment_file() {
    let (_root, config, listener) = fixture();
    let (account, mut op) = fixture_operation(&config, false).await;
    op.invoice = Some(signed_invoice(&account, &op).invoice_address);
    let hash = op.hash;
    let server = serve(
        listener,
        vec![
            (
                "get_payment",
                json!({"error":{"code":-32000,"message":format!("InvalidParameter: Payment session not found: {hash}")}}),
            ),
            (
                "send_payment",
                json!({"error":{"code":-1,"message":"lost response containing secret-token"}}),
            ),
        ],
    );
    let rpc = Rpc::new(&config.rpc_url).unwrap();
    assert!(send_payment(&account, &rpc, &op).await.is_err());
    server.join().unwrap();
    assert!(!account.dir.path.join("payments.json").exists());
}

#[tokio::test]
async fn creates_invoice_in_memory_and_leaves_old_payment_file_untouched() {
    let (_root, config, listener) = fixture();
    let (account, mut op) = fixture_operation(&config, true).await;
    let path = account.dir.path.join("payments.json");
    let old = b"old payment data: do not read, parse or overwrite";
    fs::write(&path, old).unwrap();
    let before = fs::read(account.dir.path.join("account.json")).unwrap();
    let result = signed_invoice(&account, &op);
    let server = serve(
        listener,
        vec![("new_invoice", serde_json::to_value(&result).unwrap())],
    );
    let rpc = Rpc::new(&config.rpc_url).unwrap();
    create_remote_invoice(&account, &rpc, &mut op)
        .await
        .unwrap();
    server.join().unwrap();
    assert_eq!(op.invoice.as_deref(), Some(result.invoice_address.as_str()));
    assert_eq!(fs::read(path).unwrap(), old);
    assert_eq!(
        fs::read(account.dir.path.join("account.json")).unwrap(),
        before
    );
}

#[tokio::test]
async fn malformed_created_invoice_is_rejected_without_creating_payment_file() {
    let (_root, config, listener) = fixture();
    let (account, mut op) = fixture_operation(&config, true).await;
    let preimage = op.preimage;
    let mut result = signed_invoice(&account, &op);
    result.invoice.amount = Some(2 * u128::from(CKB));
    let server = serve(
        listener,
        vec![("new_invoice", serde_json::to_value(result).unwrap())],
    );
    let rpc = Rpc::new(&config.rpc_url).unwrap();
    assert!(
        create_remote_invoice(&account, &rpc, &mut op)
            .await
            .is_err()
    );
    server.join().unwrap();
    assert!(op.invoice.is_none());
    assert_eq!(op.preimage, preimage);
    assert!(!account.dir.path.join("payments.json").exists());
}

#[test]
fn online_commands_only_accept_current_user_selection() {
    use clap::{CommandFactory, Parser};
    for args in [
        vec!["lsp-cli", "pay", "invoice"],
        vec!["lsp-cli", "create-invoice", "--amount", "1"],
    ] {
        assert!(crate::Cli::try_parse_from(&args).is_err());
        for user in ["0", "5"] {
            let mut invalid = args.clone();
            invalid.extend(["-u", user]);
            assert!(crate::Cli::try_parse_from(invalid).is_err());
        }
        let mut current = args.clone();
        current.extend(["-u", "4", "--rpc-url", "http://localhost:21716"]);
        assert!(crate::Cli::try_parse_from(&current).is_ok());
        let mut historical = current;
        historical.extend(["--round", "1"]);
        assert!(crate::Cli::try_parse_from(historical).is_err());
        let mut legacy = args.clone();
        legacy.extend(["--account", "default"]);
        assert!(crate::Cli::try_parse_from(legacy).is_err());
        let mut mixed = args;
        mixed.extend(["-u", "1", "--account", "default"]);
        assert!(crate::Cli::try_parse_from(mixed).is_err());
    }
    let mut command = crate::Cli::command();
    for name in ["pay", "create-invoice"] {
        let help = command
            .find_subcommand_mut(name)
            .unwrap()
            .render_long_help()
            .to_string();
        assert!(!help.contains("--account"));
        assert!(!help.contains("--round"));
    }
}

#[test]
fn cli_uses_fiber_defaults_and_always_receives_online() {
    use clap::Parser;
    let parse = |args: &[&str]| {
        crate::Cli::try_parse_from(
            args.iter()
                .copied()
                .chain(["--rpc-url", "http://127.0.0.1:21716"]),
        )
    };
    assert!(parse(&["lsp-cli", "pay", "invoice"]).is_err());
    assert!(parse(&["lsp-cli", "pay", "invoice", "-u", "1"]).is_ok());
    assert!(parse(&["lsp-cli", "pay", "invoice", "--max-fee", "0.01", "-u", "1"]).is_err());
    assert!(parse(&["lsp-cli", "create-invoice"]).is_err());
    assert!(
        parse(&[
            "lsp-cli",
            "create-invoice",
            "--amount",
            "1",
            "-u",
            "1",
            "--timeout",
            "1"
        ])
        .is_ok()
    );
    assert!(
        parse(&[
            "lsp-cli",
            "create-invoice",
            "--amount",
            "1",
            "-u",
            "1",
            "--wait"
        ])
        .is_err()
    );
    assert!(authorization(Hash256([1; 32]), true, 1, HashAlgorithm::CkbHash, u64::MAX).is_err());
}
#[tokio::test]
async fn payment_leaves_fee_parameters_to_fiber() {
    let (_root, config, _listener) = fixture();
    let (account, mut op) = fixture_operation(&config, false).await;
    op.invoice = Some(signed_invoice(&account, &op).invoice_address);
    let params = payment_params(&account, &op);
    assert_eq!(op.authorization.amount, op.amount + 500_000);
    assert!(params[0].get("max_fee_amount").is_none());
    assert!(params[0].get("max_fee_rate").is_none());
    assert_eq!(params[0]["max_parts"], "0x1");
    assert_eq!(
        params[0]["trampoline_hops"],
        json!([account.lsp_public_key])
    );
    assert!(params[0].get("timeout").is_none());
}

#[tokio::test]
async fn wrong_channel_signing_response_never_mutates_sdk_or_submits() {
    let (_root, config, listener) = fixture();
    let (mut account, mut op) = fixture_operation(&config, false).await;
    let server = serve(
        listener,
        vec![(
            "get_channel_signing_status",
            json!({
                "channel_id": Hash256([7;32]), "status": {"type":"NoSignatureRequired"}
            }),
        )],
    );
    let before = fs::read(account.dir.path.join("signer.bin")).unwrap();
    let rpc = Rpc::new(&config.rpc_url).unwrap();
    let error = sign(&mut account, &rpc, &mut op, None).await.unwrap_err();
    assert!(error.to_string().contains("其他通道"));
    assert_eq!(
        fs::read(account.dir.path.join("signer.bin")).unwrap(),
        before
    );
    server.join().unwrap();
}
