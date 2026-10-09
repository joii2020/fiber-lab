# 必要前置草案：付款移除签名前的 resolution 证据

审核：待确认；这是执行 2.1.1 第 8 步发现的必要前置草案，尚未正式编号或改变归属。
授权：待审核；现授权禁止修改服务端，本草案涉及 Fiber 服务端既有 RPC 的证据返回时序，须确认新增范围及执行顺序后实施。
进度：未开始（草案；待确认，不正式编号）

## 需求依据与技术范围

依据 [方案 2](2-lsp-cli-payments.md) 的在线付款、原 hash 恢复及未结算关闭保护，并影响 [2.1.1](2.1.1-payment-engine.md) 第 8 步。拟在服务端现有 `get_payment` 为原租户、原付款 hash 提前返回经过密码学核验的 fulfillment preimage；不新增接口、不让 CLI 使用管理凭证或跨租户取证、不削弱 SDK 移除 TLC 的授权校验。

建议新增二级职责 **2.2 付款 resolution 证据时序**，其三级详细方案拟为 **2.2.1**；2.2 的实际交付应先于剩余 2.1 验证，符合倒序。当前只是建议，不正式改号或新增执行索引条目。现 2.1.1 已完成代码及多用户验证保留，恢复点不变。开发者确认技术路线、授权扩展及顺序后再细化三级、调整索引与依赖。

## 当前事实

2026-10-09 实际 devnet 复现：

- 用户 1 第二轮账户 `account-18dcdc0efde4f70f`，tenant `5b35038a11aa9ebf08e4a96426898a59a04a4d766dfe3dfae1cb0b84308b18db`；原付款 hash `0x631c948f33e959e93e306ac25c6fe00e6dca67044460d9494388c31066ddf2b3`，普通节点收 1 CKB，原费用预算 0.01 CKB，授权精确 TLC 为 1.01 CKB。
- 普通 Node1 `get_invoice` 已 `Paid`。客户端租户 `get_payment` 仍 `Inflight`、`payment_preimage = None`，没有可核验的失败证据。
- `get_channel_signing_status` 为 `SignatureRequired / CompleteReceivedCommitment`，请求 ID `0xe563017bd6e4aa1759dc6ea174fabc2cca58a0350eea81c1edbfa5aeec4697a0`；候选 settlement 无 TLC，双方总余额分别为 998.99 / 100.01 CKB，表示向对方转移原 1.01 CKB 的成功移除，而非退款取消。
- SDK 拒绝：`TLC removed without a locally authorized resolution`。再次 `resume` 复现相同障碍，没有重发原 hash。
- 客户端实际有效余额仍 901 / 0 CKB，存在 1.01 CKB offered TLC；不得用候选 settlement 当作已完成流动性，不得关闭、归档或删除记录绕过。

服务端源码依据（只读核对，未修改）：

- `fiber/crates/fiber-lib/src/fiber/payment.rs` 的 `From<PaymentSession> for SendPaymentResponse`（当前约 640 行）只从 `attempt.is_success()` 中取 preimage。
- `fiber/crates/fiber-lib/src/fiber/channel.rs::apply_settled_remove_tlcs`（约 1227 行）要求 `RemoveAckConfirmed` 后才能 `apply_remove_tlc_operation`；后者约 1965 行才向付款 actor 发 `TlcRemoveReceived`。
- 同文件约 1846 行在收到并核对 `RemoveTlcFulfill` 时已经保存经 hash 校验的 preimage，但此刻尚未进入付款成功状态。
- `fiber/crates/fiber-lsp-sdk/src/commitment.rs` 约 519 行必须有本地 `Resolution::Fulfill` 或经允许的取消授权才能签 TLC 移除；此保护应保留。

实际工作环：**移除 commitment 签名 → RemoveAckConfirmed / payment Success → get_payment 返回 preimage → SDK authorize_fulfillment → 移除 commitment 签名**。仅改方案编号不能解除这个运行时环。

证据保存在项目 `.state/verification/20261009/`：`outbound-blocking-signing-status.json`、`outbound-blocking-remote-payment.json`、`outbound-blocking-channel.json`、`outbound-ordinary-paid.json`、`user1-online-outbound.log`、`outbound-recovery.log`。部分 RPC 数据含付款秘密，目录中的证据不应公开原文；方案仅记录公开标识与状态。

### 签名阻断后的既有超时强关

首次 CLI 失败日志落盘时间为 **12:26:08 UTC**；同通道 LSP 日志在 **12:26:38.347 UTC** 明确记录 `is inactive for a time, shutting down it forcefully`，随后广播关闭交易 `0x3af28c1db9ea73e26b6be0408ba9c34c20fa2c8d9d489d7eb2134195a276b5ac`。实际 LSP 重启在 **12:31:48 UTC**，所以强关发生于重启之前；重启后客户端才读到远端 Closed，不能归因于重启。

