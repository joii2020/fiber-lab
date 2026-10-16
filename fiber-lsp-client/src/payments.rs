//! One online payment at a time. Both parties stay online until settlement.
use crate::{
    account::ClientAccount,
    channel,
    rpc::{Connection, MissingRecord, Rpc},
    wallet::{CKB, Wallet},
};
use anyhow::{Context, Result, bail, ensure};
use clap::Args;
use fiber_json_types::{
    Channel, ChannelSigningStatus, ChannelSigningTransition, ChannelState, CkbInvoiceStatus,
    GetChannelSigningStatusResult, GetInvoiceResult, GetPaymentCommandResult, Hash256,
    InvoiceResult, NewInvoiceParams, PaymentStatus, SubmitChannelSignatureParams,
};
use fiber_lsp_sdk::{
    PaymentAuthorization, ProcessOutcome, RecoveryRecord, SigningPolicy, SubmitParams,
};
use fiber_types::{CkbInvoice, Currency, HashAlgorithm};
use serde_json::json;
use std::{
    io::Write,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const MAX_TLC_MS: u64 = 14 * 24 * 60 * 60 * 1000;
const FINAL_TLC_MS: u64 = 24 * 60 * 60 * 1000;

#[derive(Args)]
pub struct PayOptions {
    pub invoice: String,
    #[arg(long, default_value_t = 120, value_parser = clap::value_parser!(u64).range(1..))]
    pub timeout: u64,
    #[command(flatten)]
    pub client: crate::CurrentClientOptions,
}
#[derive(Args)]
pub struct InvoiceOptions {
    #[arg(long, value_parser = channel::parse_amount, value_name = "CKB")]
    pub amount: u64,
    #[arg(long, default_value_t = 3600, value_parser = clap::value_parser!(u64).range(1..))]
    pub expiry: u64,
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
    pub timeout: Option<u64>,
    #[command(flatten)]
    pub client: crate::CurrentClientOptions,
}
fn now_ms() -> Result<u64> {
    Ok(u64::try_from(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
    )?)
}
fn ckb(value: u128) -> String {
    format!("{}.{:08}", value / u128::from(CKB), value % u128::from(CKB))
}

struct Operation {
    hash: Hash256,
    channel_id: Hash256,
    inbound: bool,
    amount: u128,
    authorization: PaymentAuthorization,
    invoice: Option<String>,
    preimage: Option<Hash256>,
    expiry_seconds: Option<u64>,
    release_started: bool,
    received: bool,
    succeeded: bool,
    complete: bool,
    pending_signature: Option<PendingSignature>,
    // Latest versions before the operation. A successful result requires both
    // lanes to have accepted and removed this exact authorized TLC afterwards.
    start_versions: [u64; 2],
}
struct PendingSignature {
    status: ChannelSigningStatus,
    params: SubmitChannelSignatureParams,
}
fn token(account: &ClientAccount) -> Result<&str> {
    account.session.tenant_token().context("缺少用户凭证")
}
fn invoice(address: &str) -> Result<CkbInvoice> {
    let parsed: CkbInvoice = address
        .parse()
        .map_err(|_| anyhow::anyhow!("发票编码或签名无效"))?;
    ensure!(parsed.is_signed(), "发票必须有签名");
    ensure!(parsed.payee_pub_key().is_some(), "发票缺少收款公钥");
    ensure!(
        parsed.data.timestamp <= u128::from(now_ms()?),
        "发票创建时间位于未来"
    );
    ensure!(
        parsed.currency == Currency::Fibd && parsed.udt_type_script().is_none(),
        "首期仅支持 devnet CKB 发票"
    );
    ensure!(
        parsed
            .amount()
            .is_some_and(|a| a > 0 && a <= u128::from(u64::MAX)),
        "发票必须有固定正金额"
    );
    Ok(parsed)
}
fn invoice_deadline(parsed: &CkbInvoice) -> Result<u64> {
    let expiry = parsed.expiry_time().context("发票必须包含有效期")?;
    Ok(u64::try_from(
        parsed
            .data
            .timestamp
            .checked_add(expiry.as_millis())
            .context("发票有效期溢出")?,
    )?)
}
fn authorization(
    hash: Hash256,
    inbound: bool,
    amount: u128,
    algorithm: HashAlgorithm,
    expires: u64,
) -> Result<PaymentAuthorization> {
    Ok(PaymentAuthorization {
        payment_hash: hash.into(),
        inbound,
        amount,
        hash_algorithm: algorithm,
        expires_at_ms: expires,
        min_tlc_expiry_delta_ms: 1,
        max_tlc_expiry_ms: expires.checked_add(MAX_TLC_MS).context("TLC 期限溢出")?,
    })
}
fn versions(records: &[RecoveryRecord]) -> [u64; 2] {
    [false, true].map(|lane| {
        records
            .iter()
            .filter(|r| r.reference.for_remote == lane)
            .map(|r| r.reference.version)
            .max()
            .unwrap_or(0)
    })
}
async fn register(account: &ClientAccount, op: &Operation) -> Result<()> {
    let signer = account.session.channel_signer(op.channel_id.into()).await?;
    if op.inbound {
        signer
            .authorize_invoice(
                op.authorization.clone(),
                op.preimage.context("缺少收款秘密")?.into(),
            )
            .await?;
    } else {
        signer.authorize_payment(op.authorization.clone()).await?;
    }
    Ok(())
}
async fn ready_channel(account: &ClientAccount, rpc: &Rpc, id: Hash256) -> Result<Channel> {
    let channels: fiber_json_types::ListChannelsResult = rpc
        .call(
            "list_channels",
            json!([{ "include_closed": true }]),
            token(account)?,
        )
        .await?;
    let channel = channels
        .channels
        .into_iter()
        .find(|c| c.channel_id == id)
        .context("远端无当前已绑定通道")?;
    ensure!(
        !channel.is_public && channel.pubkey == account.lsp_public_key,
        "收付款通道归属不一致"
    );
    ensure!(
        channel.state == ChannelState::ChannelReady,
        "通道不可收付款：远端状态 {:?}",
        channel.state
    );
    Ok(channel)
}

pub async fn pay(options: PayOptions) -> Result<()> {
    let _timing = crate::timings::Span::new("payment.pay");
    let client = crate::ClientOptions::from(options.client);
    let (dir, _selection) =
        crate::accounts::choose(&crate::accounts::root(), &client, false, false, false)?;
    let config = Connection::connect(&client.connection)?;
    let mut account = ClientAccount::load_dir(&config, dir).await?;
    account.session = account.session.with_policy(SigningPolicy::Manual);
    let rpc = Rpc::new(&config.rpc_url)?;
    let parsed = invoice(&options.invoice)?;
    let hash: Hash256 = (*parsed.payment_hash()).into();
    let expires = invoice_deadline(&parsed)?;
    ensure!(now_ms()? < expires, "发票已过期");
    let (id, wallet, _, _) = channel::payment_context(&account)?;
    wallet.check_network().await?;
    let channel = ready_channel(&account, &rpc, id).await?;
    let amount = parsed.amount().unwrap();
    // Match Fiber's SendPaymentData::new default for the exact SDK TLC authorization.
    let fee = (amount * 5).div_ceil(1000);
    let total = amount.checked_add(fee).context("付款金额及费用溢出")?;
    ensure!(
        total <= u128::from(u64::MAX) && channel.local_balance >= total,
        "本方可用余额不足"
    );
    let records = account
        .session
        .channel_signer(id.into())
        .await?
        .recovery_records()
        .await?;
    let mut op = Operation {
        hash,
        channel_id: id,
        inbound: false,
        amount,
        authorization: authorization(
            hash,
            false,
            total,
            parsed.hash_algorithm().copied().unwrap_or_default(),
            expires,
        )?,
        invoice: Some(options.invoice),
        preimage: None,
        expiry_seconds: None,
        release_started: false,
        received: false,
        succeeded: false,
        complete: false,
        pending_signature: None,
        start_versions: versions(&records),
    };
    println!("付款 hash：{hash}\nFiber 默认费用预算：{} CKB", ckb(fee));
    register(&account, &op).await?;
    send_payment(&account, &rpc, &op).await?;
    wait(&mut account, &rpc, &mut op, Some(options.timeout)).await
}

pub async fn create_invoice(options: InvoiceOptions) -> Result<()> {
    let _timing = crate::timings::Span::new("payment.invoice");
    let client = crate::ClientOptions::from(options.client);
    let (dir, _selection) =
        crate::accounts::choose(&crate::accounts::root(), &client, false, false, false)?;
    let config = Connection::connect(&client.connection)?;
    let mut account = ClientAccount::load_dir(&config, dir).await?;
    account.session = account.session.with_policy(SigningPolicy::Manual);
    let rpc = Rpc::new(&config.rpc_url)?;
    let (id, wallet, _, _) = channel::payment_context(&account)?;
    wallet.check_network().await?;
    ready_channel(&account, &rpc, id).await?;
    let created = now_ms()?;
    let expires = created
        .checked_add(options.expiry.checked_mul(1000).context("发票有效期溢出")?)
        .context("发票有效期溢出")?;
    let preimage = fiber_types::Hash256::from(rand::random::<[u8; 32]>());
    let hash: Hash256 = Hash256(HashAlgorithm::CkbHash.hash(preimage.as_ref()));
    let records = account
        .session
        .channel_signer(id.into())
        .await?
        .recovery_records()
        .await?;
    let mut op = Operation {
        hash,
        channel_id: id,
        inbound: true,
        amount: u128::from(options.amount),
        authorization: authorization(
            hash,
            true,
            u128::from(options.amount),
            HashAlgorithm::CkbHash,
            expires,
        )?,
        invoice: None,
        preimage: Some(preimage.into()),
        expiry_seconds: Some(options.expiry),
        release_started: false,
        received: false,
        succeeded: false,
        complete: false,
        pending_signature: None,
        start_versions: versions(&records),
    };
    register(&account, &op).await?;
    create_remote_invoice(&account, &rpc, &mut op).await?;
    print_invoice(&op);
    std::io::stdout().flush()?;
    wait(&mut account, &rpc, &mut op, options.timeout).await
}

fn validate_created(
    account: &ClientAccount,
    op: &Operation,
    address: &str,
    object: &fiber_json_types::CkbInvoice,
) -> Result<()> {
    let parsed = invoice(address)?;
    let encoded: fiber_json_types::CkbInvoice = parsed.clone().into();
    ensure!(encoded == *object, "发票地址与远端对象不一致");
    ensure!(
        Hash256::from(*parsed.payment_hash()) == op.hash
            && parsed.amount() == Some(op.amount)
            && parsed.hash_algorithm().copied().unwrap_or_default()
                == op.authorization.hash_algorithm
            && parsed.expiry_time().map(Duration::as_secs) == op.expiry_seconds
            && parsed.payee_pub_key().map(|p| p.serialize()) == Some(account.invoice_public_key.0)
            && parsed.trampoline_route_hint().map(|p| p.serialize())
                == Some(account.lsp_public_key.0)
            && !parsed.allow_mpp(),
        "创建结果与原收款授权或路由归属不符"
    );
    ensure!(
        parsed.data.timestamp <= u128::from(now_ms()?)
            && parsed.data.timestamp
                >= u128::from(op.authorization.expires_at_ms - op.expiry_seconds.unwrap() * 1000),
        "创建发票时间与原创建意图不符"
    );
    Ok(())
}
async fn create_remote_invoice(
    account: &ClientAccount,
    rpc: &Rpc,
    op: &mut Operation,
) -> Result<()> {
    let request = NewInvoiceParams {
        amount: op.amount,
        currency: fiber_json_types::Currency::Fibd,
        payment_hash: Some(op.hash),
        expiry: op.expiry_seconds,
        final_expiry_delta: Some(FINAL_TLC_MS),
        hash_algorithm: Some(fiber_json_types::HashAlgorithm::CkbHash),
        allow_mpp: Some(false),
        allow_trampoline_routing: Some(true),
        ..Default::default()
    };
    let result: InvoiceResult = rpc
        .call("new_invoice", json!([request]), token(account)?)
        .await?;
    let (address, object) = (result.invoice_address, result.invoice);
    validate_created(account, op, &address, &object)?;
    op.invoice = Some(address);
    Ok(())
}
fn print_invoice(op: &Operation) {
    println!(
        "invoice_address：{}\n付款 hash：{}\n发票到期（Unix ms，本地授权）：{}",
        op.invoice.as_deref().unwrap_or("创建结果待核对"),
        op.hash,
        op.authorization.expires_at_ms
    );
}
struct ReceiptTiming {
    hash: Hash256,
    waiting: Option<crate::timings::Span>,
    processing: Option<crate::timings::Span>,
}

impl ReceiptTiming {
    fn new(hash: Hash256) -> Self {
        Self {
            hash,
            waiting: Some(crate::timings::Span::new("payment.wait_for_receipt")),
            processing: None,
        }
    }

    fn observe_tlc(&mut self, hash: Hash256, inbound: bool) {
        if hash == self.hash && inbound && self.processing.is_none() {
            drop(self.waiting.take());
            self.processing = Some(crate::timings::Span::new("payment.receipt_processing"));
        }
    }

    fn observe_status(&mut self, status: &ChannelSigningStatus) {
        if let ChannelSigningStatus::SignatureRequired {
            settlement: Some(settlement),
            ..
        } = status
        {
            for tlc in &settlement.tlcs {
                self.observe_tlc(tlc.payment_hash, tlc.inbound);
            }
        }
    }

    fn finish(mut self, complete: bool) {
        if let Some(processing) = &mut self.processing {
            processing.finish(complete);
        }
    }
}

async fn wait(
    account: &mut ClientAccount,
    rpc: &Rpc,
    op: &mut Operation,
    timeout: Option<u64>,
) -> Result<()> {
    let mut receipt = op.inbound.then(|| ReceiptTiming::new(op.hash));
    let future = drive(account, rpc, op, receipt.as_mut());
    let deadline = async {
        if let Some(seconds) = timeout {
            tokio::time::sleep(Duration::from_secs(seconds)).await;
        } else {
            std::future::pending::<()>().await;
        }
    };
    let result = tokio::select! {
        result = future => result,
        result = tokio::signal::ctrl_c() => result.map_err(anyhow::Error::from).and_then(|()| {
            bail!("收付款推进已中断；网络操作未取消，当前版本不支持中断恢复")
        }),
        _ = deadline => Err(anyhow::anyhow!("收付款等待超时；网络操作未取消，当前版本不支持中断恢复")),
    };
    if let Some(receipt) = receipt {
        receipt.finish(op.complete);
    }
    result
}

async fn lookup_payment(
    account: &ClientAccount,
    rpc: &Rpc,
    hash: Hash256,
) -> Result<Option<GetPaymentCommandResult>> {
    match rpc
        .call(
            "get_payment",
            json!([{ "payment_hash": hash }]),
            token(account)?,
        )
        .await
    {
        Ok(result) => Ok(Some(result)),
        Err(e) if e.downcast_ref::<MissingRecord>().is_some() => Ok(None),
        Err(e) => Err(e),
    }
}
fn payment_params(account: &ClientAccount, op: &Operation) -> serde_json::Value {
    json!([{ "invoice": op.invoice, "payment_hash": op.hash,
        "max_parts": "0x1", "trampoline_hops": [account.lsp_public_key],
        "keysend": false, "allow_self_payment": false,
        "tlc_expiry_limit": format!("0x{MAX_TLC_MS:x}") }])
}
async fn send_payment(account: &ClientAccount, rpc: &Rpc, op: &Operation) -> Result<()> {
    let _timing = crate::timings::Span::new("payment.send");
    ensure!(
        now_ms()? < op.authorization.expires_at_ms,
        "原付款授权已过期；未发送新付款"
    );
    ensure!(
        lookup_payment(account, rpc, op.hash).await?.is_none(),
        "远端已有此付款 hash；不重复发送"
    );
    let response: GetPaymentCommandResult = rpc
        .call("send_payment", payment_params(account, op), token(account)?)
        .await?;
    ensure!(
        response.payment_hash == op.hash,
        "发送结果属于其他付款 hash"
    );
    Ok(())
}

async fn drive(
    account: &mut ClientAccount,
    rpc: &Rpc,
    op: &mut Operation,
    mut receipt: Option<&mut ReceiptTiming>,
) -> Result<()> {
    let _timing = crate::timings::Span::new("payment.drive");
    let (id, wallet, local_reserve, remote_reserve) = channel::payment_context(account)?;
    ensure!(op.channel_id == id, "收付款属于其他通道");
    loop {
        ready_channel(account, rpc, id).await?;
        refresh(account, rpc, op).await?;
        sign(account, rpc, op, receipt.as_deref_mut()).await?;
        release(account, rpc, op, &wallet).await?;
        let remote = ready_channel(account, rpc, id).await?;
        let records = account
            .session
            .channel_signer(id.into())
            .await?
            .recovery_records()
            .await?;
        reconcile(op, &records, &remote, local_reserve, remote_reserve)?;
        if op.complete {
            println!(
                "{}完成（远端状态、SDK TLC 移除及余额已核对）\n付款 hash：{}\n金额：{} CKB",
                if op.inbound { "收款" } else { "付款" },
                op.hash,
                ckb(op.amount)
            );
            return Ok(());
        }
        let _protocol_wait = receipt
            .as_ref()
            .filter(|receipt| receipt.processing.is_some())
            .map(|_| crate::timings::Span::new("payment.receipt_protocol_wait"));
        let _poll_wait = crate::timings::Span::new("payment.poll_wait");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}
async fn refresh(account: &ClientAccount, rpc: &Rpc, op: &mut Operation) -> Result<()> {
    let _timing = crate::timings::Span::new("payment.refresh");
    let signer = account.session.channel_signer(op.channel_id.into()).await?;
    if op.inbound {
        let remote: GetInvoiceResult = rpc
            .call(
                "get_invoice",
                json!([{ "payment_hash": op.hash }]),
                token(account)?,
            )
            .await?;
        validate_created(account, op, &remote.invoice_address, &remote.invoice)?;

        match remote.status {
            CkbInvoiceStatus::Paid => op.succeeded = true,
            CkbInvoiceStatus::Expired | CkbInvoiceStatus::Cancelled => {
                bail!("发票已过期或取消；当前版本不支持中断恢复，未继续结算");
            }
            CkbInvoiceStatus::Received => op.received = true,
            _ => {}
        }
    } else if let Some(remote) = lookup_payment(account, rpc, op.hash).await? {
        ensure!(
            remote.payment_hash == op.hash && remote.fee <= op.authorization.amount - op.amount,
            "付款查询 hash 或费用不符"
        );

        if let Some(preimage) = remote.payment_preimage {
            ensure!(
                op.authorization.hash_algorithm.hash(preimage.0) == op.hash.0,
                "付款返回 preimage 与原 hash 不符"
            );
            op.preimage = Some(preimage);
            signer
                .authorize_fulfillment(op.hash.into(), preimage.into())
                .await?;
        }
        match remote.status {
            PaymentStatus::Success => {
                ensure!(op.preimage.is_some(), "远端成功但缺少可核验 preimage");
                op.succeeded = true;
            }
            PaymentStatus::Failed => {
                bail!(
                    "付款失败：{}；当前版本不支持中断恢复，不自动重试",
                    remote.failed_error.as_deref().unwrap_or("远端未提供原因")
                );
            }
            _ => {}
        }
    } else {
        bail!("远端未找到本次付款，发送结果未知；不自动重发");
    }
    Ok(())
}

async fn sign(
    account: &mut ClientAccount,
    rpc: &Rpc,
    op: &mut Operation,
    receipt: Option<&mut ReceiptTiming>,
) -> Result<()> {
    let _timing = crate::timings::Span::new("payment.external_sign");
    let id = op.channel_id;
    let result: GetChannelSigningStatusResult = rpc
        .call(
            "get_channel_signing_status",
            json!([{ "channel_id": id }]),
            token(account)?,
        )
        .await?;
    ensure!(result.channel_id == id, "签名请求属于其他通道");
    if let ChannelSigningStatus::SignatureRequired {
        settlement: Some(settlement),
        ..
    } = &result.status
    {
        ensure!(
            settlement
                .tlcs
                .iter()
                .all(|tlc| tlc.payment_hash == op.hash),
            "签名请求包含其他付款；当前仅支持单笔在线收付款"
        );
    }
    if let Some(receipt) = receipt {
        receipt.observe_status(&result.status);
    }
    let ChannelSigningStatus::SignatureRequired {
        request_id,
        transition,
        ..
    } = &result.status
    else {
        return Ok(());
    };
    ensure!(
        matches!(
            transition,
            ChannelSigningTransition::SendCommitmentSigned
                | ChannelSigningTransition::CompleteReceivedCommitment
                | ChannelSigningTransition::SendRevokeAndAck
                | ChannelSigningTransition::CompleteReceivedRevokeAndAck
        ),
        "收付款不授权关闭、公告或链上签名"
    );
    let saved = op
        .pending_signature
        .as_ref()
        .filter(|saved| saved.params.request_id == *request_id);
    let params = if let Some(saved) = saved {
        ensure!(
            saved.params.channel_id == id
                && serde_json::to_vec(&saved.status)? == serde_json::to_vec(&result.status)?,
            "同 request ID 的签名内容改变；拒绝冲突签名"
        );
        // Revalidate the original request against the current SDK state and
        // clock before replaying its saved signature. Manual policy prepares
        // here without producing another signature.
        let checked = account
            .session
            .handle_channel_status(id.into(), result.status.clone(), now_ms()?)
            .await?;
        ensure!(
            matches!(checked, ProcessOutcome::NeedConfirmation(_)),
            "SDK 未授权原签名重放"
        );
        saved.params.clone()
    } else {
        let sdk = crate::timings::Span::new("payment.sdk_prepare_confirm");
        let now = now_ms()?;
        let outcome = account
            .session
            .handle_channel_status(id.into(), result.status.clone(), now)
            .await?;
        let submit = match outcome {
            ProcessOutcome::NeedConfirmation(pending) => {
                account.session.confirm(pending, now_ms()?).await?
            }
            ProcessOutcome::ReadyToSubmit(params) => params,
            ProcessOutcome::Idle => return Ok(()),
            ProcessOutcome::Denied => bail!("SDK 拒绝签名"),
        };
        let SubmitParams::Channel(params) = submit else {
            bail!("收付款不授权 watchtower 签名");
        };
        drop(sdk);
        op.pending_signature = Some(PendingSignature {
            status: result.status,
            params: params.clone(),
        });
        params
    };
    let _: serde_json::Value = rpc
        .call("submit_channel_signature", json!([params]), token(account)?)
        .await?;
    Ok(())
}
async fn release(
    account: &ClientAccount,
    rpc: &Rpc,
    op: &mut Operation,
    wallet: &Wallet,
) -> Result<()> {
    let _timing = crate::timings::Span::new("payment.release");
    if !op.inbound || !op.received || op.release_started {
        return Ok(());
    }
    let signer = account.session.channel_signer(op.channel_id.into()).await?;
    let records = signer.recovery_records().await?;
    let Some(record) = records
        .iter()
        .filter(|r| !r.reference.for_remote)
        .max_by_key(|r| r.reference.version)
    else {
        return Ok(());
    };
    if record.complete_signature.is_none()
        || !record
            .settlement
            .data
            .tlcs
            .iter()
            .any(|t| t.payment_hash == op.hash.into())
    {
        return Ok(());
    }
    let preimage = op.preimage.context("缺少本地收款秘密")?;
    // Verify live funding and the safety window immediately before release.
    signer
        .authorize_preimage_release(
            op.hash.into(),
            preimage.into(),
            now_ms()?.checked_add(30_000).context("释放安全时间溢出")?,
            wallet,
        )
        .await?;
    let _: serde_json::Value = rpc
        .call(
            "settle_invoice",
            json!([{ "payment_hash": op.hash, "payment_preimage": preimage }]),
            token(account)?,
        )
        .await?;
    op.release_started = true;
    Ok(())
}
fn reconcile(
    op: &mut Operation,
    records: &[RecoveryRecord],
    remote: &Channel,
    local_reserve: u64,
    remote_reserve: u64,
) -> Result<()> {
    let _timing = crate::timings::Span::new("payment.reconcile");
    if !op.succeeded {
        return Ok(());
    }
    let Ok((local, peer)) = channel::settled_balances(records) else {
        return Ok(());
    };
    if remote.local_balance.checked_add(u128::from(local_reserve)) != Some(local)
        || remote
            .remote_balance
            .checked_add(u128::from(remote_reserve))
            != Some(peer)
        || !remote.pending_tlcs.is_empty()
        || remote.offered_tlc_balance != 0
        || remote.received_tlc_balance != 0
    {
        return Ok(());
    }
    let seen = [false, true].into_iter().enumerate().all(|(i, lane)| {
        records.iter().any(|r| {
            r.reference.for_remote == lane
                && r.reference.version > op.start_versions[i]
                && r.settlement.data.tlcs.iter().any(|t| {
                    t.payment_hash == op.hash.into() && t.payment_amount == op.authorization.amount
                })
        })
    });
    if !seen || (op.inbound && !op.release_started) {
        return Ok(());
    }
    op.complete = true;
    Ok(())
}

#[cfg(test)]
#[path = "test_payments.rs"]
mod tests;
