# fiber-lsp-sdk 集成指南

本文面向接入 hosted Fiber LSP 的 Rust 钱包或应用开发者，以同级仓库 `../fiber/crates/fiber-lsp-sdk` 和本项目当前 `fiber-lsp-client` 实现为准。SDK 管理客户端身份、通道密钥和签名验证；应用通过自己的 HTTP / WebSocket、钱包和存储适配层完成业务流程。

先看 [SDK README](../../fiber/crates/fiber-lsp-sdk/README.md) 和[公开导出](../../fiber/crates/fiber-lsp-sdk/src/lib.rs)，再按下面的生命周期实现。文中的 RPC 名称是服务端方法，不是 SDK 函数；`lsp-cli`、offckb 和节点管理 Shell 都不属于 SDK API。

## 1. 先划清职责

| 组件 | 负责什么 |
| --- | --- |
| `RootSigner` / `ChannelSigner` | 根身份、通道密钥派生、公有材料、按意图核验签名内容、保存签名历史与恢复证据 |
| `HostedSession` | 注册证明、tenant token、`channel_id → ChannelKeyId` 映射、开通编排、将签名状态转成 `ProcessOutcome` |
| 应用 | 安全备份根密钥、持久化 SDK 和业务状态、用户授权、可信时间、HTTP / WS、出资钱包签名、独立链上核验、重试和最终完成判定 |
| LSP | 托管租户运行时、构造通道交易和签名请求、支付路由、广播及状态查询 |

SDK 不依赖 Fiber 节点运行时、actor 传输、HTTP 客户端或具体数据库；支持 native 和 `wasm32-unknown-unknown`。浏览器仍需自行实现 IndexedDB、网络和密钥保管。不要把 LSP 的余额、交易输入或签名请求直接当作本地授权。

集成主线如下；“签名驱动”贯穿开通、收付款和关闭：

```text
恢复或创建身份 → 注册 / 刷新 token → 保存开通意图 → 核验冻结出资交易
                                                          ↓
                                  SDK 保存 commitment 基线与 channel binding
                                                          ↓
                                 钱包签出资 → 提交 → 链上确认 / ChannelReady
                                                          ↓
                    保存付款 / 发票授权 → 持续处理签名请求 → 核验最终结算
                                                          ↓
                            保存关闭授权 → 协作关闭 → 链上核验双方退款
```

## 2. 依赖与类型

以下路径对应本项目 `fiber-lsp-client/Cargo.toml` 所在位置；新项目按自身目录调整。SDK 和配套类型应来自同一份 Fiber 源码：

```toml
[dependencies]
fiber-lsp-sdk = { path = "../../fiber/crates/fiber-lsp-sdk" }
fiber-types = { path = "../../fiber/crates/fiber-types" }
fiber-json-types = { path = "../../fiber/crates/fiber-json-types" }
anyhow = "1"
serde_json = "1"
zeroize = "1"
```

SDK 默认启用 `json`，提供 `HostedSession`、开通适配接口及 `fiber_lsp_sdk::json` 转换。关闭默认 feature 后不能直接使用本文的 hosted 流程。`test-apis` 会暴露测试用 `SigningPolicy::Always`，应用不应启用它。

实现适配器还会用到 `async-trait`、`ckb-types` 和 `ckb-jsonrpc-types`；网络库由应用选择。参考客户端使用 `reqwest`、`jsonrpsee` 和 `tokio`，见[依赖清单](../fiber-lsp-client/Cargo.toml)。下文 Rust 函数可放进同一模块；标注“流程片段”的代码依赖应用自身接口，不能直接作为完整程序运行。

## 3. 持久化与根密钥恢复

至少保留三类数据，不能只备份根密钥：

| 数据 | 保存要求 |
| --- | --- |
| `RootKeyBackup` 的 32 字节根秘密 | 用独立、安全的备份机制保管；不要写日志、上传给 LSP，或混同普通业务快照 |
| `SignerStore` 全部记录 | 包含通道分配随机材料、nonce / 签名历史、已批准基线、付款授权和恢复证据；写成功必须表示已可靠持久化 |
| `HostedSessionState` 与应用业务记录 | 保存 token、binding、pending、原始开通请求；另存可信 LSP / 网络身份、钱包条款、冻结交易、待提交签名、付款意图和原像等 |

