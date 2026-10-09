//! Locally authorized off-chain operations. The SDK is the signing trust boundary.
use crate::{
    account::ClientAccount,
    channel,
    rpc::{Config, MissingRecord, Rpc},
    store::{AccountDir, atomic_write, read_private},
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
use serde::{Deserialize, Serialize};
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
    #[arg(long, value_parser = parse_fee, value_name = "CKB")]
    pub max_fee: u64,
    #[arg(long, default_value_t = 120, value_parser = clap::value_parser!(u64).range(1..))]
    pub timeout: u64,
    #[command(flatten)]
    pub client: crate::ClientOptions,
}
#[derive(Args)]
pub struct InvoiceOptions {
    #[arg(long, value_parser = channel::parse_amount, value_name = "CKB")]
    pub amount: u64,
    #[arg(long, default_value_t = 3600, value_parser = clap::value_parser!(u64).range(1..))]
    pub expiry: u64,
    #[arg(long)]
    pub wait: bool,
    #[arg(long, requires = "wait", value_parser = clap::value_parser!(u64).range(1..))]
    pub timeout: Option<u64>,
    #[command(flatten)]
    pub client: crate::ClientOptions,
}
#[derive(Args)]
pub struct HashOptions {
    #[arg(long)]
    pub payment_hash: Hash256,
    #[command(flatten)]
    pub client: crate::ClientOptions,
}
#[derive(Args)]
pub struct ResumeOptions {
    #[command(flatten)]
    pub operation: HashOptions,
    #[arg(long, default_value_t = 120, value_parser = clap::value_parser!(u64).range(1..))]
    pub timeout: u64,
}

fn parse_fee(text: &str) -> std::result::Result<u64, String> {
    if !text.is_empty()
        && text.bytes().all(|b| b == b'0' || b == b'.')
        && text.matches('.').count() <= 1
        && !text.starts_with('.')
        && !text.ends_with('.')
        && text.split('.').nth(1).is_none_or(|s| s.len() <= 8)
    {
        Ok(0)
    } else {
        channel::parse_amount(text)
    }
}
fn now_ms() -> Result<u64> {
    Ok(u64::try_from(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
    )?)
}
fn ckb(value: u128) -> String {
    format!("{}.{:08}", value / u128::from(CKB), value % u128::from(CKB))
}

#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    IntentSaved,
    Submitted,
    Pending,
    RemoteSuccess,
    Complete,
    Failed,
    ExpiredPending,
    Expired,
}
#[derive(Clone, Deserialize, Serialize)]
struct Operation {
    hash: Hash256,
    channel_id: Hash256,
    inbound: bool,
    amount: u128,
    max_fee: u64,
    authorization: PaymentAuthorization,
    invoice: Option<String>,
    preimage: Option<Hash256>,
    expiry_seconds: Option<u64>,
    creation_started: bool,
    send_started: bool,
    release_started: bool,
    #[serde(default)]
    cancellation_started: bool,
    phase: Phase,
    remote_status: Option<String>,
    updated_at_ms: u64,
    // Latest versions before the operation. A successful result requires both
    // lanes to have accepted and removed this exact authorized TLC afterwards.
    start_versions: [u64; 2],
}
#[derive(Deserialize, Serialize)]
struct PendingSignature {
    status: ChannelSigningStatus,
    params: SubmitChannelSignatureParams,
}
#[derive(Deserialize, Serialize)]
struct Book {
    format_version: u32,
    tenant_id: String,
    operations: Vec<Operation>,
    pending_signature: Option<PendingSignature>,
}
impl Book {
    fn load(account: &ClientAccount) -> Result<Self> {
        let path = account.dir.path.join("payments.json");
        let book = if path.try_exists()? {
            serde_json::from_slice::<Self>(&read_private(&path)?)
                .map_err(|_| anyhow::anyhow!("payments.json 损坏；原文件已保留"))?
        } else {
            Self {
                format_version: 1,
                tenant_id: account.session.tenant_id().as_str().into(),
                operations: vec![],
                pending_signature: None,
            }
        };
        ensure!(
            book.format_version == 1 && book.tenant_id == account.session.tenant_id().as_str(),
            "收付款记录版本或账户归属不一致"
        );
        for (i, op) in book.operations.iter().enumerate() {
            ensure!(
                account.session.binding(op.channel_id.into()).is_some(),
                "收付款通道不属于当前账户"
            );
            ensure!(
                !book.operations[..i].iter().any(|p| p.hash == op.hash),
                "存在重复付款 hash"
            );
            ensure!(
                op.authorization.payment_hash == op.hash.into()
                    && op.authorization.inbound == op.inbound
                    && op.authorization.amount
                        == op
                            .amount
                            .checked_add(u128::from(op.max_fee))
                            .context("授权金额溢出")?,
                "收付款记录与 SDK 授权不一致"
            );
            if op.inbound {
                let preimage = op.preimage.context("收款秘密缺失；不能重新生成")?;
                ensure!(
                    op.max_fee == 0
                        && op.authorization.hash_algorithm.hash(preimage.0) == op.hash.0,
                    "收款秘密或费用不匹配"
                );
            }
        }
        Ok(book)
    }
    fn save(&self, dir: &AccountDir) -> Result<()> {
        atomic_write(
            &dir.path.join("payments.json"),
            &serde_json::to_vec_pretty(self)?,
        )
    }
    fn index(&self, hash: Hash256) -> Result<usize> {
        self.operations
            .iter()
            .position(|p| p.hash == hash)
            .context("当前账户没有此付款 hash 的本地授权")
    }
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
    let channels = rpc.list_channels(token(account)?).await?;
    let channel = channels
        .channels
        .into_iter()
        .find(|c| c.channel_id == id)
        .context("远端无当前已绑定通道")?;
    ensure!(
        channel.state == ChannelState::ChannelReady
            && !channel.is_public
            && channel.pubkey == account.lsp_public_key,
        "通道尚不可收付款或归属不一致"
    );
    Ok(channel)
}

