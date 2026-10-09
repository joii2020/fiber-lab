# 通道与收付款流程

开通道、独立查询、协作关闭及收付款恢复命令已实现。当前版本的实际 devnet 收付款、离线和故障场景尚待验证，不能视为验收通过。命令见 [快速开始](../README.md)，验证记录见 [方案 2](../plans/2-lsp-cli-payments.md)。

## 联调路径与余额

最短联调路径是：

```text
普通 Fiber 节点 ⇄ LSP ⇄ 客户端用户的私有通道
```

先准备普通节点与 LSP 之间的可用通道，再为客户端开一条到 LSP 的私有通道。启动脚本不会连接节点或自动开通道。

付款需要客户端一侧有可用余额，收款需要 LSP 一侧有可用余额；两个方向都要扣除通道保留金额和所需费用。首次演示建议固定支付 1 CKB，即 `100000000` shannons，RPC 十六进制金额为 `0x5f5e100`。

## 开通道：一次准备，后续复用

以下通道命令已实现：

```bash
./lsp-cli create-user
./lsp-cli open-channel --funding-amount 1000
./lsp-cli status
```

开通使用已有账户及 LSP 公钥，读取本地 Node3 的链与通道配置，客户端从 offckb #6 出资，LSP / Public T 从 #5 出资。示例中本方 1000 CKB（含 99 CKB 保留金额）、对方默认 99 CKB，通道总出资为 1099 CKB 加实际交易费，#6 无需承担对方注入额。钱包核验双方 inputs、各自找零、合约、实际扣款和费用后，仅签名 #6 的 inputs，SDK 核验开通条款和初始 commitment；最终等待链上出资确认及 `ChannelReady`。

`status` 同时展示账户信息和当前账户的通道、余额、出资 / 关闭 hash 和待恢复阶段；查询不提交任何签名。中断后 `./lsp-cli open-channel` 核对并恢复原意图，不改变身份或重复出资。

```bash
./lsp-cli close-channel
./lsp-cli status
```

协作关闭要求本地已验证余额一致且没有未结算 TLC，退款使用开通时绑定的双方钱包脚本，新通道本方退回 #6、对方退回 #5；旧版共用 #5 的通道继续按原脚本核对。关闭费在本地独立计算，由发起方承担；只有关闭交易链上确认且退款匹配才报告成功。中断后重复 `close-channel` 继续原操作。当前同租户关闭后再开通受服务端 / SDK 单通道记录限制，旧记录保留。命令、金额及出资条件见 [README](../README.md#6-账户与通道)。

## 付款：接收对端发票

普通节点 `new_invoice` 创建有签名的 devnet 固定金额 CKB 发票，例如 amount 为 `0x5f5e100`（1 CKB），currency 为 `Fibd`，expiry 为 `0xe10`（3600 秒）。交给客户端：

```bash
./lsp-cli pay '<普通节点 invoice_address>' --max-fee 0.01 --timeout 120
```

客户端本地解析并核验签名、网络、金额、资产和有效期，通过已绑定私有通道到 Public T 的 trampoline 付款；`max_parts=1`。trampoline 可能花费完整的费用预算，因此 SDK 授权的通道 TLC 金额是发票金额加原费用预算，而不只是发票金额。第一次付款显式传入 `--max-fee`，恢复保持原金额和费用，不改变路由授权。

发送前保存意图和 SDK 授权，落盘“可能已发送”后调用 `send_payment`。网络或响应失败后 `resume` 只查询原 hash，不自动重发，即使远端当前返回不存在也保留未知结果。服务端 `Failed` 后只核对 TLC 清理及退款，不自动新尝试。

`Success` 不是单独的完成依据：须核验正确 preimage、SDK 双 lane 中原 TLC 的接受及移除、最新余额和远端余额。状态尚未同步时显示远端成功、待本地核对。

## 创建发票：默认退出或在线等待

```bash
./lsp-cli create-invoice --amount 1 --expiry 3600
./lsp-cli create-invoice --amount 1 --expiry 3600 --wait
./lsp-cli create-invoice --amount 1 --expiry 3600 --wait --timeout 120
```

