use super::*;
use crate::account::{
    test_registered,
    tests::{fixture, serve},
};
use ckb_types::core::TransactionBuilder;
use serde_json::Value;

#[test]
fn amounts_use_integer_arithmetic_and_reject_overflow_and_extra_precision() {
    assert_eq!(parse_amount("1000").unwrap(), 1000 * CKB);
    assert_eq!(parse_amount("0.00000001").unwrap(), 1);
    assert_eq!(parse_amount("184467440737.09551615").unwrap(), u64::MAX);
    for value in [
        "",
        ".1",
        "1.",
        "0",
        "+1",
        "-1",
        "1e2",
        "1.123456789",
        "1.2.3",
        "184467440737.09551616",
        " 1",
    ] {
        assert!(parse_amount(value).is_err(), "{value}");
    }
}

async fn intent(account: &mut ClientAccount) -> Intent {
    let material = account.session.allocate_pending_channel().await.unwrap();
    account.save().unwrap();
    let terms = crate::wallet::tests::terms();
    let tx = TransactionBuilder::default()
        .output(
            packed::CellOutput::new_builder()
                .capacity(1099 * CKB)
                .lock(terms.network.funding_lock.clone())
                .build(),
        )
        .output_data(packed::Bytes::default())
        .build()
        .data();
    let request: OpenChannelWithExternalFundingParams = serde_json::from_value(json!({
        "pubkey": account.lsp_public_key, "funding_amount": format!("0x{:x}",1000*CKB), "public":false,
        "shutdown_script":terms.wallet_lock, "funding_lock_script":terms.wallet_lock,
        "commitment_delay_epoch":"0x10000000001", "commitment_fee_rate":"0x3e8", "funding_fee_rate":"0x3e8",
        "external_channel_signer":fiber_lsp_sdk::json::open_material_to_rpc(&material),
    })).unwrap();
    let opening = OpenTenantChannelResult {
        channel_id: Hash256([4; 32]),
        unsigned_funding_tx: tx.into(),
        opening_context: fiber_json_types::TenantChannelOpeningContext {
            remote_funding_key: account.lsp_public_key,
            remote_settlement_key: account.lsp_public_key,
            remote_shutdown_script: terms.wallet_lock.clone(),
            commitment_delay_epoch: request.commitment_delay_epoch.unwrap(),
            commitment_fee_rate: 1000,
            local_amount: request.funding_amount,
            remote_amount: u128::from(RESERVE),
            local_reserved_ckb_amount: RESERVE,
            remote_reserved_ckb_amount: RESERVE,
        },
    };
    Intent {
        format_version: 1,
        tenant_id: account.session.tenant_id().as_str().into(),
        terms,
        request,
        stage: Stage::Ready,
        opening: Some(opening),
        signed_funding: None,
        funding_hash: None,
        funding_fee: None,
        close_authorization: None,
        close_balances: None,
        close_transaction: None,
        close_hash: None,
        pending_signature: None,
        pending_status: None,
    }
}

#[tokio::test]
async fn query_is_read_only_including_empty_and_local_pending_results() {
    let (_root, config, listener) = fixture();
    let mut account = test_registered(&config).await;
    let mut record = intent(&mut account).await;
    record.stage = Stage::Opening;
    record.opening = None;
    save_intent(&account.dir, &record).unwrap();
    let originals: Vec<_> = ["root-key", "account.json", "signer.bin", "channel.json"]
        .into_iter()
        .map(|name| (name, std::fs::read(account.dir.path.join(name)).unwrap()))
        .collect();
    drop(account);
    let server = serve(
        listener,
        vec![
            ("list_channels", json!({"channels":[]})),
            ("list_channels", json!({"channels":[]})),
        ],
    );
    let status = crate::account::run(&config, false, None).await.unwrap();
    assert_eq!(count(&status.channels), "0");
    print_details(&status.client, &status.channels)
        .await
        .unwrap();
    server.join().unwrap();
    for (name, bytes) in originals {
        assert_eq!(
            std::fs::read(config.account_dir().join(name)).unwrap(),
            bytes
        );
    }
}

