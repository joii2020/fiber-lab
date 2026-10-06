//! One durable lifecycle intent per hosted tenant (the current SDK/server limit).
use crate::{
    account::ClientAccount,
    rpc::{Config, Rpc},
    store::{AccountDir, atomic_write, read_private},
    wallet::{CKB, RESERVE, Terms, Wallet},
};
use anyhow::{Context, Result, bail, ensure};
use ckb_types::{packed, prelude::*};
use clap::Args;
use fiber_json_types::{
    Channel, ChannelSigningStatus, ChannelSigningTransition, ChannelState,
    GetChannelSigningStatusResult, Hash256, OpenChannelWithExternalFundingParams,
    OpenTenantChannelResult, ShutdownChannelParams, SubmitChannelSignatureParams,
    SubmitSignedFundingTxParams, SubmitSignedFundingTxResult,
};
use fiber_lsp_sdk::{
    CloseAuthorization, ProcessOutcome, SigningPolicy, SubmitParams, TenantOpeningRpc,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Args)]
pub struct OpenOptions {
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
    /// list-accounts 列出的账户 ID
    #[arg(long, value_name = "ID")]
    pub account: Option<String>,
    /// 本方总注入额（含保留金额），最多八位小数；首次开通必填
    #[arg(long, value_parser = parse_amount)]
    pub funding_amount: Option<u64>,
    /// 等待最终状态的秒数；超时保留进度
    #[arg(long, default_value_t = 120, value_parser = clap::value_parser!(u64).range(1..))]
    pub timeout: u64,
}

#[derive(Args)]
pub struct CloseOptions {
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
    /// list-accounts 列出的账户 ID
    #[arg(long, value_name = "ID")]
    pub account: Option<String>,
    #[arg(long)]
    pub channel_id: Option<Hash256>,
    #[arg(long, default_value_t = 120, value_parser = clap::value_parser!(u64).range(1..))]
    pub timeout: u64,
}

pub fn parse_amount(text: &str) -> std::result::Result<u64, String> {
    let parts: Vec<_> = text.split('.').collect();
    if parts.len() > 2 || parts[0].is_empty() || !parts[0].bytes().all(|b| b.is_ascii_digit()) {
        return Err("金额必须为十进制 CKB 数字".into());
    }
    let fraction = parts.get(1).copied().unwrap_or("");
    if (parts.len() == 2 && fraction.is_empty())
        || fraction.len() > 8
        || !fraction.bytes().all(|b| b.is_ascii_digit())
    {
        return Err("CKB 金额最多八位小数".into());
    }
    let whole = parts[0].parse::<u64>().map_err(|_| "CKB 金额溢出")?;
    let sub = if fraction.is_empty() {
        0
    } else {
        fraction.parse::<u64>().map_err(|_| "CKB 小数无效")? * 10u64.pow(8 - fraction.len() as u32)
    };
    let amount = whole
        .checked_mul(CKB)
        .and_then(|n| n.checked_add(sub))
        .ok_or("CKB 金额溢出")?;
    if amount == 0 {
        return Err("CKB 金额必须大于零".into());
    }
    Ok(amount)
}