`SignerStore` 是不透明字节存储接口，提供 `get`、`insert_if_absent`、`compare_and_swap`、`put`、`delete`。同一 key 的条件写必须原子化；`expected == None` 表示 key 不存在。不要用普通的“先读后写”模拟 CAS。SDK 在返回签名前将已验证状态、签名历史和恢复证据写入存储；后端提前报告成功会破坏这一保证。

`RootSigner::create_random(store)` 仅用于新身份，返回 `root_signer` 和 `root_key_backup`；外部已有根秘密时可用 `RootSigner::create(root_key, store)` 初始化空存储。已有身份使用 `RootSigner::open`，它会核对存储版本及根公钥。根密钥本身不直接作为 Fiber 通道密钥；通道派生还依赖存储中的分配随机材料。

下面是可编译的内存往返示例，演示需要一起恢复哪些对象。`MemoryStore` 仅用于演示和测试；它不会自动落盘，`snapshot()` 也不是生产存储事务。

```rust
use fiber_lsp_sdk::{
    HostedSession, HostedSessionState, MemoryStore, RootKey, RootSigner, SigningPolicy,
};
use zeroize::Zeroizing;

async fn memory_round_trip() -> anyhow::Result<HostedSession<MemoryStore>> {
    let store = MemoryStore::default();
    let created = RootSigner::create_random(store.clone()).await?;
    let secret = Zeroizing::new(created.root_key_backup.expose_secret());
    // 生产应用在任何远端操作前完成根密钥安全备份。
    let mut session = HostedSession::new(created.root_signer)
        .with_policy(SigningPolicy::Manual);
    let _public_material = session.allocate_pending_channel().await?;
    let signer_snapshot = store.snapshot()?;
    let session_snapshot = serde_json::to_vec(session.state())?;

    // 模拟重启：生产应用从持久化后端和独立秘密存储读取。
    let restored_store = MemoryStore::from_snapshot(&signer_snapshot)?;
    let root = RootSigner::open(RootKey::import(*secret)?, restored_store).await?;
    let state: HostedSessionState = serde_json::from_slice(&session_snapshot)?;
    Ok(HostedSession::new(root)
        .with_state(state)
        .with_policy(SigningPolicy::Manual))
}
```

`HostedSessionState` 的序列化和落盘由调用方负责；策略也应在恢复时显式设置。本项目将根秘密保存在 `root-key`，SDK 快照保存在 `signer.bin`，session 保存在 `account.json`，开通 / 关闭意图保存在 `channel.json`。参考 [FileSignerStore](../fiber-lsp-client/src/store.rs) 的写入顺序：先构造候选快照，可靠写入后再发布内存状态；写入失败后停止使用该实例。账户锁覆盖完整生命周期操作，不能让多个进程各持一份旧快照覆盖彼此。

同时校验本地账户、tenant、LSP 公钥、网络和钱包绑定；本项目当前还校验账户永久用户归属，见 [account.rs](../fiber-lsp-client/src/account.rs)、[accounts.rs](../fiber-lsp-client/src/accounts.rs)。目录缺失、快照损坏或密钥不匹配时保留原数据并停止推进，不要重新创建身份来“恢复”旧通道。

## 4. 注册租户与使用 token

注册和凭证刷新走相同证明流程：

1. 从可信配置确定目标 LSP 公钥；用根身份公钥调用 `lsp_get_tenant_registry_nonce`。
2. 核对响应中的 `lsp_node_id`，交给 `session.begin_registration(nonce_result)`。SDK 会核对根公钥，并签署绑定 LSP、公钥和一次性 nonce 的注册证明。
3. 把返回的 `RegisterLspTenantParams` 通过 `lsp_register_tenant` 提交；网络发送前持久化本地注册进度。
4. 核对响应的 tenant、根公钥、非空 token；凭证刷新时还应确认原有租户身份和通道关联未变。调用 `session.finish_registration(result)`，随后可靠保存 `session.state()` 和租户信息。

`finish_registration` 检查 tenant ID 并把 token 写入 session 内存，它不替应用完成全部响应核验或落盘。下面的函数补充了目标 LSP 身份检查：

```rust
fn registration_request<S: fiber_lsp_sdk::SignerStore>(
    session: &fiber_lsp_sdk::HostedSession<S>,
    trusted_lsp: fiber_json_types::Pubkey,
    nonce: fiber_json_types::GetLspTenantRegistryNonceResult,
) -> anyhow::Result<fiber_json_types::RegisterLspTenantParams> {
    anyhow::ensure!(nonce.lsp_node_id == trusted_lsp, "注册 nonce 来自非预期 LSP");
    Ok(session.begin_registration(nonce)?)
}
```