#[tokio::test]
async fn restored_amount_or_tenant_mismatch_stops_before_open_rpc() {
    let (_root, config, listener) = fixture();
    let mut account = test_registered(&config).await;
    let mut record = intent(&mut account).await;
    record.opening = None;
    record.stage = Stage::Opening;
    save_intent(&account.dir, &record).unwrap();
    let before = std::fs::read(account.dir.path.join("channel.json")).unwrap();
    let server = serve(
        listener,
        vec![
            ("list_channels", json!({"channels":[]})),
            ("list_channels", json!({"channels":[]})),
        ],
    );
    let options = OpenOptions {
        account: None,
        user: None,
        config: None,
        funding_amount: Some(1001 * CKB),
        timeout: 1,
    };
    let error = open_inner(&options, &mut account, &Rpc::new(&config.rpc_url).unwrap())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("金额与原开通意图不一致"));
    assert_eq!(
        std::fs::read(account.dir.path.join("channel.json")).unwrap(),
        before
    );
    record.tenant_id = "other-tenant".into();
    assert!(validate_owner(&account, &record).is_err());
    server.join().unwrap();
}

fn close_tx(record: &Intent, wallet: &Wallet) -> packed::Transaction {
    let opening = record.opening.as_ref().unwrap();
    let tx: packed::Transaction = opening.unsigned_funding_tx.clone().into();
    let fee = wallet
        .close_fee(
            opening
                .opening_context
                .remote_shutdown_script
                .clone()
                .into(),
        )
        .unwrap();
    TransactionBuilder::default()
        .cell_deps(
            record
                .terms
                .funding_deps
                .iter()
                .cloned()
                .map(Into::into)
                .collect::<Vec<packed::CellDep>>(),
        )
        .input(
            packed::CellInput::new_builder()
                .previous_output(
                    packed::OutPoint::new_builder()
                        .tx_hash(tx.calc_tx_hash())
                        .index(0u32)
                        .build(),
                )
                .build(),
        )
        .output(
            packed::CellOutput::new_builder()
                .capacity(1000 * CKB - fee)
                .lock(record.terms.wallet_lock.clone())
                .build(),
        )
        .output_data(packed::Bytes::default())
        .output(
            packed::CellOutput::new_builder()
                .capacity(RESERVE)
                .lock(
                    record
                        .opening
                        .as_ref()
                        .unwrap()
                        .opening_context
                        .remote_shutdown_script
                        .clone(),
                )
                .build(),
        )
        .output_data(packed::Bytes::default())
        .build()
        .data()
}

#[tokio::test]
async fn separate_wallet_close_returns_each_balance_to_original_wallet() {
    let (_root, config, _listener) = fixture();
    let mut account = test_registered(&config).await;
    let mut record = intent(&mut account).await;
    let local: packed::Script = record.terms.wallet_lock.clone().into();
    let remote = local.as_builder().args([8u8; 20].pack()).build();
    record.terms.remote_wallet_lock = Some(remote.clone().into());
    record
        .opening
        .as_mut()
        .unwrap()
        .opening_context
        .remote_shutdown_script = remote.clone().into();
    let wallet = Wallet::from_terms(record.terms.clone()).unwrap();
    let fee = wallet.close_fee(remote).unwrap();
    record.close_authorization = Some(CloseAuthorization {
        fee,
        local_fee_share: fee,
        remote_fee_share: 0,
    });
    let tx = close_tx(&record, &wallet);
    validate_close_tx(&tx, &record, &wallet).unwrap();
    let bad = tx
        .clone()
        .into_view()
        .as_advanced_builder()
        .set_outputs(vec![
            tx.raw().outputs().get(0).unwrap(),
            tx.raw()
                .outputs()
                .get(1)
                .unwrap()
                .as_builder()
                .lock(record.terms.wallet_lock.clone())
                .build(),
        ])
        .build()
        .data();
    assert!(validate_close_tx(&bad, &record, &wallet).is_err());
}

