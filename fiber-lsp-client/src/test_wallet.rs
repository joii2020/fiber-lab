use super::*;
pub fn terms() -> Terms {
    let key = secp256k1::SecretKey::from_slice(&[7; 32]).unwrap();
    let hash = ckb_hash::blake2b_256(key.public_key(secp256k1::SECP256K1).serialize());
    let lock = packed::Script::new_builder()
        .code_hash(ckb_sdk::constants::SIGHASH_TYPE_HASH.pack())
        .hash_type(1u8)
        .args(hash[..20].pack())
        .build();
    Terms {
        ckb_rpc_url: "http://127.0.0.1:1".into(),
        genesis_hash: [1; 32].into(),
        network: OpeningNetwork {
            funding_lock: packed::Script::new_builder()
                .code_hash([2; 32].pack())
                .hash_type(4u8)
                .build(),
            commitment_lock: packed::Script::new_builder()
                .code_hash([3; 32].pack())
                .hash_type(4u8)
                .build(),
            commitment_cell_deps: 2,
            epoch_duration_ms: 14_400_000,
            minimum_shutdown_fee: CKB,
        },
        funding_deps: vec![
            packed::CellDep::new_builder()
                .out_point(
                    packed::OutPoint::new_builder()
                        .tx_hash([3; 32].pack())
                        .build(),
                )
                .build()
                .into(),
        ],
        default_lock_dep: packed::CellDep::default().into(),
        wallet_lock: lock.into(),
        wallet_account: CLIENT_FUNDING_ACCOUNT,
        remote_wallet_lock: None,
        remote_amount: RESERVE,
        auto_accept_min: 100 * CKB,
        max_fee: MAX_FEE,
        funding_fee_rate: FEE_RATE,
        close_fee_rate: FEE_RATE,
    }
}
pub(crate) fn fixture() -> (
    Terms,
    packed::Transaction,
    Vec<(packed::CellOutput, Vec<u8>)>,
) {
    let terms = terms();
    let lock: packed::Script = terms.wallet_lock.clone().into();
    let inputs = (0..2)
        .map(|i| {
            packed::CellInput::new_builder()
                .previous_output(
                    packed::OutPoint::new_builder()
                        .tx_hash([1; 32].pack())
                        .index(i as u32)
                        .build(),
                )
                .build()
        })
        .collect();
    let cells = vec![
        (
            packed::CellOutput::new_builder()
                .lock(lock.clone())
                .capacity(1000 * CKB)
                .build(),
            vec![]
        );
        2
    ];
    let tx = TransactionBuilder::default()
        .set_inputs(inputs)
        .cell_dep(terms.default_lock_dep.clone())
        .output(
            packed::CellOutput::new_builder()
                .lock(
                    terms
                        .network
                        .funding_lock
                        .clone()
                        .as_builder()
                        .args([5u8; 20].pack())
                        .build(),
                )
                .capacity(1099 * CKB)
                .build(),
        )
        .output_data(packed::Bytes::default())
        .output(
            packed::CellOutput::new_builder()
                .lock(lock)
                .capacity(901 * CKB - 1000)
                .build(),
        )
        .output_data(packed::Bytes::default())
        .witness(
            packed::WitnessArgs::new_builder()
                .input_type(Some(ckb_types::bytes::Bytes::from_static(b"preserve")))
                .build()
                .as_bytes()
                .pack(),
        )
        .witness(packed::Bytes::default())
        .build()
        .data();
    (terms, tx, cells)
}
#[test]
fn wallet_rejects_unapproved_spending_assets_inputs_change_and_deps() {
    let (terms, tx, cells) = fixture();
    assert_eq!(
        validate_funding(&tx, &cells, 1000u128 * u128::from(CKB), &terms).unwrap(),
        1000
    );
    let mut wrong_terms = terms.clone();
    wrong_terms.remote_amount += CKB;
    assert!(validate_funding(&tx, &cells, 1000u128 * u128::from(CKB), &wrong_terms).is_err());
    wrong_terms = terms.clone();
    wrong_terms.max_fee = 999;
    assert!(validate_funding(&tx, &cells, 1000u128 * u128::from(CKB), &wrong_terms).is_err());
    let mut wrong_cells = cells.clone();
    wrong_cells[1].1 = vec![1];
    assert!(validate_funding(&tx, &wrong_cells, 1000u128 * u128::from(CKB), &terms).is_err());
    wrong_cells = cells.clone();
    wrong_cells[1].0 = wrong_cells[1]
        .0
        .clone()
        .as_builder()
        .lock(packed::Script::default())
        .build();
    assert!(validate_funding(&tx, &wrong_cells, 1000u128 * u128::from(CKB), &terms).is_err());
    for bad in [
        tx.clone()
            .into_view()
            .as_advanced_builder()
            .set_cell_deps(vec![])
            .build()
            .data(),
        tx.clone()
            .into_view()
            .as_advanced_builder()
            .set_inputs(vec![tx.raw().inputs().get(0).unwrap(); 2])
            .build()
            .data(),
        tx.clone()
            .into_view()
            .as_advanced_builder()
            .set_outputs(vec![
                tx.raw().outputs().get(0).unwrap(),
                tx.raw()
                    .outputs()
                    .get(1)
                    .unwrap()
                    .as_builder()
                    .lock(packed::Script::default())
                    .build(),
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
                    .capacity(1100 * CKB)
                    .build(),
                tx.raw().outputs().get(1).unwrap(),
            ])
            .build()
            .data(),
    ] {
        assert!(validate_funding(&bad, &cells, 1000u128 * u128::from(CKB), &terms).is_err());
    }
}
#[test]
fn full_default_lock_group_signature_recovers_key_and_preserves_transaction() {
    let (terms, tx, _) = fixture();
    let key = secp256k1::SecretKey::from_slice(&[7; 32]).unwrap();
    let signer = SecpCkbRawKeySigner::new_with_secret_keys(vec![key]);
    let lock: packed::Script = terms.wallet_lock.clone().into();
    let mut group = ScriptGroup::from_lock_script(&lock);
    group.input_indices = vec![0, 1];
    let signed = sign_group(tx.clone(), &group, &signer, &lock.args().raw_data()).unwrap();
    assert_eq!(signed.raw().as_slice(), tx.raw().as_slice());
    assert_eq!(signed.calc_tx_hash(), tx.calc_tx_hash());
    let witness =
        packed::WitnessArgs::from_slice(&signed.witnesses().get(0).unwrap().raw_data()).unwrap();
    assert_eq!(
        witness.input_type().to_opt().unwrap().raw_data().as_ref(),
        b"preserve"
    );
    let bytes = witness.lock().to_opt().unwrap().raw_data();
    let signature = secp256k1::ecdsa::RecoverableSignature::from_compact(
        &bytes[..64],
        secp256k1::ecdsa::RecoveryId::try_from(i32::from(bytes[64])).unwrap(),
    )
    .unwrap();
    let message =
        generate_message(&signed.clone().into_view(), &group, vec![0u8; 65].into()).unwrap();
    let public = secp256k1::SECP256K1
        .recover_ecdsa(
            &secp256k1::Message::from_digest_slice(&message).unwrap(),
            &signature,
        )
        .unwrap();
    assert_eq!(public, key.public_key(secp256k1::SECP256K1));
    let mut partial_group = group;
    partial_group.input_indices = vec![0];
    let wrong_message =
        generate_message(&signed.into_view(), &partial_group, vec![0u8; 65].into()).unwrap();
    assert_ne!(
        wrong_message, message,
        "peer's same-lock input must be in the group"
    );
}

