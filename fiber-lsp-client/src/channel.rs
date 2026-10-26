//! One durable lifecycle intent per hosted tenant (the current SDK/server limit).
use crate::{
    account::ClientAccount,
    rpc::{Connection, Rpc},
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
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Args)]
#[group(skip)]
pub struct OpenOptions {
    #[command(flatten)]
    pub connection: crate::ConnectionOptions,
    /// 仅历史关闭入口
    #[arg(long, value_name = "ID", hide = true)]
    pub account: Option<String>,
    #[arg(short = 'u', long, value_parser = clap::value_parser!(u8).range(1..=4), required = true, conflicts_with = "account")]
    pub user: Option<u8>,
    /// 本方总注入额（CKB，含保留金额），最多八位小数；首次开通默认 1000，恢复时沿用原金额
    #[arg(long, value_parser = parse_amount)]
    pub funding_amount: Option<u64>,
    /// 等待最终状态的秒数；超时保留进度
    #[arg(long, default_value_t = 120, value_parser = clap::value_parser!(u64).range(1..))]
    pub timeout: u64,
}

#[derive(Args)]
#[group(skip)]
#[command(group(clap::ArgGroup::new("close_selector").args(["user", "account"]).required(true).multiple(false)))]
pub struct CloseOptions {
    #[command(flatten)]
    pub connection: crate::ConnectionOptions,
    /// 兼容旧数据的内部身份 ID；日常历史操作使用 -u N --round R
    #[arg(long, value_name = "ID")]
    pub account: Option<String>,
    /// 固定账户编号 1～4
    #[arg(short = 'u', long, value_parser = clap::value_parser!(u8).range(1..=4), conflicts_with = "account")]
    pub user: Option<u8>,
    /// 选择固定账户的历史轮次（accounts --history 展示）
    #[arg(long, value_name = "R", value_parser = clap::value_parser!(u32).range(1..), requires = "user", conflicts_with = "account")]
    pub round: Option<u32>,
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
    #[serde(default)]
    close_balances: Option<(u128, u128)>,
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
    let client = crate::ClientOptions {
        selector: crate::Selector {
            user: options.user,
            account: options.account.clone(),
        },
        connection: options.connection.clone(),
        round: None,
    };
    client.current_only()?;
    let root = crate::accounts::root();
    let original_binding = crate::accounts::funding_for(&root, options.user.unwrap())?;
    ensure!(
        crate::wallet::public_binding(original_binding.offckb_index)? == original_binding,
        "用户钱包公开绑定不一致；未分配身份或注册"
    );
    let (dir, selection) = crate::accounts::choose(&root, &client, true, false, true)?;
    let config = Connection::connect(&options.connection)?;
    let binding = selection.funding.as_ref().unwrap();
    ensure!(
        original_binding == *binding,
        "用户钱包公开绑定与运行时不一致；尚未注册"
    );
    let budget = tokio::time::Instant::now() + Duration::from_secs(options.timeout);
    let mut account = tokio::time::timeout_at(budget, crate::account::run_dir(&config, true, dir))
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "注册等待超时；恢复：./lsp-cli create-channel {}",
                client.recovery_selector()
            )
        })??
        .client;
    account.session = account.session.with_policy(SigningPolicy::Manual);
    let rpc = Rpc::new(&config.rpc_url)?;
    let result = tokio::time::timeout_at(
        budget,
        open_bound(
            &options,
            &mut account,
            &rpc,
            selection.funding.as_ref().unwrap(),
        ),
    )
    .await;
    finish_command(result, &account.dir, "create-channel", Some(&client))
}