首次显式指定金额；expiry 默认 3600 秒。客户端先生成并保存秘密和 SDK 授权，调用 `new_invoice` 时只提供 payment hash，生成 hold invoice。核对地址、hash、金额、有效期、算法、租户发票公钥及 Public T 路由提示，再保存、输出并刷新 stdout。

默认输出后退出；不启动后台进程。`--wait` 输出后保持前台，使用与 `watch` 相同的签名与安全结算逻辑；无需另外启动 watch。带 `--wait` 时默认持续等待，可用 `--timeout` 限制本次等待。创建响应丢失后按原 hash 查询，明确不存在时复用原秘密和 hash 创建，无法区分的错误不能被当作不存在。

## 离线及中断恢复

```bash
./lsp-cli resume --payment-hash 0x... --timeout 120
./lsp-cli watch
./lsp-cli payment-status --payment-hash 0x...
./lsp-cli invoice-status --payment-hash 0x...
./lsp-cli list-payments
```

所有命令支持 `--account ID` 和 `--config PATH`，显式选历史账户不改变默认账户。账户归档保留 `payments.json`、SDK store、密钥和秘密。`resume` 有界推进原操作；watch 前台持续推进全部原授权。同通道的签名可能同时涉及其他原授权操作。

默认离线演示：创建发票 → CLI 输出后退出（停止同账户其他推进进程）→ 对端付款 → 有效窗口内 resume 或 watch 上线 → 签名、安全释放秘密、核对入账。对端离线期间可能处理中，不能先等付款成功再上线。CLI 退出仅停止签名，托管租户或私有通道可能继续运行；因此不能把退出宣称为真实 `Deferred`。

真正的私有通道离线缓冲需显式管理侧操作，并通过 `lsp_get_payment_delivery` 核对 `Deferred`、截止和失败原因。普通用户命令只使用用户凭证，不读取管理 token。当前租户查询可能唤醒租户；没有 delivery 数据不能推断“没有付款到达”。LSP 必须运行才能暂存付款，重启后的恢复需单独验收。

发票有效期、CLI 等待超时、上游缓冲截止与 TLC 安全恢复窗口是不同概念。超时 / Ctrl-C 不取消付款或发票。发票 `Expired` 时若已有 TLC，继续按实际 TLC 核对；不能仅凭发票过期报告网络付款失败。没有入账 TLC 且发票过期时停止此次等待，上游结果仍需核对。已接受的 TLC 真正过期且秘密未释放时，只授权退款，不释放秘密；安全窗口不足时明确报错并保留原资料。

## 签名、结算及余额核对

签名仅限 commitment 和 revocation。`get_channel_signing_status` 的不可信请求必须通过 SDK 必须验证器，核对原授权、金额、两条 commitment lane 的状态变化及 nonce；返回的签名先持久化再提交。提交响应丢失时重新核验原请求后重放相同响应，不签冲突内容。

收款 `Received` 表示尚待结算。仅当 SDK 最新本地 commitment 中存在匹配入账 TLC、具有完整可恢复签名、独立 CKB RPC 确认原 funding 仍 live 且剩余窗口充分时，才释放 preimage 并立即调用 `settle_invoice`。释放检查额外保留 30 秒 RPC 预算；每次重试都重新检查，不因本地已记录释放意图而绕过检查。

LSP delivery 的 `Succeeded`、发票 `Paid`、SDK TLC 移除及余额增加是不同证据。最终成功要核对 SDK 双 lane 及远端余额；服务端报告不能代替本地证明。协作关闭仍拒绝未结算 TLC 或未同步的双 lane，退款依据结算后的 SDK 最新余额和原出资脚本，不再假设原注入余额。

SDK 边界见 [fiber-lsp-sdk README](../../fiber/crates/fiber-lsp-sdk/README.md)。首期不支持 UDT、MPP、无金额发票、keysend、自动重试发送、后台常驻服务或链上 TLC 追偿。不自动清理运行数据或更换账户，也不自动开新通道。