#[tokio::test]
async fn close_after_payment_uses_persisted_settled_balances() {
    let (_root, config, _listener) = fixture();
    let mut account = test_registered(&config).await;
    let mut record = intent(&mut account).await;
    let wallet = Wallet::from_terms(record.terms.clone()).unwrap();
    let fee = wallet
        .close_fee(record.terms.wallet_lock.clone().into())
        .unwrap();
    record.close_authorization = Some(CloseAuthorization {
        fee,
        local_fee_share: fee,
        remote_fee_share: 0,
    });
    record.close_balances = Some((u128::from(999 * CKB), u128::from(100 * CKB)));
    let original = close_tx(&record, &wallet);
    // One CKB moved to the peer; both destinations remain the original locks.
    let updated = original
        .clone()
        .into_view()
        .as_advanced_builder()
        .set_outputs(vec![
            original
                .raw()
                .outputs()
                .get(0)
                .unwrap()
                .as_builder()
                .capacity(999 * CKB - fee)
                .build(),
            original
                .raw()
                .outputs()
                .get(1)
                .unwrap()
                .as_builder()
                .capacity(100 * CKB)
                .build(),
        ])
        .build()
        .data();
    validate_close_tx(&updated, &record, &wallet).unwrap();
    assert!(validate_close_tx(&original, &record, &wallet).is_err());
    save_intent(&account.dir, &record).unwrap();
    let restored = read_intent(&account.dir).unwrap().unwrap();
    assert_eq!(restored.close_balances, record.close_balances);
    validate_close_tx(&updated, &restored, &wallet).unwrap();
}

#[tokio::test]
async fn close_refund_and_fee_allocation_are_independently_verified() {
    let (_root, config, _listener) = fixture();
    let mut account = test_registered(&config).await;
    let mut record = intent(&mut account).await;
    let wallet = Wallet::from_terms(record.terms.clone()).unwrap();
    let fee = wallet
        .close_fee(record.terms.wallet_lock.clone().into())
        .unwrap();
    record.close_authorization = Some(CloseAuthorization {
        fee,
        local_fee_share: fee,
        remote_fee_share: 0,
    });
    let tx = close_tx(&record, &wallet);
    validate_close_tx(&tx, &record, &wallet).unwrap();
    for bad in [
        tx.clone()
            .into_view()
            .as_advanced_builder()
            .set_outputs(vec![
                tx.raw()
                    .outputs()
                    .get(0)
                    .unwrap()
                    .as_builder()
                    .capacity(1000 * CKB - fee - 1)
                    .build(),
                tx.raw().outputs().get(1).unwrap(),
            ])
            .build()
            .data(),
        tx.clone()
            .into_view()
            .as_advanced_builder()
            .set_outputs(vec![
                tx.raw()
                    .outputs()
                    .get(0)
                    .unwrap()
                    .as_builder()
                    .lock(packed::Script::default())
                    .build(),
                tx.raw().outputs().get(1).unwrap(),
            ])
            .build()
            .data(),
        tx.clone()
            .into_view()
            .as_advanced_builder()
            .set_cell_deps(vec![])
            .build()
            .data(),
    ] {
        assert!(validate_close_tx(&bad, &record, &wallet).is_err());
    }
    record.close_authorization = Some(CloseAuthorization {
        fee,
        local_fee_share: fee - 1,
        remote_fee_share: 1,
    });
    assert!(validate_close_tx(&tx, &record, &wallet).is_err());
}

fn status(record: &Intent) -> ChannelSigningStatus {
    ChannelSigningStatus::SignatureRequired {
        request_id: Hash256([8; 32]),
        transition: ChannelSigningTransition::SendCommitmentSigned,
        content: fiber_json_types::Musig2SigningContent {
            slot: fiber_json_types::NonceSlot {
                purpose: fiber_json_types::NoncePurpose::Commitment,
                commitment_number: 1,
            },
            commitment_counter: Some(fiber_json_types::CommitmentCounter::Local),
            key_agg_ctx: vec![],
            agg_nonce: vec![],
            content: fiber_json_types::Musig2SignableContent::CommitmentTransaction {
                transaction: record.opening.as_ref().unwrap().unsigned_funding_tx.clone(),
            },
        },
        session_evidence: fiber_json_types::SigningSessionEvidence {
            peer_public_nonce: vec![],
            peer_partial_signature: None,
        },
        settlement: Some(fiber_json_types::SigningSettlement {
            local_amount: record.request.funding_amount,
            remote_amount: u128::from(record.terms.remote_amount),
            local_settlement_pubkey: record.request.pubkey,
            remote_settlement_pubkey: record.request.pubkey,
            for_remote: true,
            tlcs: vec![],
        }),
    }
}

