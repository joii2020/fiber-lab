//! Independent CKB wallet checks and default-lock signing. No secrets cross the RPC boundary.
use crate::rpc::{Rpc, config_path};
use anyhow::{Context, Result, bail, ensure};
use ckb_sdk::{
    traits::{CellDepResolver, DefaultCellDepResolver, SecpCkbRawKeySigner, Signer},
    types::ScriptGroup,
    unlock::generate_message,
};
use ckb_types::{core::TransactionBuilder, packed, prelude::*};
use fiber_json_types::{OpenChannelWithExternalFundingParams, OpenTenantChannelResult};
use fiber_lsp_sdk::{FundingVerifier, OpeningNetwork, SessionError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::HashSet, path::Path};
use zeroize::Zeroizing;

pub const CKB: u64 = 100_000_000;
pub const FEE_RATE: u64 = 1000;
pub const MAX_FEE: u64 = CKB;
pub const RESERVE: u64 = 99 * CKB;
#[cfg(test)]
pub const CLIENT_FUNDING_ACCOUNT: usize = 6;
const LSP_FUNDING_ACCOUNT: usize = 5;

#[derive(Deserialize)]
struct FiberConfig {
    chain: String,
    scripts: Vec<Contract>,
    open_channel_auto_accept_min_ckb_funding_amount: Option<u64>,
    auto_accept_channel_ckb_funding_amount: Option<u64>,
}

impl FiberConfig {
    fn remote_funding_amount(&self) -> u64 {
        // fnn reserves 99 CKB occupied capacity plus 1 CKB shutdown fee.
        self.auto_accept_channel_ckb_funding_amount
            .unwrap_or(RESERVE + CKB)
    }
}

#[derive(Deserialize)]
struct Contract {
    name: String,
    script: ckb_jsonrpc_types::Script,
    cell_deps: Vec<Dep>,
}

#[derive(Deserialize)]
struct Dep {
    cell_dep: ckb_jsonrpc_types::CellDep,
}

fn legacy_wallet_account() -> usize {
    LSP_FUNDING_ACCOUNT
}

#[derive(Clone, Deserialize, Serialize)]
pub struct Terms {
    pub ckb_rpc_url: String,
    pub genesis_hash: ckb_types::H256,
    pub network: OpeningNetwork,
    pub funding_deps: Vec<ckb_jsonrpc_types::CellDep>,
    pub default_lock_dep: ckb_jsonrpc_types::CellDep,
    pub wallet_lock: ckb_jsonrpc_types::Script,
    #[serde(default = "legacy_wallet_account")]
    pub wallet_account: usize,
    #[serde(default)]
    pub remote_wallet_lock: Option<ckb_jsonrpc_types::Script>,
    pub remote_amount: u64,
    pub auto_accept_min: u64,
    pub max_fee: u64,
    pub funding_fee_rate: u64,
    pub close_fee_rate: u64,
}

impl Terms {
    pub fn remote_wallet_lock(&self) -> &ckb_jsonrpc_types::Script {
        self.remote_wallet_lock
            .as_ref()
            .unwrap_or(&self.wallet_lock)
    }
}

pub struct Wallet {
    pub terms: Terms,
    pub chain: Rpc,
}

impl Wallet {
    pub fn from_terms(terms: Terms) -> Result<Self> {
        Ok(Self {
            chain: Rpc::chain(&terms.ckb_rpc_url)?,
            terms,
        })
    }