async fn open_bound(
    options: &OpenOptions,
    account: &mut ClientAccount,
    rpc: &Rpc,
    binding: &crate::accounts::FundingBinding,
) -> Result<()> {
    let current = crate::wallet::public_binding(binding.offckb_index)?;
    ensure!(&current == binding, "用户固定钱包绑定不匹配");
    if let Some(intent) = read_intent(&account.dir)? {
        ensure!(
            intent.terms.wallet_account == binding.offckb_index
                && intent.terms.wallet_lock == binding.lock_script,
            "原通道条款与用户永久钱包绑定不一致"
        );
    }
    open_engine(options, account, rpc, Some(binding)).await
}

async fn open_engine(
    options: &OpenOptions,
    account: &mut ClientAccount,
    rpc: &Rpc,
    binding: Option<&crate::accounts::FundingBinding>,
) -> Result<()> {
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
            "当前服务端 / SDK 限制同一租户关闭后再次开通；旧记录已保留，请用 ./lsp-cli status 查询"
        );
        intent
    } else {
        let funding = options.funding_amount.unwrap_or(1000 * CKB);
        if options.funding_amount.is_none() {
            println!(
                "未指定 --funding-amount，本次默认出资 {} CKB",
                amount(u128::from(funding))
            );
        }
        let connection = Connection::connect(&options.connection)?;
        let bootstrap = connection.registration_token()?;
        let node: fiber_json_types::NodeInfoResult =
            rpc.call("node_info", json!([]), &bootstrap).await?;
        ensure!(
            node.pubkey == account.lsp_public_key,
            "LSP 出资条款公钥与账户绑定不一致"
        );
        let wallet = Wallet::load(binding.context("新开通缺少用户钱包绑定")?, &node).await?;
        ensure!(funding > RESERVE, "出资额必须大于 99 CKB 保留金额");
        ensure!(
            wallet.terms.auto_accept_min != 0 && funding >= wallet.terms.auto_accept_min,
            "未满足 LSP 自动接受阈值或自动接受已关闭；不能自动修改服务配置"
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
            close_balances: None,
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
        "本方 #{} 注入 {} CKB，对方注入 {} CKB；通道总注入 {} CKB，另付交易费（上限 {} CKB）",
        intent.terms.wallet_account,
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
        let mut cells = Vec::new();
        for point in points {
            let cell = wallet.input_cell(&point, true).await?;
            total += u128::from(u64::from(cell.0.capacity()));
            cells.push(cell);
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
        let signed = wallet.sign(tx, &cells)?;
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
            if intent.stage != Stage::Ready {
                check_balances(channel, &intent)?;
            }
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
    let (local, remote) = intent.close_balances.unwrap_or((
        intent.request.funding_amount,
        u128::from(intent.terms.remote_amount),
    ));
    ensure!(
        channel.local_balance.checked_add(u128::from(
            opening.opening_context.local_reserved_ckb_amount
        )) == Some(local)
            && channel.remote_balance.checked_add(u128::from(
                opening.opening_context.remote_reserved_ckb_amount
            )) == Some(remote),
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
    let client = crate::ClientOptions {
        selector: crate::Selector {
            user: options.user,
            account: options.account.clone(),
        },
        connection: options.connection.clone(),
        round: options.round,
    };
    let root = crate::accounts::root();
    let (dir, selection) = crate::accounts::choose(&root, &client, false, true, false)?;
    let config = Connection::connect(&options.connection)?;
    crate::accounts::check_historical_close(&config.root, &selection)?;
    let local: serde_json::Value = dir.read_json()?;
    if local["registration"] != "registered" && read_intent(&dir)?.is_none() {
        ensure!(options.channel_id.is_none(), "当前身份没有指定通道");
        println!("当前身份尚未开通；没有可关闭通道，未归档");
        return Ok(());
    }
    let mut account = ClientAccount::load_dir(&config, dir).await?;
    account.session = account.session.with_policy(SigningPolicy::Manual);
    let rpc = Rpc::new(&config.rpc_url)?;
    let result = tokio::time::timeout(
        Duration::from_secs(options.timeout),
        close_inner(&options, &mut account, &rpc),
    )
    .await;
    let outcome = finish_command(result, &account.dir, "close-channel", Some(&client))?;
    drop(account);
    if let CloseResult::ClosedVerified { channel_id, hash } = outcome {
        crate::accounts::archive_verified(&config.root, &selection, channel_id, &hash)
            .with_context(|| {
                format!(
                    "资金已关闭，归档待恢复；执行 ./lsp-cli close-channel {}",
                    client.recovery_selector()
                )
            })?;
    }
    Ok(())
}

#[derive(Debug)]
pub enum CloseResult {
    NoChannel,
    ClosedVerified {
        channel_id: Hash256,
        hash: ckb_types::H256,
    },
}

async fn close_inner(
    options: &CloseOptions,
    account: &mut ClientAccount,
    rpc: &Rpc,
) -> Result<CloseResult> {
    let channels = rpc.list_all_channels(token(account)?).await?;
    if channels.channels.is_empty() {
        ensure!(options.channel_id.is_none(), "当前账户没有指定通道");
        let intent = read_intent(&account.dir)?;
        ensure!(
            intent.is_none(),
            "本地有开通意图但远端尚无通道；先恢复 ./lsp-cli create-channel -u N"
        );
        println!("当前账户没有活动通道");
        return Ok(CloseResult::NoChannel);
    }
    let mut intent =
        read_intent(&account.dir)?.context("缺少已验证本地通道意图；不能授权退款或重建账户")?;
    validate_owner(account, &intent)?;
    let id = intent
        .opening
        .as_ref()
        .context("开通未完成；先恢复 ./lsp-cli create-channel -u N")?
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
            intent.close_hash.as_ref().unwrap()
        );
        return Ok(CloseResult::ClosedVerified {
            channel_id: id,
            hash: intent.close_hash.clone().unwrap(),
        });
    }
    if intent.close_authorization.is_none() {
        ensure!(
            channel.state == ChannelState::ChannelReady && intent.stage == Stage::Ready,
            "开通未完成或状态过期；先恢复 ./lsp-cli create-channel -u N"
        );
        let signer = account.session.channel_signer(id.into()).await?;
        let records = signer.recovery_records().await?;
        let (local, remote) = settled_balances(&records)?;
        intent.close_balances = Some((local, remote));
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
                "协作关闭成功（链上确认，双方退款已核对）\nchannel ID：{id}\n关闭交易：{hash:#x}\n实际关闭费：{} shannons",
                intent.close_authorization.as_ref().unwrap().fee
            );
            return Ok(CloseResult::ClosedVerified {
                channel_id: id,
                hash,
            });
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
                u64::try_from(
                    intent
                        .close_balances
                        .map_or(intent.request.funding_amount, |v| v.0),
                )?
                .checked_sub(fee)
                .context("关闭费用超过余额")?,
            )
            .lock(intent.terms.wallet_lock.clone())
            .build(),
        packed::CellOutput::new_builder()
            .capacity(u64::try_from(
                intent
                    .close_balances
                    .map_or(u128::from(intent.terms.remote_amount), |v| v.1),
            )?)
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

fn finish_command<T>(
    result: std::result::Result<Result<T>, tokio::time::error::Elapsed>,
    dir: &AccountDir,
    command: &str,
    selector: Option<&crate::ClientOptions>,
) -> Result<T> {
    let error = match result {
        Ok(Ok(value)) => return Ok(value),
        Ok(Err(error)) => error,
        Err(_) => anyhow::anyhow!("等待超时，操作尚未完成"),
    };
    let stage = read_intent(dir)?
        .map(|i| format!("{:?}", i.stage))
        .unwrap_or_else(|| "尚未保存意图".into());
    let account = dir.path.file_name().unwrap().to_string_lossy();
    let selector = selector.map_or_else(
        || format!("--account {account}"),
        crate::ClientOptions::recovery_selector,
    );
    Err(error.context(format!("当前阶段：{stage}；原数据保留。恢复命令：./lsp-cli {command} {selector}；查询：./lsp-cli status {selector}")))
}

pub fn count(channels: &[Channel]) -> &'static str {
    match channels.first() {
        None => "0",
        Some(channel) if matches!(channel.state, ChannelState::Closed(_)) => "1（已关闭）",
        Some(_) => "1",
    }
}