注册入口需要有相应权限的引导凭证。本项目由操作方凭证调用这两个 RPC；生产应用应设计自己的受控注册入口，不应把 LSP 的 Biscuit 签发私钥交给钱包。注册返回的 **tenant Biscuit** 才用于该租户的数据面 RPC 和 WS 握手，HTTP / WS 均通过 `Authorization: Bearer <token>` 发送。订阅还需要 token 的订阅和通道权限；HTTP URL 与 WS URL 分别配置。

当前服务端在租户已存在时，会验证新 nonce / 同一根身份的证明并刷新 token，保留原租户和通道，见 [RegisterAuthenticatedTenant](../../fiber/crates/fiber-lib/src/lsp/service.rs)。因此响应丢失时可设计显式重新认证流程，不能重复使用已消费的 nonce。本项目 CLI 的[注册恢复逻辑](../fiber-lsp-client/src/account.rs)目前对服务端已有租户但本地 token 丢失的情况直接停止，尚未接入该刷新流程。SDK / 服务端能力与 CLI 的当前行为应分别处理。

## 5. 开通通道：先验证，再签出资

使用 `HostedSession::open_tenant_channel`，注入以下接口：

| 参数 | 应用必须提供的保证 |
| --- | --- |
| `OpeningNetwork` | 来自可信网络配置的 FundingLock / CommitmentLock 部署、空 args、commitment deps 数量、epoch 时长、最低关闭费；不能照抄 LSP 响应 |
| `TenantOpeningRpc` | 用 tenant token 调用 `open_tenant_channel`；保存冻结响应，使中断后能复用同一提案 |
| `FundingVerifier` | 独立解析完整交易和真实 inputs，验证资产、双方出资、钱包净扣款、退款 / 找零目的地、费用和 deps；按交易顺序返回**全部已批准 input outpoint** |
| `OpeningPersistence` | 原子持久化 `HostedSessionState`，失败即停止；SDK 会在 RPC 前和成功返回前调用 |

首次开通先 `allocate_pending_channel().await?`，保存 session，再用 `json::open_material_to_rpc(&material)` 填入 `OpenChannelWithExternalFundingParams.external_channel_signer`。请求必须使用 `public: Some(false)`，显式选择 `commitment_delay_epoch` 和 `commitment_fee_rate`；`shutdown_script`、出资钱包脚本、金额、资产和对方公钥均来自本地批准条款。

下面的函数接收**已组装并保存的完整原始请求**，展示实际 SDK 调用。具体请求组装参考 [channel.rs](../fiber-lsp-client/src/channel.rs)，接口定义见 [opening.rs](../../fiber/crates/fiber-lsp-sdk/src/opening.rs)。

```rust
async fn open_saved_request<S, R, V, P>(
    session: &mut fiber_lsp_sdk::HostedSession<S>,
    request: fiber_json_types::OpenChannelWithExternalFundingParams,
    network: &fiber_lsp_sdk::OpeningNetwork,
    rpc: &R,
    wallet: &V,
    persistence: &P,
) -> Result<fiber_json_types::OpenTenantChannelResult, fiber_lsp_sdk::SessionError>
where
    S: fiber_lsp_sdk::SignerStore,
    R: fiber_lsp_sdk::TenantOpeningRpc,
    V: fiber_lsp_sdk::FundingVerifier,
    P: fiber_lsp_sdk::OpeningPersistence,
{
    session.open_tenant_channel(request, network, rpc, wallet, persistence).await
}
```

SDK 的执行顺序是：保存原始 intent 和 pending signer → 调用 RPC → `FundingVerifier` 独立批准交易 → 验证 opening context → 保存 commitment 基线和 funding binding → 保存 session → 返回冻结结果。SDK 会核对通道 ID 与 settlement keys、聚合 funding key、可信合约、资产、余额、保留金额及 commitment 费用等；**它不能代替钱包核验自己的 inputs、实际净扣款和费用上限**。不要简单把响应里的 inputs 原样返回，当作 `FundingVerifier` 的实现。

返回后仍需应用完成：