    pub async fn load(
        config: Option<&Path>,
        binding: &crate::accounts::FundingBinding,
    ) -> Result<Self> {
        #[derive(Deserialize)]
        struct LocalConfig {
            fiber: FiberConfig,
            ckb: CkbConfig,
        }
        #[derive(Deserialize)]
        struct CkbConfig {
            rpc_url: String,
        }
        let path = config_path(config)?;
        let cfg: LocalConfig = serde_yaml::from_slice(&std::fs::read(&path)?)
            .map_err(|_| anyhow::anyhow!("Node3 配置缺少 ckb.rpc_url、fiber.chain 或合约部署"))?;
        let spec: toml::Value = toml::from_str(&std::fs::read_to_string(
            path.parent().unwrap().join(&cfg.fiber.chain),
        )?)?;
        ensure!(
            spec.get("name").and_then(toml::Value::as_str) == Some("ckb_dev"),
            "首版仅支持 devnet CKB"
        );
        let epoch_seconds = spec
            .get("params")
            .and_then(|v| v.get("epoch_duration_target"))
            .and_then(toml::Value::as_integer)
            .context("链配置缺少 params.epoch_duration_target")?;
        let funding = cfg
            .fiber
            .scripts
            .iter()
            .find(|s| s.name == "FundingLock")
            .context("缺少 FundingLock 部署")?;
        let commitment = cfg
            .fiber
            .scripts
            .iter()
            .find(|s| s.name == "CommitmentLock")
            .context("缺少 CommitmentLock 部署")?;
        let funding_lock: packed::Script = funding.script.clone().into();
        let commitment_lock: packed::Script = commitment.script.clone().into();
        ensure!(
            funding_lock.args().is_empty() && commitment_lock.args().is_empty(),
            "合约部署 args 必须为空"
        );
        ensure!(!funding.cell_deps.is_empty(), "FundingLock cell deps 为空");
        let public = public_binding(binding.offckb_index)?;
        ensure!(&public == binding, "用户钱包公开绑定与运行时 offckb 不一致");
        let wallet_lock = public.lock_script;
        let remote_wallet_lock = funding_account(LSP_FUNDING_ACCOUNT, false)?.lock_script;
        validate_default_lock(&wallet_lock)?;
        validate_default_lock(&remote_wallet_lock)?;
        ensure!(
            wallet_lock != remote_wallet_lock,
            "客户端和 LSP 出资钱包必须不同"
        );
        let chain = Rpc::chain(&cfg.ckb.rpc_url)?;
        let genesis: ckb_jsonrpc_types::BlockView = chain
            .call("get_block_by_number", json!(["0x0"]), "")
            .await?;
        let genesis_hash = genesis.header.hash.clone();
        let resolver = DefaultCellDepResolver::from_genesis_async(&genesis.into()).await?;
        let dep = resolver
            .resolve(&wallet_lock.clone().into())
            .context("genesis 中无法解析默认 lock dep")?;
        let remote_amount = cfg.fiber.remote_funding_amount();
        let terms = Terms {
            ckb_rpc_url: cfg.ckb.rpc_url,
            genesis_hash,
            network: OpeningNetwork {
                funding_lock,
                commitment_lock,
                commitment_cell_deps: funding.cell_deps.len(),
                epoch_duration_ms: u64::try_from(epoch_seconds)?
                    .checked_mul(1000)
                    .context("epoch duration overflow")?,
                minimum_shutdown_fee: CKB,
            },
            funding_deps: funding
                .cell_deps
                .iter()
                .map(|d| d.cell_dep.clone())
                .collect(),
            default_lock_dep: dep.into(),
            wallet_lock,
            wallet_account: binding.offckb_index,
            remote_wallet_lock: Some(remote_wallet_lock),
            remote_amount,
            auto_accept_min: cfg
                .fiber
                .open_channel_auto_accept_min_ckb_funding_amount
                .unwrap_or(100 * CKB),
            max_fee: MAX_FEE,
            funding_fee_rate: FEE_RATE,
            close_fee_rate: FEE_RATE,
        };
        ensure!(terms.remote_amount >= RESERVE, "对端注入额不满足保留容量");
        Ok(Self { terms, chain })
    }

    pub async fn check_capacity(&self, amount: u64) -> Result<()> {
        let cells: Value = self.chain.call("get_cells", json!([{
            "script": self.terms.wallet_lock, "script_type": "lock", "script_search_mode": "exact",
            "filter": { "script_len_range": ["0x0", "0x1"], "output_data_len_range": ["0x0", "0x1"] }
        }, "asc", "0x64"]), "").await?;
        let objects = cells["objects"]
            .as_array()
            .context("CKB cells 响应缺少 objects")?;
        let mut total = 0u128;
        for object in objects {
            let output: ckb_jsonrpc_types::CellOutput =
                serde_json::from_value(object["output"].clone())
                    .context("CKB cells 输出格式错误")?;
            ensure!(
                output.lock == self.terms.wallet_lock && output.type_.is_none(),
                "CKB 返回了不匹配的钱包资产"
            );
            total = total
                .checked_add(u128::from(output.capacity.value()))
                .context("钱包容量溢出")?;
        }
        ensure!(
            total >= u128::from(amount) + u128::from(self.terms.max_fee),
            "#{} 余额不足以覆盖本方注入额和交易费上限",
            self.terms.wallet_account
        );
        Ok(())
    }