pub async fn print_details(account: &ClientAccount, channels: &[Channel]) -> Result<()> {
    let intent = read_intent(&account.dir)?;
    if let Some(intent) = &intent {
        validate_owner(account, intent)?;
    }
    if channels.is_empty() {
        println!(
            "\n通道状态：{}",
            if intent.is_some() {
                "暂无远端通道记录"
            } else {
                "未创建"
            }
        );
    }
    for channel in channels {
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
            "\n通道状态：{}\n  channel ID：{}\n  双方通道余额：{} / {} CKB",
            state,
            channel.channel_id,
            amount(channel.local_balance),
            amount(channel.remote_balance)
        );
        if let Some(point) = &channel.channel_outpoint {
            let hash: ckb_types::H256 = point.tx_hash().unpack();
            println!("  出资交易：{hash:#x}");
        }
        if let Some(hash) = &channel.shutdown_transaction_hash {
            println!("  关闭交易：{hash:#x}");
        }
    }
    if let Some(intent) = intent {
        println!("  当前状态：{:?}", intent.stage);
        if !matches!(intent.stage, Stage::Ready | Stage::Closed) {
            println!(
                "  待恢复：./lsp-cli {} {}",
                if intent.close_authorization.is_some() {
                    "close-channel"
                } else {
                    "create-channel"
                },
                {
                    let id = account.dir.path.file_name().unwrap().to_string_lossy();
                    crate::accounts::owner_for(&crate::accounts::root(), &id)?
                        .map_or_else(|| format!("--account {id}"), |u| format!("-u {u}"))
                }
            );
        }
        if intent.stage == Stage::Closed {
            println!("  再次开通受当前服务端 / SDK 的同租户单通道记录限制；旧记录保留");
        }
    }
    Ok(())
}