#[tokio::test]
async fn lost_signature_response_reuses_exact_saved_submission_and_rejects_changed_payload() {
    let (_root, config, listener) = fixture();
    let mut account = test_registered(&config).await;
    let mut record = intent(&mut account).await;
    record.pending_signature = Some(SubmitChannelSignatureParams {
        channel_id: record.opening.as_ref().unwrap().channel_id,
        request_id: Hash256([8; 32]),
        partial_signature: [1; 32],
        next_material: None,
    });
    record.pending_status = Some(status(&record));
    save_intent(&account.dir, &record).unwrap();
    let id = record.opening.as_ref().unwrap().channel_id;
    let response = json!({"channel_id":id,"status":record.pending_status});
    let mut changed = status(&record);
    if let ChannelSigningStatus::SignatureRequired { content, .. } = &mut changed {
        content.agg_nonce = vec![9; 66];
    }
    let server = serve(
        listener,
        vec![
            ("get_channel_signing_status", response.clone()),
            (
                "submit_channel_signature",
                json!({"error":{"code":-1,"message":"secret-token"}}),
            ),
            ("get_channel_signing_status", response),
            ("submit_channel_signature", Value::Null),
            (
                "get_channel_signing_status",
                json!({"channel_id":id,"status":changed}),
            ),
        ],
    );
    let rpc = Rpc::new(&config.rpc_url).unwrap();
    let wallet = Wallet::from_terms(record.terms.clone()).unwrap();
    let before = std::fs::read(account.dir.path.join("signer.bin")).unwrap();
    let error = sign_status(&mut account, &rpc, &wallet, &mut record, false)
        .await
        .unwrap_err();
    assert!(!format!("{error:#}").contains("secret-token"));
    // Simulate reopening the persisted intent after the response was lost.
    let mut restored = read_intent(&account.dir).unwrap().unwrap();
    sign_status(&mut account, &rpc, &wallet, &mut restored, false)
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(account.dir.path.join("signer.bin")).unwrap(),
        before
    );
    assert!(
        sign_status(&mut account, &rpc, &wallet, &mut restored, false)
            .await
            .unwrap_err()
            .to_string()
            .contains("签名内容改变")
    );
    server.join().unwrap();
}
#[tokio::test]
async fn lost_opening_response_retains_pending_signer_and_original_request() {
    let (_root, config, listener) = fixture();
    let mut account = test_registered(&config).await;
    let mut record = intent(&mut account).await;
    record.opening = None;
    record.stage = Stage::Opening;
    record.terms.ckb_rpc_url = config.rpc_url.clone();
    save_intent(&account.dir, &record).unwrap();
    let pending = account.session.pending_channel_key_id();
    let root = std::fs::read(account.dir.path.join("root-key")).unwrap();
    let server = serve(
        listener,
        vec![
            ("list_channels", json!({"channels":[]})),
            ("list_channels", json!({"channels":[]})),
            ("get_block_hash", json!(record.terms.genesis_hash)),
            (
                "open_tenant_channel",
                json!({"error":{"code":-1,"message":"response lost"}}),
            ),
            ("list_channels", json!({"channels":[]})),
            ("list_channels", json!({"channels":[]})),
            ("get_block_hash", json!(record.terms.genesis_hash)),
            (
                "open_tenant_channel",
                json!({"error":{"code":-1,"message":"response lost"}}),
            ),
        ],
    );
    let rpc = Rpc::new(&config.rpc_url).unwrap();
    let options = OpenOptions {
        account: None,
        user: None,
        config: None,
        funding_amount: None,
        timeout: 1,
    };
    assert!(open_inner(&options, &mut account, &rpc).await.is_err());
    let original_request = account.session.state().opening_request.clone();
    assert!(original_request.is_some());
    drop(account);
    let mut restored = ClientAccount::load(&config, None).await.unwrap();
    assert!(open_inner(&options, &mut restored, &rpc).await.is_err());
    assert_eq!(restored.session.pending_channel_key_id(), pending);
    assert_eq!(restored.session.state().opening_request, original_request);
    assert_eq!(
        std::fs::read(restored.dir.path.join("root-key")).unwrap(),
        root
    );
    assert!(
        read_intent(&restored.dir)
            .unwrap()
            .unwrap()
            .opening
            .is_none()
    );
    server.join().unwrap();
}

