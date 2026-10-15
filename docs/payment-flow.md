# 通道与收付款流程

开通道、独立查询、协作关闭及收付款恢复命令已实现。四用户开关、退款和下一轮，以及 U3 / U4 双向在线收付款已通过实际 devnet 验证；离线和故障恢复仍需单独验证。在线出款依赖当前同级 Fiber 工作区已有的提前原像查询修改；旧版本曾在 TLC 移除签名前缺少 resolution 证据而拒签。验证结果及双终端命令见 [快速开始](../README.md#7-收付款及离线恢复)。

## 联调路径与余额

最短联调路径是：

```text
普通 Fiber 节点 ⇄ LSP ⇄ 客户端用户的私有通道
```

与普通节点互付时先准备普通节点与 LSP 的通道。账户之间互付使用两条到 LSP 的私有通道即可。启动脚本不会连接节点或自动开通道。新版 `admin init-lsp` 在 LSP 出资配置缺失或为 null 时设置每条新通道 1000 CKB，保留显式配置和原通道条款。

付款需要客户端一侧有可用余额，收款需要 LSP 一侧有可用余额；两个方向都要扣除通道保留金额和所需费用。首次演示建议固定支付 1 CKB，即 `100000000` shannons，RPC 十六进制金额为 `0x5f5e100`。

## 开通道：一次准备，后续复用

以下通道命令已实现：

```bash
./lsp-cli admin init-accounts
./lsp-cli create-channel --funding-amount 1000 -u 1
./lsp-cli status -u 1
```

开通使用已有账户及 LSP 公钥，读取本地 Node3 的链与通道配置，用户 1～4 分别从 offckb #6～#9 出资（示例用户 1 为 #6），LSP / Public T 从 #5 出资。示例中本方 1000 CKB（含 100 CKB 保留金额：99 CKB 占用容量和 1 CKB 关闭手续费）、新初始化环境对方默认 1000 CKB，通道总出资为 2000 CKB 加实际交易费，#6 无需承担对方注入额。钱包核验双方 inputs、各自找零、合约、实际扣款和费用后，仅签名 #6 的 inputs，SDK 核验开通条款和初始 commitment；最终等待链上出资确认及 `ChannelReady`。

`status` 同时展示账户信息和当前账户的通道、余额、出资 / 关闭 hash 和待恢复阶段；查询不提交任何签名。中断后 `./lsp-cli create-channel -u 1` 核对并恢复原意图，不改变身份或重复出资。

```bash
./lsp-cli close-channel -u 1
./lsp-cli status -u 1
```

协作关闭要求本地已验证余额一致且没有未结算 TLC，退款使用开通时绑定的双方钱包脚本，新通道本方退回所选用户的 #6～#9、对方退回 #5；旧版共用 #5 的通道继续按原脚本核对。关闭费在本地独立计算，由发起方承担；只有关闭交易链上确认且退款匹配才报告成功。中断后重复 `close-channel` 继续原操作。当前同租户关闭后再开通受服务端 / SDK 单通道记录限制，旧记录保留。命令、金额及出资条件见 [README](../README.md#6-四用户与通道)。

## 付款：接收对端发票

普通节点 `new_invoice` 创建有签名的 devnet 固定金额 CKB 发票，例如 amount 为 `0x5f5e100`（1 CKB），currency 为 `Fibd`，expiry 为 `0xe10`（3600 秒）。交给客户端：

```bash
./lsp-cli pay '<普通节点 invoice_address>' --max-fee 0.01 --timeout 120 -u 1
```

客户端本地解析并核验签名、网络、金额、资产和有效期，通过已绑定私有通道到 Public T 的 trampoline 付款；`max_parts=1`。trampoline 可能花费完整的费用预算，因此 SDK 授权的通道 TLC 金额是发票金额加原费用预算，而不只是发票金额。第一次付款显式传入 `--max-fee`，恢复保持原金额和费用，不改变路由授权。

发送前保存意图和 SDK 授权，落盘“可能已发送”后调用 `send_payment`。网络或响应失败后 `resume` 只查询原 hash，不自动重发，即使远端当前返回不存在也保留未知结果。服务端 `Failed` 后只核对 TLC 清理及退款，不自动新尝试。

`Success` 不是单独的完成依据：须核验正确 preimage、SDK 双 lane 中原 TLC 的接受及移除、最新余额和远端余额。状态尚未同步时显示远端成功、待本地核对。

## 创建发票：默认退出或在线等待

```bash
./lsp-cli create-invoice --amount 1 --expiry 3600 -u 1
./lsp-cli create-invoice --amount 1 --expiry 3600 --wait -u 1
./lsp-cli create-invoice --amount 1 --expiry 3600 --wait --timeout 120 -u 1
```

首次显式指定金额；expiry 默认 3600 秒。客户端先生成并保存秘密和 SDK 授权，调用 `new_invoice` 时只提供 payment hash，生成 hold invoice。核对地址、hash、金额、有效期、算法、租户发票公钥及 Public T 路由提示，再保存、输出并刷新 stdout。

默认输出后退出；不启动后台进程。`--wait` 输出后保持前台，使用与 `watch` 相同的签名与安全结算逻辑；无需另外启动 watch。带 `--wait` 时默认持续等待，可用 `--timeout` 限制本次等待。创建响应丢失后按原 hash 查询，明确不存在时复用原秘密和 hash 创建，无法区分的错误不能被当作不存在。

## 离线及中断恢复

```bash
./lsp-cli resume --payment-hash 0x... --timeout 120 -u 1
./lsp-cli watch -u 1
./lsp-cli payment-status --payment-hash 0x... -u 1
./lsp-cli invoice-status --payment-hash 0x... -u 1
./lsp-cli list-payments -u 1
```

所有命令支持 `--config PATH`，日常操作必选 `-u N`。仅 resume、payment-status、invoice-status、list-payments 支持互斥 `--account ID` 历史入口；pay、create-invoice、watch 只处理用户当前轮次，用户空闲时不会回退历史。账户归档保留 `payments.json`、SDK store、密钥和秘密。`resume` 有界推进原操作；watch 前台持续推进全部原授权。同通道的签名可能同时涉及其他原授权操作。

默认离线演示：创建发票 → CLI 输出后退出（停止同账户其他推进进程）→ 对端付款 → 有效窗口内 resume 或 watch 上线 → 签名、安全释放秘密、核对入账。对端离线期间可能处理中，不能先等付款成功再上线。CLI 退出仅停止签名，托管租户或私有通道可能继续运行；因此不能把退出宣称为真实 `Deferred`。

真正的私有通道离线缓冲需显式管理侧操作，并通过 `lsp_get_payment_delivery` 核对 `Deferred`、截止和失败原因。普通用户命令通过 `get_payment_delivery` 使用用户凭证查询本人参与的投递进度，不读取管理 token；接口不唤醒租户，不暴露其他租户身份或通道。发票和付款查询仍可能唤醒租户；没有可见 delivery 不能推断“没有付款到达”。LSP 必须运行才能暂存付款，重启后的恢复需单独验收。

发票有效期、CLI 等待超时、上游缓冲截止与 TLC 安全恢复窗口是不同概念。超时 / Ctrl-C 不取消付款或发票。发票 `Expired` 时若已有 TLC，继续按实际 TLC 核对；不能仅凭发票过期报告网络付款失败。没有入账 TLC 且发票过期时停止此次等待，上游结果仍需核对。已接受的 TLC 真正过期且秘密未释放时，只授权退款，不释放秘密；安全窗口不足时明确报错并保留原资料。

## 前台事件等待

`create-invoice --wait`、`pay`、`watch` 和 `resume` 仅在命令运行期间，通过 `wait_channel_update` 等待 LSP 通知，不启动后台常驻进程。租户凭证只能等待本租户已有通道；通知仅包含通道 ID 和不透明游标，不发送原像、签名请求内容或其他租户信息。同一租户的通道、发票、付款和原像持久化变化会更新游标。

客户端先读取游标，再核对和推进状态，随后携带上次游标等待。若推进期间发生变化，等待立即返回；否则服务端异步挂起，最多 5 秒后返回供客户端补查。重启节点会产生新游标，后续等待返回后重新核对状态。监听请求失败时退出本次推进并保留恢复资料，网络恢复后使用原 hash `resume` 或 `watch` 重新读取游标；不自动重发付款。外层 `tokio::select!` 继续处理 Ctrl-C 和命令超时。

新接口需要升级 Fiber 节点及 CLI。收付款的 SDK 校验、签名前持久化、释放秘密前链上 live 校验及最终余额核对保持不变。

2026-10-14 新版真实 devnet 验证：沿用账户 1 / 2 的原通道，账户 2 向账户 1 支付 1 CKB；收款端 `create-invoice --wait` 与付款端 `pay` 均以成功退出，分别在付款 CLI 启动后约 691ms / 970ms 完成核对。原 hash、签名状态及支付记录已保留，详见 [快速开始](../README.md#7-收付款及离线恢复)。此单次端到端结果不作为协议性能基准。

## 签名、结算及余额核对

签名仅限 commitment 和 revocation。`get_channel_signing_status` 的不可信请求必须通过 SDK 必须验证器，核对原授权、金额、两条 commitment lane 的状态变化及 nonce；返回的签名先持久化再提交。提交响应丢失时重新核验原请求后重放相同响应，不签冲突内容。

收款 `Received` 表示尚待结算。仅当 SDK 最新本地 commitment 中存在匹配入账 TLC、具有完整可恢复签名、独立 CKB RPC 确认原 funding 仍 live 且剩余窗口充分时，才释放 preimage 并立即调用 `settle_invoice`。释放检查额外保留 30 秒 RPC 预算；每次重试都重新检查，不因本地已记录释放意图而绕过检查。

LSP delivery 的 `Succeeded`、发票 `Paid`、SDK TLC 移除及余额增加是不同证据。最终成功要核对 SDK 双 lane 及远端余额；服务端报告不能代替本地证明。协作关闭仍拒绝未结算 TLC 或未同步的双 lane，退款依据结算后的 SDK 最新余额和原出资脚本，不再假设原注入余额。

SDK 边界见 [fiber-lsp-sdk README](../../fiber/crates/fiber-lsp-sdk/README.md)。首期不支持 UDT、MPP、无金额发票、keysend、自动重试发送、后台常驻服务或链上 TLC 追偿。不自动清理运行数据或更换账户，也不自动开新通道。

## 分阶段耗时

`pay`、`create-invoice --wait`、`watch` 和 `resume` 使用 `--timings` 在命令结束时输出中文摘要，时间自动换算为秒、毫秒或微秒，多次阶段显示累计、次数及最长单次。准备时间是收付款流程进入 drive 前的区间，账户恢复属于其子项；外部签名下分 SDK、恢复资料保存和提交请求。状态刷新、收款释放及存储存在包含关系，不能把各行直接相加。单张发票的 create-invoice --wait / resume 另记 payment.wait_for_receipt（本次开始在线推进到首次观察到匹配 hash 的入账 TLC）和 payment.receipt_processing（该观察点到本次核对结束）。只排除前者，处理中的后续签名/结算等待仍计入收款处理，并通过 payment.receipt_protocol_wait 单列。不要从命令时间扣除整个 LSP 等待区间；旧聚合日志没有这个分界，不能回推出精确收款处理耗时。超时或中断会显示处理尚未完成，尚未观察到付款时不报告已完成的处理耗时。恢复命令只统计本次窗口，watch 多笔操作不冒充单笔延迟。

`--timings-json` 保留原机器可读计时 schema，供自动基准采集；不与 `--timings` 同时使用。未指定两者时不采集或输出计时。