pub async fn pay(options: PayOptions) -> Result<()> {
    let config = Config::load(options.client.config.as_deref())?;
    let mut account = ClientAccount::load(&config, options.client.account.as_deref()).await?;
    account.session = account.session.with_policy(SigningPolicy::Manual);
    let rpc = Rpc::new(&config.rpc_url)?;
    let parsed = invoice(&options.invoice)?;
    let hash: Hash256 = (*parsed.payment_hash()).into();
    let mut book = Book::load(&account)?;
    if let Ok(i) = book.index(hash) {
        let op = &book.operations[i];
        ensure!(
            !op.inbound
                && op.invoice.as_deref() == Some(&options.invoice)
                && op.max_fee == options.max_fee,
            "同 hash 的原付款授权不同；不能替换"
        );
    } else {
        let expires = invoice_deadline(&parsed)?;
        ensure!(now_ms()? < expires, "发票已过期");
        let (id, wallet, _, _) = channel::payment_context(&account)?;
        wallet.check_network().await?;
        let channel = ready_channel(&account, &rpc, id).await?;
        let amount = parsed.amount().unwrap();
        let total = amount
            .checked_add(u128::from(options.max_fee))
            .context("付款金额及费用溢出")?;
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
        let op = Operation {
            hash,
            channel_id: id,
            inbound: false,
            amount,
            max_fee: options.max_fee,
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
            creation_started: false,
            send_started: false,
            release_started: false,
            cancellation_started: false,
            phase: Phase::IntentSaved,
            remote_status: None,
            updated_at_ms: now_ms()?,
            start_versions: versions(&records),
        };
        book.operations.push(op);
        book.save(&account.dir)?;
    }
    println!(
        "付款 hash：{hash}\n费用上限：{} CKB",
        ckb(u128::from(options.max_fee))
    );
    wait(
        &mut account,
        &rpc,
        &mut book,
        Some(hash),
        Some(options.timeout),
        &options.client,
    )
    .await
}