#[tokio::test]
async fn chain_projection_distinguishes_unknown_pending_committed_and_rejected_funding() {
    let (_root, config, listener) = fixture();
    let server = serve(
        listener,
        ["unknown", "pending", "proposed", "committed", "rejected"]
            .into_iter()
            .map(|state| {
                (
                    "get_transaction",
                    json!({"transaction":null,"tx_status":{"status":state}}),
                )
            })
            .collect(),
    );
    let mut terms = crate::wallet::tests::terms();
    terms.ckb_rpc_url = config.rpc_url;
    let wallet = Wallet::from_terms(terms).unwrap();
    let hash = [5; 32].into();
    assert!(!wallet.funding_known(&hash).await.unwrap());
    for _ in 0..3 {
        assert!(wallet.funding_known(&hash).await.unwrap());
    }
    assert!(wallet.funding_known(&hash).await.is_err());
    server.join().unwrap();
}
#[tokio::test]
async fn lost_funding_response_checks_chain_and_never_resubmits_known_transaction() {
    let (_root, config, listener) = fixture();
    let mut account = test_registered(&config).await;
    let mut record = intent(&mut account).await;
    let (mut terms, tx, cells) = crate::wallet::tests::fixture();
    terms.ckb_rpc_url = config.rpc_url.clone();
    record.terms = terms;
    record.opening.as_mut().unwrap().unsigned_funding_tx = tx.clone().into();
    record.funding_hash = Some(tx.calc_tx_hash().unpack());
    record.signed_funding = Some(tx.clone().into());
    record.stage = Stage::FundingSaved;
    save_intent(&account.dir, &record).unwrap();
    let original = std::fs::read(account.dir.path.join("channel.json")).unwrap();
    let live = |cell: &packed::CellOutput| json!({"status":"live","cell":{"output":ckb_jsonrpc_types::CellOutput::from(cell.clone()),"data":{"content":"0x","hash":format!("0x{}", "00".repeat(32))}}});
    let expected = json!([SubmitSignedFundingTxParams {
        channel_id: record.opening.as_ref().unwrap().channel_id,
        signed_funding_tx: record.signed_funding.clone().unwrap()
    }]);
    let server = serve(
        listener,
        vec![
            ("get_transaction", json!({"tx_status":{"status":"unknown"}})),
            ("get_live_cell", live(&cells[0].0)),
            ("get_live_cell", live(&cells[1].0)),
            (
                "submit_signed_funding_tx",
                json!({"_expect_params":expected,"_reply":{"error":{"code":-1,"message":"response lost"}}}),
            ),
            ("get_transaction", json!({"tx_status":{"status":"pending"}})),
        ],
    );
    let rpc = Rpc::new(&config.rpc_url).unwrap();
    let wallet = Wallet::from_terms(record.terms.clone()).unwrap();
    assert!(
        submit_funding(&account, &rpc, &wallet, &mut record, true)
            .await
            .is_err()
    );
    assert_eq!(
        std::fs::read(account.dir.path.join("channel.json")).unwrap(),
        original
    );
    let mut restored = read_intent(&account.dir).unwrap().unwrap();
    submit_funding(&account, &rpc, &wallet, &mut restored, true)
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(account.dir.path.join("channel.json")).unwrap(),
        original
    );
    server.join().unwrap();
}

fn channel_projection(record: &Intent) -> Channel {
    serde_json::from_value(json!({
        "channel_id":record.opening.as_ref().unwrap().channel_id,"is_public":false,"is_acceptor":false,"is_one_way":false,
        "channel_outpoint":null,"pubkey":record.request.pubkey,"funding_udt_type_script":null,"state":{"state_name":"ChannelReady"},
        "local_balance":format!("0x{:x}", 901*CKB),"remote_balance":"0x0","offered_tlc_balance":"0x0","received_tlc_balance":"0x0","pending_tlcs":[],
        "latest_commitment_transaction_hash":null,"created_at":"0x1","enabled":true,"tlc_expiry_delta":"0x0","tlc_fee_proportional_millionths":"0x0","shutdown_transaction_hash":null,"failure_detail":null,
    })).unwrap()
}