    pub async fn check_network(&self) -> Result<()> {
        let hash: ckb_types::H256 = self
            .chain
            .call("get_block_hash", json!(["0x0"]), "")
            .await?;
        ensure!(
            hash == self.terms.genesis_hash,
            "CKB genesis 与保存的意图不一致"
        );
        Ok(())
    }

    pub async fn transaction(
        &self,
        hash: &ckb_types::H256,
    ) -> Result<Option<ckb_jsonrpc_types::TransactionWithStatusResponse>> {
        self.chain.call("get_transaction", json!([hash]), "").await
    }

    pub async fn funding_known(&self, hash: &ckb_types::H256) -> Result<bool> {
        let Some(response) = self.transaction(hash).await? else {
            return Ok(false);
        };
        ensure!(
            response.tx_status.status != ckb_jsonrpc_types::Status::Rejected,
            "原出资交易被 CKB 拒绝；未重建意图"
        );
        Ok(response.tx_status.status != ckb_jsonrpc_types::Status::Unknown)
    }

    pub async fn committed_transaction(
        &self,
        hash: &ckb_types::H256,
    ) -> Result<Option<packed::Transaction>> {
        let Some(response) = self.transaction(hash).await? else {
            return Ok(None);
        };
        if response.tx_status.status != ckb_jsonrpc_types::Status::Committed {
            return Ok(None);
        }
        let tx = response.transaction.context("已确认交易缺少交易体")?;
        match tx.inner {
            ckb_jsonrpc_types::Either::Left(view) => {
                let tx: packed::Transaction = view.inner.into();
                ensure!(
                    ckb_types::H256::from(tx.calc_tx_hash()) == *hash,
                    "CKB 返回交易 hash 与查询不一致"
                );
                Ok(Some(tx))
            }
            ckb_jsonrpc_types::Either::Right(_) => bail!("CKB 返回了不支持的交易格式"),
        }
    }

    pub async fn input_cell(
        &self,
        outpoint: &packed::OutPoint,
        live: bool,
    ) -> Result<(packed::CellOutput, Vec<u8>)> {
        if live {
            let point: ckb_jsonrpc_types::OutPoint = outpoint.clone().into();
            let cell: ckb_jsonrpc_types::CellWithStatus = self
                .chain
                .call("get_live_cell", json!([point, true]), "")
                .await?;
            ensure!(cell.status == "live", "出资 input 不是 live cell；未签名");
            let cell = cell.cell.context("live cell 缺少数据")?;
            Ok((
                cell.output.into(),
                cell.data
                    .context("live cell 缺少 output data")?
                    .content
                    .into_bytes()
                    .to_vec(),
            ))
        } else {
            let hash: ckb_types::H256 = outpoint.tx_hash().unpack();
            let tx = self
                .committed_transaction(&hash)
                .await?
                .context("input 的原交易未确认")?;
            let index: u32 = outpoint.index().unpack();
            Ok((
                tx.raw()
                    .outputs()
                    .get(index as usize)
                    .context("input index 不存在")?,
                tx.raw()
                    .outputs_data()
                    .get(index as usize)
                    .context("input data 不存在")?
                    .raw_data()
                    .to_vec(),
            ))
        }
    }