fn separate_wallet_fixture() -> (
    Terms,
    packed::Transaction,
    Vec<(packed::CellOutput, Vec<u8>)>,
) {
    let (mut terms, tx, _) = fixture();
    let key = secp256k1::SecretKey::from_slice(&[8; 32]).unwrap();
    let hash = ckb_hash::blake2b_256(key.public_key(secp256k1::SECP256K1).serialize());
    let local: packed::Script = terms.wallet_lock.clone().into();
    let remote = local.clone().as_builder().args(hash[..20].pack()).build();
    terms.remote_wallet_lock = Some(remote.clone().into());
    let cells = vec![remote.clone(), local.clone(), local.clone()]
        .into_iter()
        .map(|lock| {
            (
                packed::CellOutput::new_builder()
                    .lock(lock)
                    .capacity(1000 * CKB)
                    .build(),
                vec![],
            )
        })
        .collect();
    let tx = tx
        .clone()
        .into_view()
        .as_advanced_builder()
        .set_inputs(
            (0u32..3)
                .map(|index| {
                    packed::CellInput::new_builder()
                        .previous_output(
                            packed::OutPoint::new_builder()
                                .tx_hash([1; 32].pack())
                                .index(index)
                                .build(),
                        )
                        .build()
                })
                .collect(),
        )
        .set_outputs(vec![
            tx.raw().outputs().get(0).unwrap(),
            packed::CellOutput::new_builder()
                .lock(local)
                .capacity(1000 * CKB - 600)
                .build(),
            packed::CellOutput::new_builder()
                .lock(remote)
                .capacity(901 * CKB - 400)
                .build(),
        ])
        .set_outputs_data(vec![packed::Bytes::default(); 3])
        .set_witnesses(vec![
            packed::WitnessArgs::new_builder()
                .lock(Some([9u8; 65].pack()))
                .build()
                .as_bytes()
                .pack(),
            packed::WitnessArgs::new_builder()
                .input_type(Some(ckb_types::bytes::Bytes::from_static(b"preserve")))
                .build()
                .as_bytes()
                .pack(),
            packed::Bytes::default(),
        ])
        .build()
        .data();
    (terms, tx, cells)
}