#[tokio::test]
async fn channel_count_distinguishes_missing_open_and_closed() {
    let (_root, config, _listener) = fixture();
    let mut account = test_registered(&config).await;
    let record = intent(&mut account).await;
    let mut channel = channel_projection(&record);
    assert_eq!(count(&[]), "0");
    assert_eq!(count(std::slice::from_ref(&channel)), "1");
    channel.state = ChannelState::Closed(fiber_json_types::CloseFlags::COOPERATIVE.into());
    assert_eq!(count(&[channel]), "1（已关闭）");
}

#[tokio::test]
async fn lost_shutdown_response_uses_remote_state_and_preserves_close_intent() {
    let (_root, config, listener) = fixture();
    let mut account = test_registered(&config).await;
    let mut record = intent(&mut account).await;
    record.stage = Stage::CloseRequested;
    record.close_authorization = Some(CloseAuthorization {
        fee: 500,
        local_fee_share: 500,
        remote_fee_share: 0,
    });
    save_intent(&account.dir, &record).unwrap();
    let original = std::fs::read(account.dir.path.join("channel.json")).unwrap();
    drop(account);
    let archived_id = crate::accounts::archive(&config.root, None).unwrap();
    let new_default = crate::accounts::select(&config.root, None, true).unwrap();
    atomic_write(&new_default.path.join("root-key"), b"new-account-marker").unwrap();
    let account = ClientAccount::load(&config, Some(&archived_id))
        .await
        .unwrap();
    assert_eq!(
        read_intent(&account.dir).unwrap().unwrap().stage,
        Stage::CloseRequested
    );
    let ready = channel_projection(&record);
    let mut shutting = ready.clone();
    shutting.state = serde_json::from_value(
        json!({"state_name":"ShuttingDown","state_flags":"OUR_SHUTDOWN_SENT"}),
    )
    .unwrap();
    let expected = json!([ShutdownChannelParams {
        channel_id: ready.channel_id,
        close_script: Some(record.terms.wallet_lock.clone()),
        fee_rate: Some(record.terms.close_fee_rate),
        force: Some(false)
    }]);
    let server = serve(
        listener,
        vec![
            (
                "shutdown_channel",
                json!({"_expect_params":expected,"_reply":{"error":{"code":-1,"message":"response lost"}}}),
            ),
            ("list_channels", json!({"channels":[shutting]})),
            ("list_channels", json!({"channels":[]})),
        ],
    );
    let rpc = Rpc::new(&config.rpc_url).unwrap();
    assert!(
        request_shutdown(&account, &rpc, &record, &ready)
            .await
            .is_err()
    );
    let restored = read_intent(&account.dir).unwrap().unwrap();
    let channels = rpc
        .list_all_channels(token(&account).unwrap())
        .await
        .unwrap();
    request_shutdown(&account, &rpc, &restored, &channels.channels[0])
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(account.dir.path.join("channel.json")).unwrap(),
        original
    );
    assert_eq!(
        std::fs::read(new_default.path.join("root-key")).unwrap(),
        b"new-account-marker"
    );
    assert!(!new_default.path.join("channel.json").exists());
    server.join().unwrap();
}

#[tokio::test]
async fn explicit_foreign_channel_is_rejected_without_signing_or_shutdown() {
    let (_root, config, listener) = fixture();
    let mut account = test_registered(&config).await;
    let record = intent(&mut account).await;
    save_intent(&account.dir, &record).unwrap();
    let server = serve(
        listener,
        vec![
            (
                "list_channels",
                json!({"channels":[channel_projection(&record)]}),
            ),
            ("list_channels", json!({"channels":[]})),
        ],
    );
    let options = CloseOptions {
        account: None,
        user: None,
        round: None,
        config: None,
        channel_id: Some(Hash256([9; 32])),
        timeout: 1,
    };
    let before = std::fs::read(account.dir.path.join("signer.bin")).unwrap();
    assert!(
        close_inner(&options, &mut account, &Rpc::new(&config.rpc_url).unwrap())
            .await
            .unwrap_err()
            .to_string()
            .contains("不属于当前账户")
    );
    assert_eq!(
        std::fs::read(account.dir.path.join("signer.bin")).unwrap(),
        before
    );
    server.join().unwrap();
}