    pub async fn verify(
        &self,
        request: &OpenChannelWithExternalFundingParams,
        result: &OpenTenantChannelResult,
        live: bool,
    ) -> Result<Vec<packed::OutPoint>> {
        ensure!(
            request.funding_udt_type_script.is_none() && request.public == Some(false),
            "仅支持 CKB 私有通道"
        );
        ensure!(
            request.funding_lock_script == self.terms.wallet_lock
                && request.shutdown_script == self.terms.wallet_lock,
            "钱包脚本与原意图不一致"
        );
        ensure!(
            result.opening_context.remote_amount == u128::from(self.terms.remote_amount),
            "对方注入额与本地条款不一致：服务端 {} shannons，本地 {} shannons；原开通意图已保留",
            result.opening_context.remote_amount,
            self.terms.remote_amount
        );
        ensure!(
            &result.opening_context.remote_shutdown_script == self.terms.remote_wallet_lock(),
            "LSP 退款脚本与保存的条款不一致"
        );
        let tx: packed::Transaction = result.unsigned_funding_tx.clone().into();
        let mut inputs = Vec::new();
        let mut cells = Vec::new();
        for input in tx.raw().inputs() {
            ensure!(input.since() == 0u64.pack(), "出资 input since 非零");
            let point = input.previous_output();
            cells.push(self.input_cell(&point, live).await?);
            inputs.push(point);
        }
        validate_funding(&tx, &cells, request.funding_amount, &self.terms)?;
        Ok(inputs)
    }

    pub fn sign(
        &self,
        tx: packed::Transaction,
        cells: &[(packed::CellOutput, Vec<u8>)],
    ) -> Result<packed::Transaction> {
        let index = self.terms.wallet_account;
        let account = funding_account(index, true)?;
        ensure!(
            account.lock_script == self.terms.wallet_lock,
            "offckb #{index} 脚本与保存的条款不一致"
        );
        let secret = account
            .privkey
            .with_context(|| format!("offckb #{index} 缺少私钥"))?;
        let bytes = Zeroizing::new(
            hex::decode(secret.strip_prefix("0x").unwrap_or(&secret))
                .map_err(|_| anyhow::anyhow!("offckb #{index} 私钥格式错误"))?,
        );
        let key = secp256k1::SecretKey::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("offckb #{index} 私钥无效"))?;
        let public = key.public_key(secp256k1::SECP256K1).serialize();
        let hash = ckb_hash::blake2b_256(public);
        let lock: packed::Script = self.terms.wallet_lock.clone().into();
        ensure!(
            lock.args().raw_data().as_ref() == &hash[..20],
            "offckb #{index} 私钥与 lock args 不匹配"
        );
        let signer = SecpCkbRawKeySigner::new_with_secret_keys(vec![key]);
        let group = wallet_signing_group(&tx, cells, &lock)?;
        sign_group(tx, &group, &signer, &hash[..20])
    }

    pub fn close_fee(&self, remote_script: packed::Script) -> Result<u64> {
        let local: packed::Script = self.terms.wallet_lock.clone().into();
        let tx = TransactionBuilder::default()
            .cell_deps(
                self.terms
                    .funding_deps
                    .iter()
                    .cloned()
                    .map(Into::into)
                    .collect::<Vec<packed::CellDep>>(),
            )
            .input(packed::CellInput::default())
            .output(packed::CellOutput::new_builder().lock(local).build())
            .output(
                packed::CellOutput::new_builder()
                    .lock(remote_script)
                    .build(),
            )
            .outputs_data(vec![packed::Bytes::default(); 2])
            .witness([0u8; 112].pack())
            .build();
        let fee = u64::try_from(
            u128::from(self.terms.close_fee_rate) * tx.data().serialized_size_in_block() as u128
                / 1000,
        )?;
        ensure!(fee <= self.terms.max_fee, "关闭费用超出钱包上限");
        Ok(fee)
    }
}

pub(crate) fn validate_default_lock(script: &ckb_jsonrpc_types::Script) -> Result<()> {
    ensure!(
        script.code_hash == ckb_sdk::constants::SIGHASH_TYPE_HASH
            && script.hash_type == ckb_jsonrpc_types::ScriptHashType::Type
            && script.args.len() == 20,
        "出资钱包必须使用默认 secp256k1_blake160_sighash_all lock"
    );
    Ok(())
}