fn amount(value: u128) -> String {
    format!("{}.{:08}", value / u128::from(CKB), value % u128::from(CKB))
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Stage {
    Opening,
    FundingSaved,
    FundingSubmitted,
    Ready,
    CloseRequested,
    Closing,
    Closed,
}

#[derive(Clone, Deserialize, Serialize)]
struct Intent {
    format_version: u32,
    tenant_id: String,
    terms: Terms,
    request: OpenChannelWithExternalFundingParams,
    stage: Stage,
    opening: Option<OpenTenantChannelResult>,
    signed_funding: Option<ckb_jsonrpc_types::Transaction>,
    funding_hash: Option<ckb_types::H256>,
    funding_fee: Option<u64>,
    close_authorization: Option<CloseAuthorization>,
    close_transaction: Option<ckb_jsonrpc_types::Transaction>,
    close_hash: Option<ckb_types::H256>,
    pending_signature: Option<SubmitChannelSignatureParams>,
    #[serde(default)]
    pending_status: Option<ChannelSigningStatus>,
}

fn read_intent(dir: &AccountDir) -> Result<Option<Intent>> {
    let path = dir.path.join("channel.json");
    if !path.try_exists()? {
        return Ok(None);
    }
    let record: Intent = serde_json::from_slice(&read_private(&path)?)
        .map_err(|_| anyhow::anyhow!("channel.json 损坏；原文件已保留"))?;
    ensure!(record.format_version == 1, "channel.json 格式版本不支持");
    Ok(Some(record))
}
fn save_intent(dir: &AccountDir, record: &Intent) -> Result<()> {
    atomic_write(
        &dir.path.join("channel.json"),
        &serde_json::to_vec_pretty(record)?,
    )
}
fn token(account: &ClientAccount) -> Result<&str> {
    account.session.tenant_token().context("缺少用户凭证")
}
fn validate_owner(account: &ClientAccount, intent: &Intent) -> Result<()> {
    ensure!(
        intent.tenant_id == account.session.tenant_id().as_str()
            && intent.request.pubkey == account.lsp_public_key,
        "通道意图与当前账户或 LSP 不一致"
    );
    ensure!(
        intent.request.funding_amount <= u128::from(u64::MAX),
        "保存的 CKB 金额超过容量范围"
    );
    if let Some(opening) = &intent.opening
        && account.session.binding(opening.channel_id.into()).is_none()
    {
        ensure!(
            intent.signed_funding.is_none(),
            "出资已签名但 SDK binding 缺失；停止恢复，原数据已保留"
        );
    }
    if let Some(saved) = &account.session.state().opening_request {
        ensure!(
            *saved == serde_json::to_vec(&intent.request)?,
            "SDK 原始请求与本地意图不一致"
        );
    }
    if let Some(signed) = &intent.signed_funding {
        let tx: packed::Transaction = signed.clone().into();
        let frozen: packed::Transaction = intent
            .opening
            .as_ref()
            .context("签名交易缺少冻结结果")?
            .unsigned_funding_tx
            .clone()
            .into();
        ensure!(
            tx.raw().as_slice() == frozen.raw().as_slice()
                && intent.funding_hash.as_ref() == Some(&ckb_types::H256::from(tx.calc_tx_hash())),
            "保存的签名交易、冻结交易或 hash 不一致"
        );
    }
    Ok(())
}

// Store the frozen response before the SDK installs the binding. A crash between
// these writes can retry the same request/response without losing the signer.
struct OpeningRpc<'a> {
    rpc: &'a Rpc,
    dir: &'a AccountDir,
}
#[async_trait::async_trait]
impl TenantOpeningRpc for OpeningRpc<'_> {
    async fn open_tenant_channel(
        &self,
        token: &str,
        request: OpenChannelWithExternalFundingParams,
    ) -> std::result::Result<OpenTenantChannelResult, fiber_lsp_sdk::SessionError> {
        async {
            let mut intent = read_intent(self.dir)?.context("缺少开通意图")?;
            if let Some(result) = intent.opening {
                return Ok(result);
            }
            let result = self.rpc.open_tenant_channel(token, request).await?;
            intent.opening = Some(result.clone());
            save_intent(self.dir, &intent)?;
            Ok::<_, anyhow::Error>(result)
        }
        .await
        .map_err(|e| fiber_lsp_sdk::SessionError::Invalid(format!("{e:#}")))
    }
}

pub async fn open(options: OpenOptions) -> Result<()> {
    let config = Config::load(options.config.as_deref())?;
    let mut account = ClientAccount::load(&config, options.account.as_deref()).await?;
    account.session = account.session.with_policy(SigningPolicy::Manual);
    let rpc = Rpc::new(&config.rpc_url)?;
    let result = tokio::time::timeout(
        Duration::from_secs(options.timeout),
        open_inner(&options, &mut account, &rpc),
    )
    .await;
    finish_command(result, &account.dir, "openchannel", options.config.as_ref())
}