#[test]
fn separate_wallets_reject_shifted_contributions_and_missing_peer_funding() {
    let (terms, tx, cells) = separate_wallet_fixture();
    let amount = 1000u128 * u128::from(CKB);
    assert_eq!(validate_funding(&tx, &cells, amount, &terms).unwrap(), 1000);
    // Preserve total capacity and fee, but shift one CKB between wallets.
    for shift_to_peer in [true, false] {
        let local_change = if shift_to_peer {
            999 * CKB - 600
        } else {
            1001 * CKB - 600
        };
        let remote_change = if shift_to_peer {
            902 * CKB - 400
        } else {
            900 * CKB - 400
        };
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
                    .capacity(local_change)
                    .build(),
                tx.raw()
                    .outputs()
                    .get(2)
                    .unwrap()
                    .as_builder()
                    .capacity(remote_change)
                    .build(),
            ])
            .build()
            .data();
        assert!(validate_funding(&bad, &cells, amount, &terms).is_err());
    }
    let mut bad_cells = cells.clone();
    bad_cells[0].0 = bad_cells[0]
        .0
        .clone()
        .as_builder()
        .lock(terms.wallet_lock.clone())
        .build();
    assert!(validate_funding(&tx, &bad_cells, amount, &terms).is_err());
}

#[test]
fn separate_wallet_signature_only_changes_client_group() {
    let (terms, tx, cells) = separate_wallet_fixture();
    let key = secp256k1::SecretKey::from_slice(&[7; 32]).unwrap();
    let signer = SecpCkbRawKeySigner::new_with_secret_keys(vec![key]);
    let lock: packed::Script = terms.wallet_lock.into();
    let group = wallet_signing_group(&tx, &cells, &lock).unwrap();
    assert_eq!(group.input_indices, vec![1, 2]);
    let signed = sign_group(tx.clone(), &group, &signer, &lock.args().raw_data()).unwrap();
    assert_eq!(signed.raw().as_slice(), tx.raw().as_slice());
    assert_eq!(signed.witnesses().get(0), tx.witnesses().get(0));
    assert_eq!(signed.witnesses().get(2), tx.witnesses().get(2));
    let witness =
        packed::WitnessArgs::from_slice(&signed.witnesses().get(1).unwrap().raw_data()).unwrap();
    assert_eq!(
        witness.input_type().to_opt().unwrap().raw_data().as_ref(),
        b"preserve"
    );
    let bytes = witness.lock().to_opt().unwrap().raw_data();
    let signature = secp256k1::ecdsa::RecoverableSignature::from_compact(
        &bytes[..64],
        secp256k1::ecdsa::RecoveryId::try_from(i32::from(bytes[64])).unwrap(),
    )
    .unwrap();
    let message = generate_message(&signed.into_view(), &group, vec![0u8; 65].into()).unwrap();
    assert_eq!(
        secp256k1::SECP256K1
            .recover_ecdsa(
                &secp256k1::Message::from_digest_slice(&message).unwrap(),
                &signature
            )
            .unwrap(),
        key.public_key(secp256k1::SECP256K1)
    );
}

#[test]
fn legacy_terms_keep_original_wallet_and_shared_input_signing() {
    let (terms, tx, cells) = fixture();
    let mut saved = serde_json::to_value(&terms).unwrap();
    saved.as_object_mut().unwrap().remove("wallet_account");
    saved.as_object_mut().unwrap().remove("remote_wallet_lock");
    let restored: Terms = serde_json::from_value(saved).unwrap();
    assert_eq!(restored.wallet_account, 5);
    assert_eq!(restored.remote_wallet_lock(), &restored.wallet_lock);
    assert_eq!(
        validate_funding(&tx, &cells, 1000u128 * u128::from(CKB), &restored).unwrap(),
        1000
    );
    let lock = restored.wallet_lock.into();
    assert_eq!(
        wallet_signing_group(&tx, &cells, &lock)
            .unwrap()
            .input_indices,
        vec![0, 1]
    );
}

#[tokio::test]
async fn one_client_cell_only_needs_local_contribution_and_fee_budget() {
    let (_root, config, listener) = crate::account::tests::fixture();
    let mut terms = terms();
    terms.ckb_rpc_url = config.rpc_url;
    let capacity = 1000 * CKB + terms.max_fee;
    let output = ckb_jsonrpc_types::CellOutput {
        capacity: capacity.into(),
        lock: terms.wallet_lock.clone(),
        type_: None,
    };
    let server = crate::account::tests::serve(
        listener,
        vec![("get_cells", json!({"objects":[{"output":output}]}))],
    );
    let wallet = Wallet::from_terms(terms).unwrap();
    wallet.check_capacity(1000 * CKB).await.unwrap();
    server.join().unwrap();
}