fn validate_funding(
    tx: &packed::Transaction,
    cells: &[(packed::CellOutput, Vec<u8>)],
    amount: u128,
    terms: &Terms,
) -> Result<u64> {
    let raw = tx.raw();
    ensure!(
        raw.version() == 0u32.pack() && raw.header_deps().is_empty(),
        "不支持的出资交易结构"
    );
    ensure!(
        !cells.is_empty() && cells.len() == raw.inputs().len(),
        "出资 inputs 缺失"
    );
    let mut seen = HashSet::new();
    for input in raw.inputs() {
        ensure!(
            seen.insert(input.previous_output().as_slice().to_vec()),
            "重复出资 input"
        );
    }
    let wallet: packed::Script = terms.wallet_lock.clone().into();
    let remote_wallet: packed::Script = terms.remote_wallet_lock().clone().into();
    let mut total = 0u128;
    let mut local_inputs = 0u128;
    let mut remote_inputs = 0u128;
    for (cell, data) in cells {
        ensure!(
            (cell.lock() == wallet || cell.lock() == remote_wallet)
                && cell.type_().to_opt().is_none()
                && data.is_empty(),
            "出资 input 归属或资产错误"
        );
        let capacity = u128::from(u64::from(cell.capacity()));
        if cell.lock() == wallet {
            local_inputs += capacity;
        } else {
            remote_inputs += capacity;
        }
        total = total.checked_add(capacity).context("input 容量溢出")?;
    }
    ensure!(
        !raw.outputs().is_empty() && raw.outputs().len() == raw.outputs_data().len(),
        "出资输出缺失"
    );
    let funding = raw.outputs().get(0).unwrap();
    ensure!(
        u128::from(u64::from(funding.capacity()))
            == amount
                .checked_add(u128::from(terms.remote_amount))
                .context("出资总额溢出")?,
        "通道容量不等于双方授权的总注入额"
    );
    ensure!(
        funding.lock().code_hash() == terms.network.funding_lock.code_hash()
            && funding.lock().hash_type() == terms.network.funding_lock.hash_type(),
        "不可信通道合约"
    );
    let mut output_total = 0u128;
    let mut local_change = 0u128;
    let mut remote_change = 0u128;
    for (index, output) in raw.outputs().into_iter().enumerate() {
        ensure!(
            output.type_().to_opt().is_none() && raw.outputs_data().get(index).unwrap().is_empty(),
            "出资 output 资产或数据错误"
        );
        if index != 0 {
            let capacity = u128::from(u64::from(output.capacity()));
            if output.lock() == wallet {
                local_change += capacity;
            } else if output.lock() == remote_wallet {
                remote_change += capacity;
            } else {
                bail!("找零未退回原出资钱包");
            }
        }
        ensure!(
            !output.is_lack_of_capacity(ckb_types::core::Capacity::zero())?,
            "输出占用容量不足"
        );
        output_total = output_total
            .checked_add(u128::from(u64::from(output.capacity())))
            .context("output 容量溢出")?;
    }
    let fee = total
        .checked_sub(output_total)
        .context("出资 outputs 超过 inputs")?;
    ensure!(fee <= u128::from(terms.max_fee), "出资费用超出钱包上限");
    if wallet != remote_wallet {
        let local_debit = local_inputs
            .checked_sub(local_change)
            .context("本方找零超过 inputs")?;
        let remote_debit = remote_inputs
            .checked_sub(remote_change)
            .context("对方找零超过 inputs")?;
        ensure!(
            local_debit >= amount && local_debit <= amount + u128::from(terms.max_fee),
            "本方钱包实际扣款与授权注入额或费用上限不符"
        );
        ensure!(
            remote_debit >= u128::from(terms.remote_amount),
            "LSP 未足额承担对方出资"
        );
    }
    let allowed: Vec<packed::CellDep> = vec![terms.default_lock_dep.clone().into()];
    let actual: Vec<_> = raw.cell_deps().into_iter().collect();
    ensure!(
        allowed.iter().all(|dep| actual.contains(dep))
            && actual.iter().all(|dep| allowed.contains(dep))
            && actual.len() == allowed.len(),
        "出资 cell deps 与可信部署不一致"
    );
    Ok(u64::try_from(fee)?)
}