async fn open_inner(options: &OpenOptions, account: &mut ClientAccount, rpc: &Rpc) -> Result<()> {
    let remote = rpc.list_all_channels(token(account)?).await?;
    let loaded = read_intent(&account.dir)?;
    if loaded.is_none() {
        ensure!(
            remote.channels.is_empty() && account.session.state().bindings.is_empty(),
            "当前租户已有通道记录但缺少本地意图；不能重新出资或重建账户"
        );
    }
    let mut intent = if let Some(intent) = loaded {
        validate_owner(account, &intent)?;
        if let Some(value) = options.funding_amount {
            ensure!(
                u128::from(value) == intent.request.funding_amount,
                "金额与原开通意图不一致；原记录已保留"
            );
        }
        ensure!(
            !matches!(
                intent.stage,
                Stage::Closed | Stage::CloseRequested | Stage::Closing
            ),
            "当前服务端 / SDK 限制同一租户关闭后再次开通；旧记录已保留，请用 ./lsp-cli listChannel 查询"
        );
        intent
    } else {
        let funding = options
            .funding_amount
            .context("首次开通必须提供 --funding-amount，单位 CKB")?;
        let wallet = Wallet::load(options.config.as_deref()).await?;
        ensure!(funding > RESERVE, "出资额必须大于 99 CKB 保留金额");
        ensure!(
            wallet.terms.auto_accept_min != 0 && funding >= wallet.terms.auto_accept_min,
            "未满足 Node3 自动接受阈值或自动接受已关闭；不能自动修改服务配置"
        );
        funding
            .checked_add(wallet.terms.remote_amount)
            .context("双方总注入额超过 CKB cell 容量范围")?;
        wallet.check_capacity(funding).await?;
        let material = account.session.allocate_pending_channel().await?;
        account.save()?;
        let request = OpenChannelWithExternalFundingParams {
            pubkey: account.lsp_public_key,
            funding_amount: u128::from(funding),
            public: Some(false),
            funding_udt_type_script: None,
            shutdown_script: wallet.terms.wallet_lock.clone(),
            funding_lock_script: wallet.terms.wallet_lock.clone(),
            funding_lock_script_cell_deps: None,
            commitment_delay_epoch: Some(
                ckb_types::core::EpochNumberWithFraction::new(1, 0, 1).into(),
            ),
            commitment_fee_rate: Some(wallet.terms.funding_fee_rate),
            funding_fee_rate: Some(wallet.terms.funding_fee_rate),
            tlc_expiry_delta: None,
            tlc_min_value: None,
            tlc_fee_proportional_millionths: None,
            max_tlc_value_in_flight: None,
            max_tlc_number_in_flight: None,
            external_channel_signer: Some(fiber_lsp_sdk::json::open_material_to_rpc(&material)),
        };
        let intent = Intent {
            format_version: 1,
            tenant_id: account.session.tenant_id().as_str().to_owned(),
            terms: wallet.terms,
            request,
            stage: Stage::Opening,
            opening: None,
            signed_funding: None,
            funding_hash: None,
            funding_fee: None,
            close_authorization: None,
            close_transaction: None,
            close_hash: None,
            pending_signature: None,
            pending_status: None,
        };
        save_intent(&account.dir, &intent)?;
        intent
    };
    let wallet = Wallet::from_terms(intent.terms.clone())?;
    wallet.check_network().await?;
    println!(
        "本方注入 {} CKB，对方注入 {} CKB；共用 #5 总注入 {} CKB，另付交易费（上限 {} CKB）",
        amount(intent.request.funding_amount),
        amount(u128::from(intent.terms.remote_amount)),
        amount(intent.request.funding_amount + u128::from(intent.terms.remote_amount)),
        amount(u128::from(intent.terms.max_fee))
    );
    if intent.signed_funding.is_none() {
        // Identical SDK retry restores a pending opening. The adapter saves the response.
        let adapter = OpeningRpc {
            rpc,
            dir: &account.dir,
        };
        let opening = account
            .session
            .open_tenant_channel(
                intent.request.clone(),
                &wallet.terms.network,
                &adapter,
                &wallet,
                &account.dir,
            )
            .await?;
        intent = read_intent(&account.dir)?.context("开通意图丢失")?;
        intent.opening = Some(opening.clone());
        let points = wallet.verify(&intent.request, &opening, true).await?;
        let tx: packed::Transaction = opening.unsigned_funding_tx.clone().into();
        let mut total = 0u128;
        for point in points {
            total += u128::from(u64::from(
                wallet.input_cell(&point, true).await?.0.capacity(),
            ));
        }
        let outputs: u128 = tx
            .raw()
            .outputs()
            .into_iter()
            .map(|o| u128::from(u64::from(o.capacity())))
            .sum();
        intent.funding_fee = Some(u64::try_from(
            total.checked_sub(outputs).context("交易容量不一致")?,
        )?);
        let signed = wallet.sign(tx)?;
        intent.funding_hash = Some(signed.calc_tx_hash().unpack());
        intent.signed_funding = Some(signed.into());
        intent.stage = Stage::FundingSaved;
        save_intent(&account.dir, &intent)?;
    }
    let id = intent.opening.as_ref().context("缺少冻结交易")?.channel_id;
    ensure!(
        account.session.binding(id.into()).is_some(),
        "SDK binding 缺失"
    );
    loop {
        let channels = rpc.list_all_channels(token(account)?).await?;
        let channel = find_channel(&channels.channels, id)?;
        ensure!(
            channel.pubkey == account.lsp_public_key && !channel.is_public,
            "远端通道归属或类型不一致"
        );
        ensure!(
            channel.failure_detail.is_none()
                && !matches!(
                    channel.state,
                    ChannelState::Closed(_) | ChannelState::Stale | ChannelState::ShuttingDown(_)
                ),
            "通道失败、状态过期或已在关闭中；原意图保留"
        );
        let hash = intent.funding_hash.as_ref().context("缺少出资 hash")?;
        if channel.state == ChannelState::ChannelReady {
            let confirmed = wallet
                .committed_transaction(hash)
                .await?
                .context("远端通道可用，但出资交易尚未链上确认")?;
            let original: packed::Transaction = intent.signed_funding.clone().unwrap().into();
            ensure!(
                confirmed.raw().as_slice() == original.raw().as_slice(),
                "链上出资交易与原意图不一致"
            );
            check_balances(channel, &intent)?;
            intent.stage = Stage::Ready;
            save_intent(&account.dir, &intent)?;
            println!(
                "开通成功（ChannelReady，链上出资已确认）\nchannel ID：{id}\n出资交易：{hash:#x}\n实际出资费：{} shannons",
                intent.funding_fee.unwrap()
            );
            return Ok(());
        }
        // Query both the tenant state and chain before any re-submission. If the
        // server moved beyond external funding, process signatures instead.
        let external = serde_json::to_string(&channel.state)?.contains("AWAITING_EXTERNAL_FUNDING");
        submit_funding(account, rpc, &wallet, &mut intent, external).await?;
        sign_status(account, rpc, &wallet, &mut intent, false).await?;
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn submit_funding(
    account: &ClientAccount,
    rpc: &Rpc,
    wallet: &Wallet,
    intent: &mut Intent,
    external: bool,
) -> Result<()> {
    let id = intent.opening.as_ref().context("缺少冻结交易")?.channel_id;
    let hash = intent.funding_hash.as_ref().context("缺少出资 hash")?;
    if external && !wallet.funding_known(hash).await? {
        let opening = intent.opening.as_ref().unwrap();
        wallet.verify(&intent.request, opening, true).await?;
        let submitted: SubmitSignedFundingTxResult = rpc
            .call(
                "submit_signed_funding_tx",
                json!([SubmitSignedFundingTxParams {
                    channel_id: id,
                    signed_funding_tx: intent.signed_funding.clone().unwrap()
                }]),
                token(account)?,
            )
            .await?;
        ensure!(
            submitted.channel_id == id
                && ckb_types::H256::from(submitted.funding_tx_hash.0) == *hash,
            "出资提交响应与原交易不一致"
        );
        intent.stage = Stage::FundingSubmitted;
        save_intent(&account.dir, intent)?;
    }
    Ok(())
}

fn find_channel(channels: &[Channel], id: Hash256) -> Result<&Channel> {
    channels
        .iter()
        .find(|c| c.channel_id == id)
        .context("当前账户没有指定通道；未操作其他租户通道")
}
fn check_balances(channel: &Channel, intent: &Intent) -> Result<()> {
    let opening = intent.opening.as_ref().context("缺少已验证开通条款")?;
    ensure!(
        channel.pending_tlcs.is_empty()
            && channel.offered_tlc_balance == 0
            && channel.received_tlc_balance == 0,
        "存在未结算 TLC；拒绝关闭或签名"
    );
    ensure!(
        channel.local_balance.checked_add(u128::from(
            opening.opening_context.local_reserved_ckb_amount
        )) == Some(intent.request.funding_amount)
            && channel.remote_balance.checked_add(u128::from(
                opening.opening_context.remote_reserved_ckb_amount
            )) == Some(u128::from(intent.terms.remote_amount)),
        "远端余额与本地已验证余额不一致；状态待核对"
    );
    Ok(())
}

async fn sign_status(
    account: &mut ClientAccount,
    rpc: &Rpc,
    wallet: &Wallet,
    intent: &mut Intent,
    closing: bool,
) -> Result<()> {
    let id = intent.opening.as_ref().context("缺少冻结结果")?.channel_id;
    let status: GetChannelSigningStatusResult = rpc
        .call(
            "get_channel_signing_status",
            json!([{ "channel_id": id }]),
            token(account)?,
        )
        .await?;
    ensure!(status.channel_id == id, "签名状态属于其他通道");
    let ChannelSigningStatus::SignatureRequired {
        request_id,
        transition,
        ref content,
        ref settlement,
        ..
    } = status.status
    else {
        return Ok(());
    };
    match transition {
        ChannelSigningTransition::SendCommitmentSigned
        | ChannelSigningTransition::CompleteReceivedCommitment => {
            ensure!(
                !closing && content.slot.commitment_number == 1,
                "本地开通意图不授权后续状态变更"
            );
            let settlement = settlement
                .as_ref()
                .context("初始 commitment 缺少余额快照")?;
            ensure!(
                settlement.tlcs.is_empty()
                    && settlement.local_amount == intent.request.funding_amount
                    && settlement.remote_amount == u128::from(intent.terms.remote_amount),
                "签名余额或 TLC 与开通意图不符"
            );
        }
        ChannelSigningTransition::SendClosingSigned => {
            ensure!(
                closing && intent.close_authorization.is_some(),
                "缺少本地协作关闭授权"
            );
            let fiber_json_types::Musig2SignableContent::CooperativeCloseTransaction {
                transaction,
            } = &content.content
            else {
                bail!("关闭签名内容类型不一致");
            };
            let tx: packed::Transaction = transaction.clone().into();
            validate_close_tx(&tx, intent, wallet)?;
            if let Some(saved) = &intent.close_transaction {
                ensure!(
                    packed::Transaction::from(saved.clone()).raw().as_slice()
                        == tx.raw().as_slice(),
                    "关闭交易改变了原条款"
                );
            }
            intent.close_hash = Some(tx.calc_tx_hash().unpack());
            intent.close_transaction = Some(transaction.clone());
            intent.stage = Stage::Closing;
            save_intent(&account.dir, intent)?;
        }
        _ => bail!("本地开关通道意图不授权此签名请求"),
    }
    let params = if let (Some(saved), Some(saved_status)) =
        (&intent.pending_signature, &intent.pending_status)
        && saved.request_id == request_id
    {
        ensure!(
            serde_json::to_vec(saved_status)? == serde_json::to_vec(&status.status)?,
            "同一 request ID 的签名内容改变；拒绝复用或生成冲突签名"
        );
        ensure!(saved.channel_id == id, "保存的签名响应属于其他通道");
        Some(saved.clone())
    } else {
        None
    };
    let params = match params {
        Some(params) => params,
        None => {
            let now = u64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())?;
            let outcome = account
                .session
                .handle_channel_status(id.into(), status.status.clone(), now)
                .await?;
            let submit = match outcome {
                ProcessOutcome::NeedConfirmation(pending) => {
                    account.session.confirm(pending, now).await?
                }
                ProcessOutcome::ReadyToSubmit(params) => params,
                ProcessOutcome::Idle => return Ok(()),
                ProcessOutcome::Denied => bail!("SDK 拒绝本操作的签名请求"),
            };
            let SubmitParams::Channel(params) = submit else {
                bail!("开关通道不授权 watchtower 签名");
            };
            account.save()?;
            intent.pending_signature = Some(params.clone());
            intent.pending_status = Some(status.status.clone());
            save_intent(&account.dir, intent)?;
            params
        }
    };
    let _: serde_json::Value = rpc
        .call("submit_channel_signature", json!([params]), token(account)?)
        .await?;
    // Retain the last response. The next status decides whether to reuse it or
    // prepare a new request from the SDK's durable signing history.
    Ok(())
}

pub async fn close(options: CloseOptions) -> Result<()> {
    let config = Config::load(options.config.as_deref())?;
    let mut account = ClientAccount::load(&config, options.account.as_deref()).await?;
    account.session = account.session.with_policy(SigningPolicy::Manual);
    let rpc = Rpc::new(&config.rpc_url)?;
    let result = tokio::time::timeout(
        Duration::from_secs(options.timeout),
        close_inner(&options, &mut account, &rpc),
    )
    .await;
    finish_command(
        result,
        &account.dir,
        "closechannel",
        options.config.as_ref(),
    )
}
async fn close_inner(options: &CloseOptions, account: &mut ClientAccount, rpc: &Rpc) -> Result<()> {
    let channels = rpc.list_all_channels(token(account)?).await?;
    if channels.channels.is_empty() {
        ensure!(options.channel_id.is_none(), "当前账户没有指定通道");
        let intent = read_intent(&account.dir)?;
        ensure!(
            intent.is_none(),
            "本地有开通意图但远端尚无通道；先恢复 ./lsp-cli openchannel"
        );
        println!("当前账户没有活动通道");
        return Ok(());
    }
    let mut intent =
        read_intent(&account.dir)?.context("缺少已验证本地通道意图；不能授权退款或重建账户")?;
    validate_owner(account, &intent)?;
    let id = intent
        .opening
        .as_ref()
        .context("开通未完成；先恢复 ./lsp-cli openchannel")?
        .channel_id;
    ensure!(
        options.channel_id.is_none_or(|value| value == id),
        "指定 channel ID 不属于当前账户本地 binding"
    );
    ensure!(
        account.session.binding(id.into()).is_some(),
        "通道不属于当前账户 SDK binding"
    );
    let wallet = Wallet::from_terms(intent.terms.clone())?;
    wallet.check_network().await?;
    let channel = find_channel(&channels.channels, id)?;
    if intent.stage == Stage::Closed {
        verify_closed(channel, &intent, &wallet)
            .await?
            .context("关闭交易尚未链上确认")?;
        println!(
            "通道已关闭\nchannel ID：{id}\n关闭交易：{:#x}",
            intent.close_hash.unwrap()
        );
        return Ok(());
    }
    if intent.close_authorization.is_none() {
        ensure!(
            channel.state == ChannelState::ChannelReady && intent.stage == Stage::Ready,
            "开通未完成或状态过期；先恢复 ./lsp-cli openchannel"
        );
        check_balances(channel, &intent)?;
        let hash = intent.funding_hash.as_ref().context("缺少出资 hash")?;
        let tx = wallet
            .committed_transaction(hash)
            .await?
            .context("出资未确认，不能关闭")?;
        let opening = intent.opening.as_ref().unwrap();
        let original: packed::Transaction = opening.unsigned_funding_tx.clone().into();
        ensure!(
            tx.raw().as_slice() == original.raw().as_slice(),
            "链上出资与 binding 不一致"
        );
        let point = packed::OutPoint::new_builder()
            .tx_hash(tx.calc_tx_hash())
            .index(0u32)
            .build();
        ensure!(
            channel.channel_outpoint.as_ref() == Some(&point),
            "远端通道出资 outpoint 不匹配"
        );
        let cell = wallet.input_cell(&point, true).await?;
        ensure!(
            cell.0 == tx.raw().outputs().get(0).unwrap() && cell.1.is_empty(),
            "通道链上资产不匹配"
        );
        let fee = wallet.close_fee(
            opening
                .opening_context
                .remote_shutdown_script
                .clone()
                .into(),
        )?;
        intent.close_authorization = Some(CloseAuthorization {
            fee,
            local_fee_share: fee,
            remote_fee_share: 0,
        });
        // Only initial balances are supported. Compare both SDK-verified lanes,
        // rather than trusting the RPC balance projection before sending shutdown.
        let signer = account.session.channel_signer(id.into()).await?;
        let records = signer.recovery_records().await?;
        for lane in [false, true] {
            let record = records
                .iter()
                .filter(|r| r.reference.for_remote == lane)
                .max_by_key(|r| r.reference.version)
                .context("缺少已验证 commitment；不能关闭")?;
            ensure!(
                record.settlement.data.tlcs.is_empty()
                    && record.settlement.data.local_amount == intent.request.funding_amount
                    && record.settlement.data.remote_amount
                        == u128::from(intent.terms.remote_amount),
                "本地 commitment 余额不一致或有未结算 TLC"
            );
        }
        intent.stage = Stage::CloseRequested;
        save_intent(&account.dir, &intent)?;
    }
    account
        .session
        .channel_signer(id.into())
        .await?
        .authorize_close(intent.close_authorization.clone().unwrap())
        .await?;
    loop {
        let channels = rpc.list_all_channels(token(account)?).await?;
        let channel = find_channel(&channels.channels, id)?;
        ensure!(
            channel.pubkey == account.lsp_public_key && !channel.is_public,
            "远端通道归属或类型不一致"
        );
        if let Some(hash) = verify_closed(channel, &intent, &wallet).await? {
            intent.close_hash = Some(hash.clone());
            intent.stage = Stage::Closed;
            save_intent(&account.dir, &intent)?;
            println!(
                "协作关闭成功（链上确认，#5 退款已核对）\nchannel ID：{id}\n关闭交易：{hash:#x}\n实际关闭费：{} shannons",
                intent.close_authorization.as_ref().unwrap().fee
            );
            return Ok(());
        }
        match channel.state {
            ChannelState::ChannelReady => {
                request_shutdown(account, rpc, &intent, channel).await?;
            }
            ChannelState::ShuttingDown(_) => {
                sign_status(account, rpc, &wallet, &mut intent, true).await?;
            }
            ChannelState::Closed(_) => {}
            _ => bail!("通道开通未完成、失败或状态过期；不能协作关闭"),
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn request_shutdown(
    account: &ClientAccount,
    rpc: &Rpc,
    intent: &Intent,
    channel: &Channel,
) -> Result<()> {
    if channel.state != ChannelState::ChannelReady {
        return Ok(());
    }
    let id = intent.opening.as_ref().context("缺少本地通道")?.channel_id;
    ensure!(
        channel.channel_id == id && intent.close_authorization.is_some(),
        "缺少原关闭意图或通道不匹配"
    );
    check_balances(channel, intent)?;
    let _: serde_json::Value = rpc
        .call(
            "shutdown_channel",
            json!([ShutdownChannelParams {
                channel_id: id,
                close_script: Some(intent.terms.wallet_lock.clone()),
                fee_rate: Some(intent.terms.close_fee_rate),
                force: Some(false)
            }]),
            token(account)?,
        )
        .await?;
    Ok(())
}

fn validate_close_tx(tx: &packed::Transaction, intent: &Intent, wallet: &Wallet) -> Result<()> {
    let opening = intent.opening.as_ref().context("缺少原始开通条款")?;
    let raw = tx.raw();
    let funding: packed::Transaction = opening.unsigned_funding_tx.clone().into();
    let expected_point = packed::OutPoint::new_builder()
        .tx_hash(funding.calc_tx_hash())
        .index(0u32)
        .build();
    ensure!(
        raw.version() == 0u32.pack()
            && raw.inputs().len() == 1
            && raw.inputs().get(0).unwrap().previous_output() == expected_point
            && raw.inputs().get(0).unwrap().since() == 0u64.pack()
            && raw.header_deps().is_empty(),
        "关闭交易 input 或结构不一致"
    );
    let deps: Vec<packed::CellDep> = intent
        .terms
        .funding_deps
        .iter()
        .cloned()
        .map(Into::into)
        .collect();
    ensure!(
        raw.cell_deps().into_iter().collect::<Vec<_>>() == deps,
        "关闭合约 deps 不匹配"
    );
    let auth = intent
        .close_authorization
        .as_ref()
        .context("缺少关闭授权")?;
    let fee = wallet.close_fee(
        opening
            .opening_context
            .remote_shutdown_script
            .clone()
            .into(),
    )?;
    ensure!(
        auth.fee == fee && auth.local_fee_share == fee && auth.remote_fee_share == 0,
        "关闭费用分摊与本地计算不符"
    );
    ensure!(
        raw.outputs().len() == 2 && raw.outputs_data().len() == 2,
        "退款 outputs 数量不符"
    );
    let mut expected = vec![
        packed::CellOutput::new_builder()
            .capacity(
                u64::try_from(intent.request.funding_amount)?
                    .checked_sub(fee)
                    .context("关闭费用超过余额")?,
            )
            .lock(intent.terms.wallet_lock.clone())
            .build(),
        packed::CellOutput::new_builder()
            .capacity(intent.terms.remote_amount)
            .lock(opening.opening_context.remote_shutdown_script.clone())
            .build(),
    ];
    for (index, output) in raw.outputs().into_iter().enumerate() {
        ensure!(
            raw.outputs_data().get(index).unwrap().is_empty(),
            "退款资产数据不一致"
        );
        let index = expected
            .iter()
            .position(|e| *e == output)
            .context("退款金额或脚本与本地条款不符")?;
        expected.remove(index);
    }
    Ok(())
}
async fn verify_closed(
    channel: &Channel,
    intent: &Intent,
    wallet: &Wallet,
) -> Result<Option<ckb_types::H256>> {
    let Some(hash) = &channel.shutdown_transaction_hash else {
        return Ok(None);
    };
    if !matches!(channel.state, ChannelState::Closed(_)) {
        return Ok(None);
    }
    let Some(tx) = wallet.committed_transaction(hash).await? else {
        return Ok(None);
    };
    validate_close_tx(&tx, intent, wallet)?;
    if let Some(saved) = &intent.close_transaction {
        ensure!(
            tx.raw().as_slice() == packed::Transaction::from(saved.clone()).raw().as_slice(),
            "链上关闭交易与已签名交易不一致"
        );
    }
    ensure!(
        intent
            .close_hash
            .as_ref()
            .is_none_or(|expected| expected == hash),
        "关闭 hash 不一致"
    );
    Ok(Some(hash.clone()))
}

fn finish_command(
    result: std::result::Result<Result<()>, tokio::time::error::Elapsed>,
    dir: &AccountDir,
    command: &str,
    config: Option<&PathBuf>,
) -> Result<()> {
    let error = match result {
        Ok(Ok(())) => return Ok(()),
        Ok(Err(error)) => error,
        Err(_) => anyhow::anyhow!("等待超时，操作尚未完成"),
    };
    let stage = read_intent(dir)?
        .map(|i| format!("{:?}", i.stage))
        .unwrap_or_else(|| "尚未保存意图".into());
    let account = dir.path.file_name().unwrap().to_string_lossy();
    let config = config
        .map(|p| format!(" --config {}", p.display()))
        .unwrap_or_default();
    if command == "openchannel"
        && matches!(
            read_intent(dir)?.map(|i| i.stage),
            Some(Stage::Closed | Stage::Closing | Stage::CloseRequested)
        )
    {
        return Err(error.context(format!("当前阶段：{stage}；旧记录保留。查询：./lsp-cli listChannel --account {account}{config}；账户管理：./lsp-cli list-accounts / archive-account --account {account}")));
    }
    Err(error.context(format!("当前阶段：{stage}；原数据保留。恢复命令：./lsp-cli {command} --account {account}{config}；查询：./lsp-cli listChannel --account {account}{config}")))
}

pub async fn list(config: &Config, selected: Option<&str>) -> Result<()> {
    let account = ClientAccount::load(config, selected).await?;
    let rpc = Rpc::new(&config.rpc_url)?;
    let channels = rpc.list_all_channels(token(&account)?).await?;
    let intent = read_intent(&account.dir)?;
    if let Some(intent) = &intent {
        validate_owner(&account, intent)?;
    }
    println!("tenant_id：{}", account.session.tenant_id());
    if channels.channels.is_empty() {
        println!("当前账户通道列表为空");
    }
    for channel in &channels.channels {
        let mut state = match channel.state {
            ChannelState::ChannelReady => "可用（远端；链上待核对）",
            ChannelState::ShuttingDown(_) => "关闭中",
            ChannelState::Closed(_) => "已关闭（远端；链上待核对）",
            ChannelState::Stale => "状态过期",
            _ => "开通中",
        };
        if channel.failure_detail.is_some() {
            state = "失败";
        }
        if let Some(intent) = &intent
            && intent
                .opening
                .as_ref()
                .is_some_and(|o| o.channel_id == channel.channel_id)
        {
            let wallet = Wallet::from_terms(intent.terms.clone())?;
            wallet.check_network().await?;
            if channel.state == ChannelState::ChannelReady {
                if let Some(hash) = &intent.funding_hash
                    && let Some(tx) = wallet.committed_transaction(hash).await?
                {
                    let funding: packed::Transaction = intent
                        .opening
                        .as_ref()
                        .unwrap()
                        .unsigned_funding_tx
                        .clone()
                        .into();
                    if tx.raw().as_slice() == funding.raw().as_slice() {
                        state = "可用（链上出资已确认）";
                    }
                }
            } else if matches!(channel.state, ChannelState::Closed(_))
                && intent.close_authorization.is_some()
                && verify_closed(channel, intent, &wallet).await?.is_some()
            {
                state = "已关闭（链上退款已核对）";
            }
        }
        println!(
            "channel ID：{}\n状态：{}\n双方通道余额：{} / {} CKB",
            channel.channel_id,
            state,
            amount(channel.local_balance),
            amount(channel.remote_balance)
        );
        if let Some(point) = &channel.channel_outpoint {
            let hash: ckb_types::H256 = point.tx_hash().unpack();
            println!("出资交易：{hash:#x}");
        }
        if let Some(hash) = &channel.shutdown_transaction_hash {
            println!("关闭交易：{hash:#x}");
        }
    }
    if let Some(intent) = intent {
        println!("本地阶段：{:?}", intent.stage);
        if !matches!(intent.stage, Stage::Ready | Stage::Closed) {
            println!(
                "待恢复：./lsp-cli {} --account {}",
                if intent.close_authorization.is_some() {
                    "closechannel"
                } else {
                    "openchannel"
                },
                account.dir.path.file_name().unwrap().to_string_lossy()
            );
        }
        if intent.stage == Stage::Closed {
            println!("再次开通受当前服务端 / SDK 的同租户单通道记录限制；旧记录保留");
        }
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
        list(&config, None).await.unwrap();
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
                    .lock(record.terms.wallet_lock.clone())
                    .build(),
            )
            .output_data(packed::Bytes::default())
            .build()
            .data()
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
}
