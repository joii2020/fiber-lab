# 通道与收付款流程

开通道、独立 `list-channel` 查询及协作关闭已实现。`pay`、`create-invoice` 和 `watch` 仍为后续设计，其示例命令尚不可执行。当前可执行流程见 [快速开始](../README.md)。

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
./lsp-cli list-channel
```

开通使用已有账户及 LSP 公钥，依据本地 Node3 配置授权 #5 钱包的双方注入额。示例中本方 1000 CKB（含 99 CKB 保留金额）、对方默认 99 CKB，总钱包出资为 1099 CKB 加实际交易费。钱包核验所有 inputs、找零、合约和费用后签名，SDK 核验开通条款和初始 commitment；最终等待链上出资确认及 `ChannelReady`。

`list-channel` 独立展示当前账户的通道、余额、出资 / 关闭 hash 和待恢复阶段；查询不提交任何签名。中断后 `./lsp-cli open-channel` 核对并恢复原意图，不改变身份或重复出资。

```bash
./lsp-cli close-channel
./lsp-cli list-channel
```

协作关闭要求本地已验证余额一致且没有未结算 TLC，退款使用开通时绑定的 #5 默认 lock。关闭费在本地独立计算，由发起方承担；只有关闭交易链上确认且退款匹配才报告成功。中断后重复 `close-channel` 继续原操作。当前同租户关闭后再开通受服务端 / SDK 单通道记录限制，旧记录保留。命令、金额及资金 cells 准备见 [README](../README.md#7-开通查询和协作关闭)。

## 付款：接收对端发票

对端普通节点通过 `new_invoice` 创建 devnet 发票，使用 `currency: "Fibd"`，将 `invoice_address` 交给客户端。预期命令为：

```bash
./lsp-cli pay '<对端的 invoice_address>'   # 待实现
```

`pay` 应完成发票检查、保存 SDK 付款授权与付款进度、调用 `send_payment`、处理 SDK 签名请求并查询 `get_payment`。命令等待最终结果后退出，以 `status: "Success"` 确认付款完成。

## 收款：创建发票并保持在线

预期命令为：

```bash
./lsp-cli create-invoice   # 待实现；默认创建 1 CKB 发票并输出 invoice_address
./lsp-cli watch            # 待实现；持续处理签名和收款结算
```

`create-invoice` 应在本地生成并保存 preimage 和 SDK 收款授权，通过 `new_invoice` 创建只提供 `payment_hash` 的 hold invoice，保存并输出发票。

将 `invoice_address` 交给对端普通节点，由对端调用 `send_payment`。客户端在 `watch` 中处理入账签名和结算，查询 `get_invoice`，以 `status: "Paid"` 确认收款完成。`Received` 表示已收到但尚未结算。

## 签名、结算与恢复

`watch` 应查询 `get_channel_signing_status`，经 SDK 核验后调用 `submit_channel_signature`。入账满足 SDK 的 preimage 释放检查后，调用 `settle_invoice`，再持续查询最终收款状态。

这些操作使用注册返回的用户凭证访问 LSP RPC，服务端从凭证确定用户身份。签名密钥和收款秘密保留在客户端，操作进度需要及时落盘。命令中断后先核对原操作的状态，避免重复出资或付款。

SDK 对开通道结果、通道签名请求和 preimage 释放条件的核验要求见 [fiber-lsp-sdk README](../../fiber/crates/fiber-lsp-sdk/README.md)。本章描述的收付款扩展尚未纳入当前已批准的实现范围；项目范围见 [方案索引](../PLAN.md)。