fn wallet_signing_group(
    tx: &packed::Transaction,
    cells: &[(packed::CellOutput, Vec<u8>)],
    lock: &packed::Script,
) -> Result<ScriptGroup> {
    ensure!(
        cells.len() == tx.raw().inputs().len(),
        "签名 inputs 缺少链上核验"
    );
    let mut group = ScriptGroup::from_lock_script(lock);
    group.input_indices = cells
        .iter()
        .enumerate()
        .filter_map(|(index, (cell, _))| (cell.lock() == *lock).then_some(index))
        .collect();
    ensure!(!group.input_indices.is_empty(), "没有本方钱包可签名 inputs");
    Ok(group)
}

fn sign_group(
    tx: packed::Transaction,
    group: &ScriptGroup,
    signer: &SecpCkbRawKeySigner,
    id: &[u8],
) -> Result<packed::Transaction> {
    ensure!(!group.input_indices.is_empty(), "没有可签名 inputs");
    let view = tx.clone().into_view();
    let message = generate_message(&view, group, vec![0u8; 65].into())?;
    let signature = signer.sign(id, &message, true, &view)?;
    let mut witnesses: Vec<_> = view.witnesses().into_iter().collect();
    let index = group.input_indices[0];
    witnesses.resize(
        witnesses.len().max(view.inputs().len()),
        packed::Bytes::default(),
    );
    let data = witnesses[index].raw_data();
    let witness = if data.is_empty() {
        packed::WitnessArgs::default()
    } else {
        packed::WitnessArgs::from_slice(&data).context("钱包 witness 格式错误")?
    };
    witnesses[index] = witness
        .as_builder()
        .lock(Some(signature).pack())
        .build()
        .as_bytes()
        .pack();
    let signed = view
        .as_advanced_builder()
        .set_witnesses(witnesses)
        .build()
        .data();
    ensure!(
        signed.raw().as_slice() == tx.raw().as_slice()
            && signed.calc_tx_hash() == tx.calc_tx_hash(),
        "签名改变了 raw transaction"
    );
    Ok(signed)
}

#[derive(Deserialize)]
struct FundingAccount {
    index: usize,
    #[serde(rename = "lockScript")]
    #[serde(deserialize_with = "offckb_script")]
    lock_script: ckb_jsonrpc_types::Script,
    #[serde(default, deserialize_with = "private_key")]
    privkey: Option<Zeroizing<String>>,
    #[serde(default)]
    address: String,
}

fn private_key<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<Zeroizing<String>>, D::Error> {
    Ok(Option::<String>::deserialize(deserializer)?.map(Zeroizing::new))
}

fn offckb_script<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<ckb_jsonrpc_types::Script, D::Error> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Script {
        code_hash: ckb_types::H256,
        hash_type: ckb_jsonrpc_types::ScriptHashType,
        args: ckb_jsonrpc_types::JsonBytes,
    }
    let script = Script::deserialize(deserializer)?;
    Ok(ckb_jsonrpc_types::Script {
        code_hash: script.code_hash,
        hash_type: script.hash_type,
        args: script.args,
    })
}

fn funding_account(account_index: usize, private: bool) -> Result<FundingAccount> {
    let mut command = std::process::Command::new("offckb");
    command.args(["--json", "accounts"]);
    if private {
        command.arg("--show-private-keys");
    }
    let mut output = command.output().context("无法执行 offckb accounts")?;
    let stdout = Zeroizing::new(std::mem::take(&mut output.stdout));
    let _stderr = Zeroizing::new(std::mem::take(&mut output.stderr));
    ensure!(
        output.status.success(),
        "offckb accounts 执行失败（输出已隐藏）"
    );
    #[derive(Deserialize)]
    struct Accounts<'a> {
        #[serde(borrow)]
        accounts: Vec<&'a serde_json::value::RawValue>,
    }
    #[derive(Deserialize)]
    struct Index {
        index: usize,
    }
    let data: Accounts<'_> = serde_json::from_slice(&stdout)
        .map_err(|_| anyhow::anyhow!("offckb accounts 响应格式错误（输出已隐藏）"))?;
    let mut selected = None;
    for raw in data.accounts {
        let index: Index = serde_json::from_str(raw.get())
            .map_err(|_| anyhow::anyhow!("offckb accounts 索引无效"))?;
        if index.index == account_index {
            ensure!(selected.is_none(), "offckb 返回重复的 #{account_index}");
            selected = Some(raw);
        }
    }
    let raw = selected.with_context(|| format!("缺少 offckb #{account_index}"))?;
    let account: FundingAccount = serde_json::from_str(raw.get())
        .map_err(|_| anyhow::anyhow!("offckb #{account_index} 格式错误（输出已隐藏）"))?;
    ensure!(account.index == account_index, "错误出资账户");
    Ok(account)
}