链上该交易已 `committed`，唯一输出是 CommitmentLock，容量 1099 CKB 减 456 shannons，**没有双方退款输出**。本地 channel.json 仍 Ready、SDK 仍有未结算 TLC、原付款仍 Inflight。既有协作关闭的 ClosedVerified 不适用于此结果；未更新关闭意图、未归档或清空用户指针。

源码 `channel.rs::CheckActiveChannel / peer_does_not_reply_ack_in_time / set_waiting_ack` 与 release `PEER_CHANNEL_RESPONSE_TIMEOUT = 30 * 1000` 符合此次观察。新增服务端前置还需要核对外部签名等待期间的 peer ACK / 超时策略与安全时间窗口，避免把一般 peer 无响应和合法 hosted 客户端停止签名混同。保持正常 peer 安全保护及 TLC 到期边界，不能简单无限增加超时；具体改动需在新增授权后细化。

证据：`current-channel-force-close-summary.json`、`restart-unexpected-close-chain.json`、`lsp-restart-offline-init-evidence.json`、`restart-original-payment.json`、`restart-original-resume.log`。LSP 停止期间执行 init-accounts 的所有账户/SDK/清单哈希未变；重启保留六个注册租户及原 hash，但完整支付恢复没有通过。

## 技术要点

拟优先调整现有 `get_payment`：对该付款所属租户/节点，在成功移除签名前返回该原 hash 已接收且严格核验的 preimage，同时保持实际状态 `Inflight`，不要提前把状态改为 `Success`。CLI 的现有 `refresh` 已验证 hash 并调用 `authorize_fulfillment`；得到证据后继续原 SDK 签名、双 lane / revocation 与余额核验才报告完成。

需先细化证据来源：优先使用付款第一跳相关 channel 中已校验 fulfillment 的持久 preimage；核对 payment hash、算法、租户存储、相关 attempt / channel 和实际 receipt，不能以无关全局 hash 记录、候选余额、普通节点 `Paid` 或管理 delivery 状态代替证明。现有 JSON optional `payment_preimage` 字段可复用，避免接口形状变更；旧普通节点成功结果继续兼容。

取消移除也需独立核对时序：若服务端同样在移除签名后才返回 `Failed`，详细方案应提供可验证、原操作绑定的失败证据，或确认现有接口能取得；不能因 `Inflight` / 无 preimage 猜测取消。范围应以实际复现为依据细化，不顺带新增支付或管理功能。

## 风险与依赖

- 新增服务端范围超出现授权“不改服务端”，须开发者确认；不在本草案阶段修改 sibling Fiber 仓库或 SDK。
- 返回 preimage 与标记付款完成是不同事实；不得提前完成账务或隐藏未结算 TLC。
- 保留租户授权及存储隔离；普通用户不能读取另一租户的秘密。日志及查询展示继续隐藏 preimage。
- 证据持久化需能跨 LSP 重启恢复；接收 fulfillment、写 preimage、生成外部签名请求之间故障也应可恢复。
- 提交后失联仍不重发原 hash，沿用已有 send_started 保护。保留目前 devnet 的全部账户及通道，特别是未完成原付款；外部签名时序与超时策略须授权，已有链上 TLC 处置另行明确范围。

## 验收条件

1. 实际有普通节点收款且客户端仍 `Inflight` 时，原租户 `get_payment` 可得到原 hash 的有效 preimage；其他租户及错误 hash 不得取得。
2. CLI 保持原授权、原 request/hash 完成 SDK 双 lane 签名、撤销与结算，随后 `Success`、TLC 清理和余额一致；只扣一次 1.01 CKB。
3. 候选余额、错误 preimage、跨租户记录、缺失持久 receipt 均不能授权移除；SDK 原拒绝测试保持。
4. 断点与 LSP 重启后原 hash 可恢复，不能重复付款或提前报告完成；普通节点原 API 和现有 JSON 字段兼容。
5. 现有原 hash 单独做只读对账与链上 TLC 状态调查；不得承诺其仍可链下恢复。链上追偿不属于原方案 2，CommitmentLock/TLC 处置范围须另行明确。
6. 在未来可用的新验证轮次完成原收款、停止签名、Deferred、超时/过期、故障注入及重启场景，不清理旧身份或擅自归档当前指针，按真实证据验收。

## 执行记录与恢复点

已完成：只读源代码查证、真实复现与证据保存、草案路线及编号建议。
剩余工作：开发者确认必要前置、授权扩展与顺序；确认后细化 2.2.1、服务端实现及验证，再恢复 2.1.1。
验证结果：真实 Node1 已 `Paid`、客户端 `Inflight`、无 preimage 与移除签名阻断已复现；草案实现测试未运行。
未验证项：服务端修复、安全边界与上述验收全部未实施/验证。
恢复点：保留账户 `account-18dcdc0efde4f70f` 的 channel.json、payments.json、root-key、signer.bin、account.json；当前通道已因服务端 ACK 超时强关，原 hash 不能假设仍可链下恢复。确认前不要用新付款、手工秘密注入、取消、关闭、归档或清理替代原操作；保留当前指针做对账，链上 TLC 调查/处置范围待明确。