1. 钱包在签名前再次核验冻结交易和 input 存活状态，只签自己控制的出资锁组；根身份 / 通道 signer 不替出资钱包签名。
2. 可靠保存签好的交易和 hash，再用 tenant token 调用 `submit_signed_funding_tx`，核对响应中的 channel ID 和 funding hash。
3. 驱动下一节的通道签名请求，直到 `ChannelReady`；独立查链确认出资交易已 committed，且 raw transaction 与原冻结交易一致，再宣布开通完成。

中断后恢复原根密钥、store、session、网络条款和冻结结果，重试**完全相同的**开通请求；SDK 保留 `opening_request` 以核验重试，不能替换金额、密钥或条款。已绑定后重试也不要再次分配 pending signer。`open_tenant_channel` 不会自动广播出资，也没有额外的 approval / bind RPC。低层 `approve_opening` 是本地 signer API，JSON 集成应优先使用上述已封装验证的路径。

## 6. 驱动 WS 签名请求与提交结果

应用保持带 tenant token 的 WS 连接，调用：

```json
{"jsonrpc":"2.0","id":1,"method":"subscribe","params":["channel_signing",{"channel_ids":["<channel_id>"]}]}
```

这是一份参数结构示意，`<channel_id>` 要替换成真实 hex ID。使用 `unsubscribe` 取消订阅。事件解码为 `fiber_json_types::SubscriptionEvent`；`ChannelSigning` 内的数据与 `get_channel_signing_status` 的 `GetChannelSigningStatusResult` 相同。

订阅建立 / 重连后，服务端先重发当前待签名请求，再推送状态变化；没有历史 cursor、session header 或“快照完成”事件。应用应按通道串行处理、控制队列、重连重订阅，并处理 `Removed`。轮询 `get_channel_signing_status` 也可使用相同处理器。参考 [events.rs](../fiber-lsp-client/src/events.rs)。

先校验最外层 `result.channel_id`，再把状态交给 SDK。`now_ms` 必须是钱包自己的 Unix 毫秒时钟；不能使用 LSP 声称的时间。

```rust
async fn prepare_channel_status<S: fiber_lsp_sdk::SignerStore>(
    session: &mut fiber_lsp_sdk::HostedSession<S>,
    expected_channel: fiber_types::Hash256,
    result: fiber_json_types::GetChannelSigningStatusResult,
    now_ms: u64,
) -> anyhow::Result<fiber_lsp_sdk::ProcessOutcome> {
    anyhow::ensure!(
        fiber_types::Hash256::from(result.channel_id) == expected_channel,
        "签名响应属于其他通道"
    );
    Ok(session.handle_channel_status(expected_channel, result.status, now_ms).await?)
}
```

SDK 按 commitment、revocation、close 或 announcement 选择必须的 `prepare_*` 校验，验证本地 binding、支付授权、双向 commitment 历史、状态转换、nonce 和对方签名证据。`PreparedSigning` 绑定验证时的存储 revision；`confirm` 会重新检查相关状态和时间条件。不要绕到无上下文 `ChannelSigner::prepare`，该入口当前明确拒绝签名。

| `ProcessOutcome` | 应用动作 |
| --- | --- |
| `Idle` | 当前无需签名；它不表示开通或支付已完成 |
| `NeedConfirmation(pending)` | 展示 `pending.prepared.review()`、`commitment_review()` / context 和本地业务条款，取得授权后调用 `session.confirm(pending, fresh_now_ms)` |
| `ReadyToSubmit(params)` | SDK 已完成签名并保存状态，进入可靠提交队列 |
| `Denied` 或错误 | 停止该请求；不要用“用户确认”绕过验证失败 |

`HostedSession::new` 默认 `Auto`。它只自动批准版本大于 1 且没有出站变化的已核验 commitment；开通首轮、出站变化、撤销、关闭和公告仍需确认。`Manual` 每次要求显式确认。业务可把用户对一次付款的明确授权作为后续已核验协议签名的依据，但必须限制通道、付款 hash、金额和允许的 transition，不能照抄测试 agent 的无条件确认循环。本项目在 [payments.rs](../fiber-lsp-client/src/payments.rs) 中使用 `Manual` 并限定单笔操作范围。

以下是**应用流程片段**；`ui`、`outbox`、`rpc`、`clock` 均为应用接口，省略实现：