/// The original verified opening context; no new wallet identity or funding.
pub fn payment_context(account: &ClientAccount) -> Result<(Hash256, Wallet, u64, u64)> {
    let intent = read_intent(&account.dir)?.context("缺少已验证开通条款")?;
    validate_owner(account, &intent)?;
    ensure!(
        intent.stage == Stage::Ready,
        "通道开通未完成或正在关闭；先核对通道生命周期"
    );
    let opening = intent.opening.context("缺少开通结果")?;
    Ok((
        opening.channel_id,
        Wallet::from_terms(intent.terms)?,
        opening.opening_context.local_reserved_ckb_amount,
        opening.opening_context.remote_reserved_ckb_amount,
    ))
}

pub fn settled_balances(records: &[fiber_lsp_sdk::RecoveryRecord]) -> Result<(u128, u128)> {
    let lane = |remote| {
        records
            .iter()
            .filter(|r| r.reference.for_remote == remote)
            .max_by_key(|r| r.reference.version)
            .context("缺少 SDK commitment lane")
    };
    let local = lane(false)?;
    let remote = lane(true)?;
    ensure!(
        local.complete_signature.is_some(),
        "本地 commitment 尚无完整恢复签名"
    );
    ensure!(
        local.settlement.data.tlcs.is_empty() && remote.settlement.data.tlcs.is_empty(),
        "本地仍有未结算 TLC"
    );
    let balance = (
        local.settlement.data.local_amount,
        local.settlement.data.remote_amount,
    );
    ensure!(
        balance
            == (
                remote.settlement.data.local_amount,
                remote.settlement.data.remote_amount
            ),
        "SDK 双 lane 余额尚未同步"
    );
    Ok(balance)
}

#[cfg(test)]
#[path = "../tests/unit/test_channel.rs"]
pub(crate) mod tests;