#[async_trait::async_trait]
impl FundingVerifier for Wallet {
    async fn verify_funding(
        &self,
        request: &OpenChannelWithExternalFundingParams,
        result: &OpenTenantChannelResult,
    ) -> std::result::Result<Vec<packed::OutPoint>, SessionError> {
        self.verify(request, result, true)
            .await
            .map_err(|e| SessionError::Invalid(format!("钱包拒绝出资：{e:#}")))
    }
}

// Only live funding checks are needed for off-chain preimage release. On-chain
// spending remains outside the CLI's authorized capabilities.
#[async_trait::async_trait]
impl fiber_lsp_sdk::ChainVerifier for Wallet {
    async fn live_cell(
        &self,
        point: &packed::OutPoint,
    ) -> std::result::Result<fiber_lsp_sdk::VerifiedCell, fiber_lsp_sdk::SignerError> {
        let _timing = crate::timings::Span::new("payment.chain_check");
        let result = async {
            self.check_network().await?;
            let (output, data) = self.input_cell(point, true).await?;
            Ok::<_, anyhow::Error>(fiber_lsp_sdk::VerifiedCell { output, data })
        }
        .await;
        result.map_err(|e| {
            fiber_lsp_sdk::SignerError::InvalidContent(format!("独立链上核验失败：{e:#}"))
        })
    }
    async fn verify_commitment_lineage(
        &self,
        _: fiber_types::Hash256,
        _: &packed::OutPoint,
    ) -> std::result::Result<(), fiber_lsp_sdk::SignerError> {
        Err(fiber_lsp_sdk::SignerError::InvalidContent(
            "不支持链上追偿".into(),
        ))
    }
    async fn verify_maturity(
        &self,
        _: &[packed::CellInput],
    ) -> std::result::Result<(), fiber_lsp_sdk::SignerError> {
        Err(fiber_lsp_sdk::SignerError::InvalidContent(
            "不支持链上追偿".into(),
        ))
    }
    async fn median_time_ms(&self) -> std::result::Result<u64, fiber_lsp_sdk::SignerError> {
        Err(fiber_lsp_sdk::SignerError::InvalidContent(
            "不支持链上追偿".into(),
        ))
    }
}

pub fn public_binding(index: usize) -> Result<crate::accounts::FundingBinding> {
    let account = funding_account(index, false)?;
    validate_default_lock(&account.lock_script)?;
    ensure!(!account.address.is_empty(), "offckb #{index} address 为空");
    Ok(crate::accounts::FundingBinding {
        offckb_index: index,
        lock_script: account.lock_script,
        address: account.address,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    #[test]
    fn omitted_funding_uses_fnn_default_and_explicit_values_are_preserved() {
        let config: FiberConfig =
            serde_yaml::from_str("chain: devnet.toml\nscripts: []\n").unwrap();
        assert_eq!(config.remote_funding_amount(), 100 * CKB);
        let config: FiberConfig = serde_yaml::from_str(
            "chain: devnet.toml\nscripts: []\nauto_accept_channel_ckb_funding_amount: null\n",
        )
        .unwrap();
        assert_eq!(config.remote_funding_amount(), 100 * CKB);
        for amount in [0, 99 * CKB, 200 * CKB] {
            let config: FiberConfig = serde_yaml::from_str(&format!(
                "chain: devnet.toml\nscripts: []\nauto_accept_channel_ckb_funding_amount: {amount}\n"
            )).unwrap();
            assert_eq!(config.remote_funding_amount(), amount);
        }
    }
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
            packed::WitnessArgs::from_slice(&signed.witnesses().get(0).unwrap().raw_data())
                .unwrap();
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
            packed::WitnessArgs::from_slice(&signed.witnesses().get(1).unwrap().raw_data())
                .unwrap();
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
}