```rust
let params = match outcome {
    ProcessOutcome::Idle => return Ok(()),
    ProcessOutcome::Denied => anyhow::bail!("策略拒绝签名"),
    ProcessOutcome::ReadyToSubmit(params) => params,
    ProcessOutcome::NeedConfirmation(pending) => {
        ui.require_approval(&pending).await?;
        session.confirm(pending, clock.now_ms()?).await?
    }
};
outbox.save_request_and_params(&original_status, &params).await?;
match params {
    SubmitParams::Channel(p) => rpc.call("submit_channel_signature", p).await?,
    SubmitParams::Watchtower(p) => rpc.call("submit_watchtower_signature", p).await?,
}
```

实际 JSON-RPC `params` 使用 `[p]`，请求携带 tenant token；`SubmitParams::Channel` 已包含 `next_material`，不要自行补造 nonce 或下一轮公有材料。SDK 返回参数不会替应用 POST，提交成功也不能直接认定业务完成。

提交超时属于结果未知。保存原始完整状态和提交参数，重连 / 查询当前状态，确认 channel、request ID 和内容一致后才考虑重放；仅相同 request ID 不够。当前客户端开通 / 关闭会保存待提交签名并重核验；收付款只在本次进程内保存。遇到 `SigningStateChanged` 应重新读取状态、准备和确认，不复用过时的授权对象，也不回滚 SDK 历史快照。

## 7. 收付款授权、原像释放和完成验证

### 先保存精确授权

`PaymentAuthorization` 是单通道、固定金额、单分片授权，字段包括 `payment_hash`、`hash_algorithm`、方向 `inbound`、精确 `amount`、新 TLC 接受截止时间 `expires_at_ms`、最短剩余 TLC 时长 `min_tlc_expiry_delta_ms` 和绝对过期上限 `max_tlc_expiry_ms`，时间均为毫秒。授权金额使用通道资产单位；CKB 为 shannons，1 CKB = 100,000,000 shannons。

下面两个函数可编译；传入的授权必须先由钱包核对发票或本地创建条款，不能从待签名 settlement 反向构造：

```rust
async fn authorize_outgoing<S: fiber_lsp_sdk::SignerStore>(
    session: &fiber_lsp_sdk::HostedSession<S>,
    channel: fiber_types::Hash256,
    authorization: fiber_lsp_sdk::PaymentAuthorization,
) -> anyhow::Result<()> {
    session.channel_signer(channel).await?
        .authorize_payment(authorization).await?;
    Ok(())
}

async fn authorize_incoming<S: fiber_lsp_sdk::SignerStore>(
    session: &fiber_lsp_sdk::HostedSession<S>,
    channel: fiber_types::Hash256,
    authorization: fiber_lsp_sdk::PaymentAuthorization,
    preimage: fiber_types::Hash256,
) -> anyhow::Result<()> {
    session.channel_signer(channel).await?
        .authorize_invoice(authorization, preimage).await?;
    Ok(())
}
```

`authorize_payment` 要求 `inbound == false`；`authorize_invoice` 要求 `inbound == true`，并验证持有匹配原像。后者不会把原像发给 LSP，也不表示可以立即释放。`PaymentRegistry` / `registry_mut().record_*` 只是旧的 hash 记账接口，不能代替这些持久化授权。

### 出站付款

1. 本地解析并验证签名发票，核对网络 / 资产、收款公钥、固定金额、hash、算法、期限及路由。保存付款意图、开始时两条 commitment lane 的版本和准确费用预算。
2. 先 `authorize_payment`，再通过 tenant token 调用标准 `send_payment`。授权金额必须覆盖该通道中实际创建的完整 TLC 金额，不能默认等于发票金额；trampoline 可能加入费用预算。本项目按当前 Fiber 默认 0.5% 预算计算精确出站 TLC 金额，并限制 `max_parts = 1`，详见 [payments.rs](../fiber-lsp-client/src/payments.rs)。生产应用应显式约束费用并与实际路由参数保持一致。
3. 持续处理 `channel_signing`，同时用 `get_payment` 查询本次 hash。返回原像时先用授权的 hash 算法验证，再 `signer.authorize_fulfillment(hash, preimage).await?`，允许后续 commitment 将该 TLC 移除并结算。
4. 远端 `Success` 仍需原像和下面的本地完成核验。发送响应丢失时先查该 hash；不要因超时重新发送一笔付款。

### 入站收款与安全释放原像