pub async fn create_invoice(options: InvoiceOptions) -> Result<()> {
    let config = Config::load(options.client.config.as_deref())?;
    let mut account = ClientAccount::load(&config, options.client.account.as_deref()).await?;
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
    let mut book = Book::load(&account)?;
    book.operations.push(Operation {
        hash,
        channel_id: id,
        inbound: true,
        amount: u128::from(options.amount),
        max_fee: 0,
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
        creation_started: false,
        send_started: false,
        release_started: false,
        cancellation_started: false,
        phase: Phase::IntentSaved,
        remote_status: None,
        updated_at_ms: created,
        start_versions: versions(&records),
    });
    book.save(&account.dir)?;
    println!("创建意图已保存；付款 hash：{hash}");
    std::io::stdout().flush()?;
    let i = book.index(hash)?;
    let result = async {
        register(&account, &book.operations[i]).await?;
        ensure_invoice(&account, &rpc, &mut book, i).await
    }
    .await;
    if let Err(e) = result {
        recovery(&account, hash, &options.client);
        return Err(e);
    }
    print_invoice(&book.operations[i]);
    std::io::stdout().flush()?;
    if options.wait {
        wait(
            &mut account,
            &rpc,
            &mut book,
            Some(hash),
            options.timeout,
            &options.client,
        )
        .await
    } else {
        println!("已退出在线推进；重连后才可完成收款。CLI 退出不保证私有通道离线。");
        recovery(&account, hash, &options.client);
        Ok(())
    }
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
async fn lookup_invoice(
    account: &ClientAccount,
    rpc: &Rpc,
    hash: Hash256,
) -> Result<Option<GetInvoiceResult>> {
    match rpc
        .call(
            "get_invoice",
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
async fn ensure_invoice(
    account: &ClientAccount,
    rpc: &Rpc,
    book: &mut Book,
    i: usize,
) -> Result<()> {
    if book.operations[i].invoice.is_some() {
        return Ok(());
    }
    let op = &book.operations[i];
    let remote = if op.creation_started {
        lookup_invoice(account, rpc, op.hash).await?
    } else {
        None
    };
    let (address, object) = if let Some(remote) = remote {
        (remote.invoice_address, remote.invoice)
    } else {
        ensure!(
            now_ms()? < op.authorization.expires_at_ms,
            "原创建授权已过期；秘密保留，不重新创建发票"
        );
        book.operations[i].creation_started = true;
        book.save(&account.dir)?;
        let op = &book.operations[i];
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
        (result.invoice_address, result.invoice)
    };
    validate_created(account, &book.operations[i], &address, &object)?;
    book.operations[i].invoice = Some(address);
    book.operations[i].phase = Phase::Pending;
    book.operations[i].updated_at_ms = now_ms()?;
    book.save(&account.dir)
}
fn print_invoice(op: &Operation) {
    println!(
        "invoice_address：{}\n付款 hash：{}\n发票到期（Unix ms，本地授权）：{}",
        op.invoice.as_deref().unwrap_or("创建结果待核对"),
        op.hash,
        op.authorization.expires_at_ms
    );
}
fn recovery(account: &ClientAccount, hash: Hash256, options: &crate::ClientOptions) {
    let id = account.dir.path.file_name().unwrap().to_string_lossy();
    println!("恢复：./lsp-cli resume --payment-hash {hash} --account {id}");
    if let Some(path) = &options.config {
        println!("恢复时继续使用 --config {}", path.display());
    }
}

pub async fn resume(options: ResumeOptions) -> Result<()> {
    let client = &options.operation.client;
    let config = Config::load(client.config.as_deref())?;
    let mut account = ClientAccount::load(&config, client.account.as_deref()).await?;
    account.session = account.session.with_policy(SigningPolicy::Manual);
    let rpc = Rpc::new(&config.rpc_url)?;
    let mut book = Book::load(&account)?;
    book.index(options.operation.payment_hash)?;
    wait(
        &mut account,
        &rpc,
        &mut book,
        Some(options.operation.payment_hash),
        Some(options.timeout),
        client,
    )
    .await
}
pub async fn watch(options: crate::ClientOptions) -> Result<()> {
    let config = Config::load(options.config.as_deref())?;
    let mut account = ClientAccount::load(&config, options.account.as_deref()).await?;
    account.session = account.session.with_policy(SigningPolicy::Manual);
    let rpc = Rpc::new(&config.rpc_url)?;
    let mut book = Book::load(&account)?;
    println!("前台推进本账户原授权；Ctrl-C 停止等待，保留发票与恢复资料。");
    wait(&mut account, &rpc, &mut book, None, None, &options).await
}
async fn wait(
    account: &mut ClientAccount,
    rpc: &Rpc,
    book: &mut Book,
    target: Option<Hash256>,
    timeout: Option<u64>,
    options: &crate::ClientOptions,
) -> Result<()> {
    let future = drive(account, rpc, book, target);
    let deadline = async {
        if let Some(seconds) = timeout {
            tokio::time::sleep(Duration::from_secs(seconds)).await;
        } else {
            std::future::pending::<()>().await;
        }
    };
    let result = tokio::select! {
        result = future => result,
        result = tokio::signal::ctrl_c() => { result?; println!("已停止本次等待；网络操作未取消。"); Ok(()) },
        _ = deadline => { println!("本次等待超时；网络操作未取消，结果需继续核对。"); Ok(()) },
    };
    if let Some(hash) = target {
        recovery(account, hash, options);
    } else {
        println!("再次执行 ./lsp-cli watch 或按原 hash resume。");
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
fn payment_params(account: &ClientAccount, op: &Operation) -> Result<serde_json::Value> {
    // Trampoline consumes the fixed fee budget. Avoid the server's default fee
    // rate silently reducing it and invalidating the SDK's exact TLC amount.
    let rate = u64::try_from((u128::from(op.max_fee) * 1000).div_ceil(op.amount))?;
    Ok(json!([{ "invoice": op.invoice, "payment_hash": op.hash,
        "max_fee_amount": format!("0x{:x}", op.max_fee), "max_fee_rate": format!("0x{rate:x}"),
        "max_parts": "0x1", "trampoline_hops": [account.lsp_public_key],
        "keysend": false, "allow_self_payment": false,
        "tlc_expiry_limit": format!("0x{MAX_TLC_MS:x}") }]))
}
async fn send_once(account: &ClientAccount, rpc: &Rpc, book: &mut Book, i: usize) -> Result<()> {
    let op = &book.operations[i];
    if op.send_started {
        return Ok(());
    }
    ensure!(
        now_ms()? < op.authorization.expires_at_ms,
        "原付款授权已过期；未发送新付款"
    );
    // A duplicate remote hash is never replaced even if no local response exists.
    if lookup_payment(account, rpc, op.hash).await?.is_some() {
        book.operations[i].send_started = true;
        book.save(&account.dir)?;
        return Ok(());
    }
    book.operations[i].send_started = true;
    book.operations[i].phase = Phase::Submitted;
    book.operations[i].updated_at_ms = now_ms()?;
    book.save(&account.dir)?;
    let response: GetPaymentCommandResult = rpc
        .call(
            "send_payment",
            payment_params(account, &book.operations[i])?,
            token(account)?,
        )
        .await?;
    ensure!(
        response.payment_hash == book.operations[i].hash,
        "发送结果属于其他付款 hash"
    );
    Ok(())
}

async fn drive(
    account: &mut ClientAccount,
    rpc: &Rpc,
    book: &mut Book,
    target: Option<Hash256>,
) -> Result<()> {
    if let Some(hash) = target {
        let op = &book.operations[book.index(hash)?];
        if op.phase == Phase::Complete {
            println!(
                "本地已完成（原结果）：{}；查询远端请使用对应 status 命令",
                op.hash
            );
            return Ok(());
        }
    }
    let (id, wallet, local_reserve, remote_reserve) = channel::payment_context(account)?;
    wallet.check_network().await?;
    for i in 0..book.operations.len() {
        ensure!(book.operations[i].channel_id == id, "收付款属于其他通道");
        register(account, &book.operations[i]).await?;
        if book.operations[i].inbound && book.operations[i].invoice.is_none() {
            ensure_invoice(account, rpc, book, i).await?;
            print_invoice(&book.operations[i]);
        }
        if !book.operations[i].inbound && !book.operations[i].send_started {
            send_once(account, rpc, book, i).await?;
        }
    }
    loop {
        ready_channel(account, rpc, id).await?;
        for i in 0..book.operations.len() {
            refresh(account, rpc, book, i).await?;
        }
        sign(account, rpc, book, id).await?;
        for i in 0..book.operations.len() {
            release(account, rpc, book, i, &wallet).await?;
        }
        let remote = ready_channel(account, rpc, id).await?;
        let records = account
            .session
            .channel_signer(id.into())
            .await?
            .recovery_records()
            .await?;
        for i in 0..book.operations.len() {
            reconcile(
                &mut book.operations[i],
                &records,
                &remote,
                local_reserve,
                remote_reserve,
            )?;
        }
        book.save(&account.dir)?;
        if let Some(hash) = target {
            let op = &book.operations[book.index(hash)?];
            if op.phase == Phase::Complete {
                println!(
                    "{}完成（远端状态、SDK TLC 移除及余额已核对）\n付款 hash：{}\n金额：{} CKB",
                    if op.inbound { "收款" } else { "付款" },
                    hash,
                    ckb(op.amount)
                );
                return Ok(());
            }
            if op.phase == Phase::Expired {
                bail!("发票已过期且当前无入账 TLC；hash：{hash}，上游结果和余额仍需核对");
            }
            if op.phase == Phase::Failed {
                bail!("原操作已失败（TLC 清理已核对）；hash：{hash}，不自动再次发送");
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}
async fn refresh(account: &ClientAccount, rpc: &Rpc, book: &mut Book, i: usize) -> Result<()> {
    let op = &mut book.operations[i];
    let signer = account.session.channel_signer(op.channel_id.into()).await?;
    if op.inbound {
        let remote = lookup_invoice(account, rpc, op.hash)
            .await?
            .context("原发票远端不存在；保留秘密与授权")?;
        validate_created(account, op, &remote.invoice_address, &remote.invoice)?;
        op.remote_status = Some(format!("{:?}", remote.status));
        let records = signer.recovery_records().await?;
        let local = records
            .iter()
            .filter(|r| !r.reference.for_remote)
            .max_by_key(|r| r.reference.version);
        let tlc = local.and_then(|r| {
            r.settlement
                .data
                .tlcs
                .iter()
                .find(|t| t.payment_hash == op.hash.into())
        });
        let current = now_ms()?;
        if !op.release_started && tlc.is_some_and(|t| (t.expiry / 1000) * 1000 <= current) {
            signer.authorize_cancellation(op.hash.into()).await?;
            op.cancellation_started = true;
        }
        if remote.status == CkbInvoiceStatus::Paid {
            op.phase = Phase::RemoteSuccess;
        } else if remote.status == CkbInvoiceStatus::Expired || op.cancellation_started {
            // An accepted TLC may still settle after invoice expiry. No inferred cancellation.
            op.phase = Phase::ExpiredPending;
        } else if remote.status == CkbInvoiceStatus::Cancelled && !op.release_started {
            signer.authorize_cancellation(op.hash.into()).await?;
        }
    } else if let Some(remote) = lookup_payment(account, rpc, op.hash).await? {
        ensure!(
            remote.payment_hash == op.hash && remote.fee <= u128::from(op.max_fee),
            "付款查询 hash 或费用不符"
        );
        op.remote_status = Some(format!("{:?}", remote.status));
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
                op.phase = Phase::RemoteSuccess;
            }
            PaymentStatus::Failed if op.preimage.is_none() => {
                signer.authorize_cancellation(op.hash.into()).await?;
            }
            _ => {
                op.phase = Phase::Pending;
            }
        }
    } else {
        // The submit marker cannot prove whether the server saw the request.
        // Even confirmed absence does not authorize an automatic second send.
        op.remote_status = Some("不存在；原发送结果未知，未重发".into());
    }
    op.updated_at_ms = now_ms()?;
    book.save(&account.dir)
}
async fn sign(account: &mut ClientAccount, rpc: &Rpc, book: &mut Book, id: Hash256) -> Result<()> {
    let result: GetChannelSigningStatusResult = rpc
        .call(
            "get_channel_signing_status",
            json!([{ "channel_id": id }]),
            token(account)?,
        )
        .await?;
    ensure!(result.channel_id == id, "签名请求属于其他通道");
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
    let saved = book
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
            ProcessOutcome::Denied => bail!("SDK 拒绝签名；原资料保留"),
        };
        let SubmitParams::Channel(params) = submit else {
            bail!("收付款不授权 watchtower 签名");
        };
        account.save()?;
        book.pending_signature = Some(PendingSignature {
            status: result.status,
            params: params.clone(),
        });
        book.save(&account.dir)?;
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
    book: &mut Book,
    i: usize,
    wallet: &Wallet,
) -> Result<()> {
    let op = &book.operations[i];
    if !op.inbound || op.cancellation_started || op.remote_status.as_deref() != Some("Received") {
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
    // Check fresh live funding and remaining window on every retry. Never
    // bypass this because release_started or remote Received was persisted.
    signer
        .authorize_preimage_release(
            op.hash.into(),
            preimage.into(),
            now_ms()?.checked_add(30_000).context("释放安全时间溢出")?,
            wallet,
        )
        .await?;
    book.operations[i].release_started = true;
    book.operations[i].updated_at_ms = now_ms()?;
    book.save(&account.dir)?;
    let _: serde_json::Value = rpc
        .call(
            "settle_invoice",
            json!([{ "payment_hash": book.operations[i].hash, "payment_preimage": preimage }]),
            token(account)?,
        )
        .await?;
    Ok(())
}
fn reconcile(
    op: &mut Operation,
    records: &[RecoveryRecord],
    remote: &Channel,
    local_reserve: u64,
    remote_reserve: u64,
) -> Result<()> {
    let successful =
        op.remote_status.as_deref() == Some(if op.inbound { "Paid" } else { "Success" });
    let failed =
        op.remote_status.as_deref() == Some(if op.inbound { "Cancelled" } else { "Failed" });
    let expired =
        op.inbound && (op.remote_status.as_deref() == Some("Expired") || op.cancellation_started);
    if !successful && !failed && !expired {
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
    if expired {
        op.phase = Phase::Expired;
        return Ok(());
    }
    if successful {
        let seen = [false, true].into_iter().enumerate().all(|(i, lane)| {
            records.iter().any(|r| {
                r.reference.for_remote == lane
                    && r.reference.version > op.start_versions[i]
                    && r.settlement.data.tlcs.iter().any(|t| {
                        t.payment_hash == op.hash.into()
                            && t.payment_amount == op.authorization.amount
                    })
            })
        });
        if !seen || (op.inbound && !op.release_started) {
            return Ok(());
        }
        op.phase = Phase::Complete;
    } else {
        op.phase = Phase::Failed;
    }
    Ok(())
}

fn print_local(op: &Operation) {
    println!(
        "方向：{}\nhash：{}\n金额：{} CKB\n通道：{}\n本地阶段：{:?}\n更新时间（Unix ms）：{}\n本地授权到期（Unix ms）：{}",
        if op.inbound { "收款" } else { "付款" },
        op.hash,
        ckb(op.amount),
        op.channel_id,
        op.phase,
        op.updated_at_ms,
        op.authorization.expires_at_ms
    );
    if !op.inbound {
        println!("费用上限：{} CKB", ckb(u128::from(op.max_fee)));
    }
    println!(
        "最近推进时的远端状态：{}",
        op.remote_status.as_deref().unwrap_or("未核对")
    );
    if op.phase == Phase::RemoteSuccess {
        println!("远端成功，待本地核对");
    }
}
pub async fn status(options: HashOptions, inbound: bool) -> Result<()> {
    let config = Config::load(options.client.config.as_deref())?;
    let account = ClientAccount::load(&config, options.client.account.as_deref()).await?;
    let book = Book::load(&account)?;
    let op = &book.operations[book.index(options.payment_hash)?];
    ensure!(
        op.inbound == inbound,
        "hash 方向不符；请使用对应 invoice-status / payment-status"
    );
    print_local(op);
    let rpc = Rpc::new(&config.rpc_url)?;
    let result = async {
        if inbound {
            let result = lookup_invoice(&account, &rpc, op.hash).await?.context("远端发票不存在")?;
            validate_created(&account, op, &result.invoice_address, &result.invoice)?;
            println!("本次远端原始状态：{:?}\n发票到期（Unix ms）：{}", result.status, invoice_deadline(&invoice(&result.invoice_address)?)?);
            println!("delivery：用户接口不可用；未查询管理数据，不能推断是否已收到付款。查询可能唤醒租户。");
        } else {
            let result = lookup_payment(&account, &rpc, op.hash).await?.context("远端付款不存在，原发送结果需核对")?;
            ensure!(result.payment_hash == op.hash && result.fee <= u128::from(op.max_fee), "远端付款 hash 或费用不匹配");
            println!("本次远端原始状态：{:?}\n远端费用：{} CKB", result.status, ckb(result.fee));
            if result.failed_error.is_some() { println!("远端报告失败；具体原因请在节点日志核对。"); }
        }
        Ok::<_, anyhow::Error>(())
    }.await;
    println!("查询时间（Unix ms）：{}", now_ms()?);
    if let Err(e) = result {
        println!("远端未核对：{e:#}");
    }
    recovery(&account, op.hash, &options.client);
    Ok(())
}
pub async fn list(options: crate::ClientOptions) -> Result<()> {
    let config = Config::load(options.config.as_deref())?;
    let account = ClientAccount::load(&config, options.account.as_deref()).await?;
    let book = Book::load(&account)?;
    println!("本地收付款记录：{}（未查询远端）", book.operations.len());
    for op in &book.operations {
        print_local(op);
        recovery(&account, op.hash, &options);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::{
        test_registered,
        tests::{fixture, serve},
    };
    use fiber_types::Privkey;
    use std::{fs, os::unix::fs::PermissionsExt};

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
            max_fee: 0,
            authorization: authorization(
                hash,
                inbound,
                u128::from(CKB),
                HashAlgorithm::CkbHash,
                expires,
            )
            .unwrap(),
            invoice: None,
            preimage: inbound.then_some(preimage),
            expiry_seconds: Some(3600),
            creation_started: false,
            send_started: false,
            release_started: false,
            cancellation_started: false,
            phase: Phase::IntentSaved,
            remote_status: None,
            updated_at_ms: now_ms().unwrap(),
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
    async fn fixture_book(config: &Config, inbound: bool) -> (ClientAccount, Book) {
        let mut account = test_registered(config).await;
        let op = operation(&mut account, inbound).await;
        drop(account);
        let account = ClientAccount::load(config, None).await.unwrap();
        let mut book = Book::load(&account).unwrap();
        book.operations.push(op);
        book.save(&account.dir).unwrap();
        (account, book)
    }

    #[tokio::test]
    async fn lost_send_response_is_never_automatically_sent_again() {
        let (_root, config, listener) = fixture();
        let (account, mut book) = fixture_book(&config, false).await;
        let result = signed_invoice(&account, &book.operations[0]);
        book.operations[0].invoice = Some(result.invoice_address);
        let hash = book.operations[0].hash;
        book.save(&account.dir).unwrap();
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
        assert!(send_once(&account, &rpc, &mut book, 0).await.is_err());
        assert!(book.operations[0].send_started);
        server.join().unwrap();
        drop(account);
        let account = ClientAccount::load(&config, None).await.unwrap();
        let mut book = Book::load(&account).unwrap();
        // No server remains: this succeeds only if no duplicate RPC is sent.
        send_once(&account, &rpc, &mut book, 0).await.unwrap();
        assert_eq!(book.operations[0].hash, hash);
        assert_eq!(
            fs::metadata(account.dir.path.join("payments.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[tokio::test]
    async fn lost_creation_response_looks_up_the_same_hash_and_secret() {
        let (_root, config, listener) = fixture();
        let (account, mut book) = fixture_book(&config, true).await;
        let result = signed_invoice(&account, &book.operations[0]);
        let preimage = book.operations[0].preimage;
        let hash = book.operations[0].hash;
        let server = serve(
            listener,
            vec![
                ("new_invoice", json!({"error":{"code":-1,"message":"lost"}})),
                (
                    "get_invoice",
                    json!({"invoice_address":result.invoice_address,"invoice":result.invoice,"status":"Open"}),
                ),
            ],
        );
        let rpc = Rpc::new(&config.rpc_url).unwrap();
        assert!(ensure_invoice(&account, &rpc, &mut book, 0).await.is_err());
        drop(account);
        let account = ClientAccount::load(&config, None).await.unwrap();
        let mut book = Book::load(&account).unwrap();
        ensure_invoice(&account, &rpc, &mut book, 0).await.unwrap();
        assert_eq!(book.operations[0].hash, hash);
        assert_eq!(book.operations[0].preimage, preimage);
        assert_eq!(
            book.operations[0].invoice.as_deref(),
            Some(result.invoice_address.as_str())
        );
        server.join().unwrap();
    }

    #[tokio::test]
    async fn malformed_created_invoice_keeps_original_secret_and_creation_marker() {
        let (_root, config, listener) = fixture();
        let (account, mut book) = fixture_book(&config, true).await;
        let mut result = signed_invoice(&account, &book.operations[0]);
        result.invoice.amount = Some(2 * u128::from(CKB));
        let server = serve(
            listener,
            vec![("new_invoice", serde_json::to_value(result).unwrap())],
        );
        let rpc = Rpc::new(&config.rpc_url).unwrap();
        assert!(ensure_invoice(&account, &rpc, &mut book, 0).await.is_err());
        assert!(book.operations[0].creation_started && book.operations[0].invoice.is_none());
        assert!(book.operations[0].preimage.is_some());
        server.join().unwrap();
    }

    #[tokio::test]
    async fn query_failure_does_not_write_records_or_authorize_any_signature() {
        let (_root, config, listener) = fixture();
        let (account, mut book) = fixture_book(&config, true).await;
        let result = signed_invoice(&account, &book.operations[0]);
        book.operations[0].invoice = Some(result.invoice_address);
        book.save(&account.dir).unwrap();
        let paths = ["payments.json", "signer.bin", "account.json"];
        let before = paths.map(|p| fs::read(account.dir.path.join(p)).unwrap());
        let server = serve(
            listener,
            vec![(
                "get_invoice",
                json!({"error":{"code":-1,"message":"secret-preimage"}}),
            )],
        );
        let rpc = Rpc::new(&config.rpc_url).unwrap();
        assert!(
            lookup_invoice(&account, &rpc, book.operations[0].hash)
                .await
                .is_err()
        );
        server.join().unwrap();
        for (p, content) in paths.into_iter().zip(before) {
            assert_eq!(fs::read(account.dir.path.join(p)).unwrap(), content);
        }
    }

    #[test]
    fn cli_requires_explicit_amount_fee_and_wait_for_timeout() {
        use clap::Parser;
        assert!(crate::Cli::try_parse_from(["lsp-cli", "pay", "invoice"]).is_err());
        assert!(
            crate::Cli::try_parse_from(["lsp-cli", "pay", "invoice", "--max-fee", "0"]).is_ok()
        );
        assert!(crate::Cli::try_parse_from(["lsp-cli", "create-invoice"]).is_err());
        assert!(
            crate::Cli::try_parse_from([
                "lsp-cli",
                "create-invoice",
                "--amount",
                "1",
                "--timeout",
                "1"
            ])
            .is_err()
        );
        assert!(
            crate::Cli::try_parse_from([
                "lsp-cli",
                "create-invoice",
                "--amount",
                "1",
                "--wait",
                "--timeout",
                "1"
            ])
            .is_ok()
        );
        assert!(
            authorization(Hash256([1; 32]), true, 1, HashAlgorithm::CkbHash, u64::MAX).is_err()
        );
    }
    #[tokio::test]
    async fn trampoline_fee_and_restored_authorization_cannot_drift() {
        let (_root, config, _listener) = fixture();
        let (account, mut book) = fixture_book(&config, false).await;
        book.operations[0].max_fee = 1_000_000;
        book.operations[0].authorization.amount += 1_000_000;
        let result = signed_invoice(&account, &book.operations[0]);
        book.operations[0].invoice = Some(result.invoice_address);
        book.save(&account.dir).unwrap();
        let loaded = Book::load(&account).unwrap();
        let params = payment_params(&account, &loaded.operations[0]).unwrap();
        assert_eq!(params[0]["max_fee_amount"], "0xf4240");
        assert_eq!(params[0]["max_fee_rate"], "0xa");
        assert_eq!(params[0]["max_parts"], "0x1");
        assert_eq!(
            params[0]["trampoline_hops"],
            json!([account.lsp_public_key])
        );
        // No network timeout is passed from the CLI's local wait timeout.
        assert!(params[0].get("timeout").is_none());
        book.operations[0].max_fee += 1;
        book.save(&account.dir).unwrap();
        assert!(Book::load(&account).is_err());
    }

    #[tokio::test]
    async fn wrong_channel_signing_response_never_mutates_sdk_or_submits() {
        let (_root, config, listener) = fixture();
        let (mut account, mut book) = fixture_book(&config, false).await;
        let id = book.operations[0].channel_id;
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
        let error = sign(&mut account, &rpc, &mut book, id).await.unwrap_err();
        assert!(error.to_string().contains("其他通道"));
        assert_eq!(
            fs::read(account.dir.path.join("signer.bin")).unwrap(),
            before
        );
        server.join().unwrap();
    }

    #[test]
    fn fee_budget_is_bound_to_single_part_trampoline_parameters() {
        assert_eq!(parse_fee("0").unwrap(), 0);
        assert_eq!(parse_fee("0.001").unwrap(), 100_000);
        assert!(parse_fee("-1").is_err());
    }
}