1. 客户端生成并安全保存随机原像、hash、收款条款和操作进度，调用 `authorize_invoice`。通过 tenant token 调标准 `new_invoice`，只传 `payment_hash` 创建 hold invoice，不把原像交给 LSP。当前 SDK 授权模型应关闭 MPP。
2. 验证返回的发票地址与对象一致，并匹配本地金额、hash、算法、有效期、资产、注册租户的 `invoice_pubkey`、目标 LSP 的 trampoline hint 等，再展示给付款人。LSP 会补入 Public T 的路由 hint 并登记 hosted invoice。
3. 保持签名驱动在线。`get_invoice` 的 `Received` 只表示收到待结算付款。读取 `signer.recovery_records()`，等待包含该入站 TLC 的本地 lane（`for_remote == false`）具备 `complete_signature`。
4. 用本地新鲜时间和独立 `ChainVerifier` 调用 `authorize_preimage_release`；成功后立即调用标准 `settle_invoice` 释放原像。不要把先前成功检查无限期复用。

实际释放调用如下，可编译；`chain` 必须使用钱包认可的链数据源，返回的 `CommitmentReference` 可记录到业务日志中作为依据：

```rust
async fn permit_release<S, C>(
    session: &fiber_lsp_sdk::HostedSession<S>,
    channel: fiber_types::Hash256,
    hash: fiber_types::Hash256,
    preimage: fiber_types::Hash256,
    fresh_now_ms: u64,
    chain: &C,
) -> anyhow::Result<fiber_lsp_sdk::CommitmentReference>
where
    S: fiber_lsp_sdk::SignerStore,
    C: fiber_lsp_sdk::ChainVerifier,
{
    Ok(session.channel_signer(channel).await?
        .authorize_preimage_release(hash, preimage, fresh_now_ms, chain).await?)
}
```

该 API 会验证 funding cell 仍存活且资产 / 容量匹配、本地存在完整且未撤销的可恢复 commitment、TLC 与发票授权和原像一致，并检查剩余追偿时间窗口；成功后持久化释放许可和 fulfillment 意图。它本身不传输原像，也不保存原像字节。应用应在持久化付款记录中自行保管原像，预留网络延迟，并在实际发送前保证许可仍新鲜。本项目额外预留 30 秒作为检查余量，这是应用策略，不是 SDK 固定要求。

`authorize_fulfillment` 只记录 TLC 结算意图，不能替代上述安全释放流程；`authorize_cancellation(hash)` 批准退款，不能覆盖已批准的 fulfillment。原像可能已公开后，不应再按“发送失败”处理为可退款状态。

### 何时向用户宣布完成

SDK 没有一个自动完成全部付款核验的 `wait_payment` 接口。应用继续驱动签名，至少联合确认：

- 远端 `get_payment == Success` 或 `get_invoice == Paid`；付款返回原像与原 hash 匹配。
- 两条 commitment lane 在本次操作之后确实接受过该 hash、方向和精确金额的 TLC，避免把旧历史当作本次完成证据。
- 两条 lane 的最新状态都移除了 TLC、余额一致，本地 lane 具有完整恢复签名；实际余额转移由 SDK 的 commitment 验证约束。
- 远端余额与本地最终余额一致，且该操作没有未结算 TLC。CKB 通道 SDK 的余额包含 reserve，而 RPC `local_balance` / `remote_balance` 是可用余额，比较时需加回各自 reserve。

参考 [payments.rs 的 reconcile](../fiber-lsp-client/src/payments.rs) 和 [channel.rs 的 settled_balances](../fiber-lsp-client/src/channel.rs)。本项目只推进单笔在线支付，双方需保持在线；其 `Operation`、原像和起始版本当前未持久化，因此超时 / 退出后的跨进程付款恢复尚未实现。开发生产应用时，应先补业务 journal 和结果未知的对账流程，再提供自动恢复。

## 8. 关闭、链上恢复与能力边界

协作关闭前，先读取最新恢复记录，确认双 lane 余额一致、没有存活 TLC、本地完整恢复签名就绪，并独立核对 funding outpoint 和链上资产。保存本地关闭意图、最终余额和精确费用分摊，再调用：

```rust
async fn approve_cooperative_close<S: fiber_lsp_sdk::SignerStore>(
    session: &fiber_lsp_sdk::HostedSession<S>,
    channel: fiber_types::Hash256,
    terms: fiber_lsp_sdk::CloseAuthorization,
) -> anyhow::Result<()> {
    session.channel_signer(channel).await?.authorize_close(terms).await?;
    Ok(())
}
```

`CloseAuthorization { fee, local_fee_share, remote_fee_share }` 使用 shannons，两方分摊之和必须等于总费用。之后通过 tenant token 调标准 `shutdown_channel`，协作关闭用 `force: Some(false)`，并继续处理签名。SDK `prepare_close` 会校验已批准费用、双方退款脚本、精确输出金额、资产、funding input 和 nonce 会话；应用还需查链确认最终关闭交易及双方退款，再标记关闭完成。参考 [channel.rs](../fiber-lsp-client/src/channel.rs)。

当前 hosted session 只支持一个已批准通道；pending signer 也只有一个。服务端保存同租户单私有通道关联，关闭不会自动清空记录以允许另开通道。本项目用新的独立账户 / 根身份建立下一轮，保留历史账户和通道证据；这属于应用账户管理，不是 SDK 的 `reset` / `reopen` API。

链上追偿另有完整验证路径，不等于协作关闭：

- `recovery_records()` 提供已验证 commitment、settlement、会话证据和可用的完整签名；`revocation_records()` 提供已验证撤销证据。不要把只有 partial signature 的记录当成已可单独执行的链上恢复交易。
- 应用为一次支出构造 `OnchainSpendAuthorization`，批准准确的来源 commitment、目的脚本、总费、辅助 inputs 和可信 deps。
- 完整 `ChainVerifier` 必须独立核验 live cell、commitment 继承关系、所有 input 的成熟条件和链中位时间，并处理重组。使用 `handle_watchtower_status_verified(..., authorization, chain)` 准备，取得用户确认后调用 `confirm_onchain(pending, chain)`，它会重查链状态并生成 `SubmitParams::Watchtower`。
- 普通 `handle_watchtower_status` 缺少上述证据；当前会经过被拒绝的无上下文 preparation，不能用于实际链上签名。本项目 [Wallet 的 ChainVerifier 实现](../fiber-lsp-client/src/wallet.rs)仅支持原像释放所需的 `live_cell`，其他方法明确返回“不支持链上追偿”，不能直接复制为完整链上恢复适配器。

根密钥、旧快照或 LSP 查询结果都不能单独重建已经丢失的完整 signer 历史。开通和关闭可以围绕持久化意图、冻结交易、待提交签名与链状态设计重试；付款是否能恢复取决于应用是否保存了业务状态和秘密。保留原始数据并核对，不通过清空数据或回滚快照消除签名冲突。

## 参考实现入口

| 要实现的部分 | 优先阅读 |
| --- | --- |
| SDK 接口与验证边界 | [session.rs](../../fiber/crates/fiber-lsp-sdk/src/session.rs)、[signer.rs](../../fiber/crates/fiber-lsp-sdk/src/signer.rs)、[policy.rs](../../fiber/crates/fiber-lsp-sdk/src/policy.rs) |
| 持久化与账户恢复 | [store.rs](../fiber-lsp-client/src/store.rs)、[account.rs](../fiber-lsp-client/src/account.rs) |
| JSON-RPC / WebSocket | [rpc.rs](../fiber-lsp-client/src/rpc.rs)、[events.rs](../fiber-lsp-client/src/events.rs)、[RPC 类型](../../fiber/crates/fiber-json-types/src/channel.rs) |
| 开通、资金核验与关闭 | [channel.rs](../fiber-lsp-client/src/channel.rs)、[wallet.rs](../fiber-lsp-client/src/wallet.rs)、[opening.rs](../../fiber/crates/fiber-lsp-sdk/src/opening.rs) |
| 收付款与完成判定 | [payments.rs](../fiber-lsp-client/src/payments.rs)、[SDK commitment 验证](../../fiber/crates/fiber-lsp-sdk/src/commitment.rs) |
| 链上核验要求 | [onchain.rs](../../fiber/crates/fiber-lsp-sdk/src/onchain.rs)、[lifecycle.rs](../../fiber/crates/fiber-lsp-sdk/src/lifecycle.rs) |

参考客户端包含 devnet CKB、默认钱包锁、单笔在线支付等应用限制；SDK 类型虽包含 UDT 和链上恢复能力，适配器仍需分别实现与验证。不要把参考客户端的固定金额、网络常量或开发环境密钥来源直接带到其他网络。
