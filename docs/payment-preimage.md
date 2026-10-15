# 外部签名付款的 preimage 时序问题

本文记录 2026-10-10 在 Fiber PR [#1623](https://github.com/nervosnetwork/fiber/pull/1623) 最新提交 `8db6a1175cc94b6f9386953fbd3e3ebbed2ff3f8` 上发现的付款阻断，以及 lsp-cli 的手动恢复实现。自动向付款客户端提供结算原像仍需修改 Fiber；CLI 的补录入口用于已经取得真实原像时恢复原付款。

## 现象及结论

用户 1 创建金额 1 CKB 的 hold invoice 并在线等待，用户 2 以费用上限 0.01 CKB 付款。用户 1 报告收款完成，用户 2 最后报错：

```text
invalid signing content: commitment validation: TLC removed without a locally authorized resolution
```

错误来自 `fiber-lsp-sdk` 的 commitment 校验：新状态移除了旧 TLC，但本地仍未授权 fulfillment 或 cancellation。这个拒签保护应保留。根因是 Fiber 外部签名付款流程没有在签署 TLC 移除前，通过付款查询向客户端提供 preimage。

路径为 `U2 → Public T / LSP → U1`，涉及两条独立私有通道。U1 的下游收款完成，并不代表 U2 的上游付款已完成。账户列表的 `ready` 也不证明通道已无未结算 TLC。

最初提供的日志使用 hash `0xcd7a654fd0c939279493873212490dcc77d28bb2dd8a7c531564b2f27998a8f1`；现场现存交易使用 hash `0x7e2fd5b552785bf98496c30975c9b1cece49b2c39b21f28e689782b1234b2b04`。以下动态证据针对现存交易，不能作为最初那笔交易的完整执行轨迹。

## 为什么 Fiber 已收到原像，CLI 仍然拒签

三个值或事件需要分别核对：

| 内容 | 能证明什么 |
| --- | --- |
| `payment_hash` / CLI 的“付款 hash” | 付款标识，等于 `hash(preimage)`，不能反推出原像 |
| 节点日志 `store payment_preimage` | Fiber 服务端已经收到并保存原像 |
| 付款 RPC 返回非空 `payment_preimage` | 该原像已通过查询交给付款客户端；客户端还需校验并授权 |

Fiber 的 [`handle_remove_tlc_peer_message`](https://github.com/chenyukang/fiber/blob/8db6a1175cc94b6f9386953fbd3e3ebbed2ff3f8/crates/fiber-lib/src/fiber/channel.rs#L1865-L1876) 收到 `RemoveTlcFulfill` 后会校验并保存 preimage。但付款响应来自 payment session 的 attempt，[只提取已经成功的 attempt 的原像](https://github.com/chenyukang/fiber/blob/8db6a1175cc94b6f9386953fbd3e3ebbed2ff3f8/crates/fiber-lib/src/fiber/payment.rs#L636-L643)。

attempt 成功和原像写入 attempt，则要等 TLC 移除确认，ChannelActor 发出 `TlcRemoveReceived`，PaymentActor 再处理事件。这形成循环依赖：

```text
CLI 取得 preimage → SDK 授权 fulfillment → 签署 TLC 移除
       ↑                                      ↓
付款 RPC 返回 preimage ← attempt 成功 ← TLC 移除确认
```

lsp-cli 原本已在处理签名请求之前调用 `get_payment`，取得非空原像时校验 hash 并调用 `authorize_fulfillment`。因此单纯调整轮询顺序或增加重试次数不能解决这个循环。

## 现场诊断证据

修复前只读查询得到：

| 检查项 | 结果 |
| --- | --- |
| U1 发票及本地记录 | `Paid` / `complete`，已完成收款 |
| U2 付款 | `Inflight`，没有 `payment_preimage` |
| U2 本地付款记录 | `pending`，没有 preimage |
| U2 通道 TLC | `RemoteRemoved`，金额 1.01 CKB |
| U2 待签请求 | `CompleteReceivedCommitment`，settlement 中的 TLC 列表为空 |
| U2 SDK 双 commitment lane | 最新 version 均为 2，各仍有一个 TLC |

将 U2 的 SDK 快照恢复到独立内存 store，处理真实待签请求，复现了完全相同的拒签。仅在内存中提供 U1 已释放的原像并调用 `authorize_fulfillment` 后，同一个请求通过校验，返回 `NeedConfirmation`。这一步没有生成或提交签名，也没有修改持久化账户资料；它证明了缺少结算授权的原因，单独不能证明整笔付款已完成。

## lsp-cli 的修复及使用方法

先重新编译客户端，保留运行节点和账户资料：

```bash
cargo build --manifest-path fiber-lsp-client/Cargo.toml --locked --release
```

已从收款方取得真实原像后，在付款方执行；将示例中的 hash 和 preimage 换成同一笔付款的实际值：

```bash
./lsp-cli resume -u 2 --payment-hash 0x... --preimage 0x... --timeout 120
```

该参数是 `0x` 前缀的 32 字节原像，只处理所选账户已提交的原付款。补录前先查询并核对通道仍为 `ChannelReady`；已关闭或不可链下结算时退出，不写入新的结算授权。随后核对付款方向、发送标记、原 hash 算法和既有证据，再调用 SDK `authorize_fulfillment`，保存 `payments.json` 中的原像。SDK 已授权的取消不可覆盖；错误原像或 SDK 拒绝不会写入新的付款证据。金额、费用、invoice 和发送标记保持原值。

重启后，`resume` / `watch` 会从保存的原像恢复 SDK fulfillment 授权，即使服务端仍返回 `Inflight` 且没有原像。中断后继续原 hash，无需再次提供：

```bash
./lsp-cli resume -u 2 --payment-hash 0x... --timeout 120
```

SDK 授权与 CLI 记录分别持久化；若 SDK 授权已成功而 CLI 保存失败，重新提供同一个原像即可重试。补录本身不代表付款成功，客户端仍需核对远端 `Success`、双 commitment lane 的原 TLC 接受及移除、通道余额和未结算 TLC。已提交付款不会再次调用 `send_payment`。

缺少结算授权的 SDK 错误现在附带 `resume --preimage` 的恢复提示；其他校验错误仍保持原拒签行为。CLI 不自动读取另一账户秘密，收款也继续使用原有完整恢复证明、live funding 和安全窗口检查，补录入口不用于收款方向。

### 本地 U1 / U2 联调时取得已释放原像

此例只适用于操作者同时拥有两个测试账户，且 U1 已报告收款完成。先停止同账户的其他推进命令，再按原 hash 从 U1 当前轮次的完成记录取得原像。将第一行换成实际原付款 hash；若交易属于历史轮次，应选择原收款身份目录，不要使用当前轮次目录。

```bash
PAYMENT_HASH='0x7e2fd5b552785bf98496c30975c9b1cece49b2c39b21f28e689782b1234b2b04'
RECEIVER_ACCOUNT="$(jq -er '.users[] | select(.number == 1) | .current_account' .state/fiber-lsp-client/accounts.json)"
PAYMENT_PREIMAGE="$(jq -er --arg hash "$PAYMENT_HASH" '.operations[] | select(.inbound and .hash == $hash and .release_started and .phase == "complete" and .remote_status == "Paid") | .preimage // empty' ".state/fiber-lsp-client/accounts/$RECEIVER_ACCOUNT/payments.json")"
./lsp-cli resume -u 2 --payment-hash "$PAYMENT_HASH" --preimage "$PAYMENT_PREIMAGE" --timeout 120
unset PAYMENT_PREIMAGE
```

不要把付款 hash 当作原像。补录成功及结算校验错误不会打印原像；上述方式也不把具体秘密写入命令文本。它不是正常付款的自动取证方式：普通付款人事前不知道收款人的原像，需要由服务端在正确的协议阶段提供。

### 已强制关闭的通道

补录必须在通道仍可链下结算时进行。此次现存 U2 通道 `0xad09a4873b65b848288e14b726a549b16a7f8c2fbaf8cf5fe310801d90a86237` 已在 2026-10-10 13:58:12（香港时间，日志 UTC 05:58:12）因 peer ACK 等待超时被强制关闭。后续包含关闭记录的通道查询返回 `Closed / UNCOOPERATIVE_REMOTE|WAITING_ONCHAIN_SETTLEMENT`。

该状态下，即使 `get_channel_signing_status` 仍有持久化旧请求，且 `get_payment` 仍显示 `Inflight`，也不能继续链下结算。`resume --preimage` 会在保存补录证据前停止。保留账户、SDK store 和原付款记录；链上 TLC 追偿及 watchtower 失败排查需要另行处理，本次修改不自动清理、重建或关闭其他通道。

## 为什么已有测试没有发现

[LSP 设计文档 #1625](https://github.com/nervosnetwork/fiber/issues/1625) 定义了真实通道和外部签名边界，收款、付款与恢复是不同验证范围。[远程签名集成 PR #8 的验证报告](https://github.com/chenyukang/fiber/pull/8) 给出了当时 SDK、LSP、watchtower 和迁移测试的通过结果，但不能代替最新代码上完整发票出款流程的验证。

在当前代码里，部分 Rust 测试通过 [`FIXTURE_PREIMAGES`](https://github.com/chenyukang/fiber/blob/8db6a1175cc94b6f9386953fbd3e3ebbed2ff3f8/crates/fiber-lib/src/fiber/tests/external_signer_restart.rs#L308-L335) 预先持有付款原像，发现 TLC 被移除时[直接提供给 SDK](https://github.com/chenyukang/fiber/blob/8db6a1175cc94b6f9386953fbd3e3ebbed2ff3f8/crates/fiber-lib/src/fiber/tests/external_signer_restart.rs#L384-L395)。它们验证 SDK 签名与状态恢复，但绕过了付款客户端只能从 RPC 学到原像的关键环节。

最新 [独立 SDK agent README](https://github.com/chenyukang/fiber/blob/8db6a1175cc94b6f9386953fbd3e3ebbed2ff3f8/tests/fiber-lsp-sdk-agent/README.md) 明确说明尚未实现主动付款控制。因此现有通过报告与本问题并不矛盾。

## 上游修复及验证边界

正式自动付款流程需要 Fiber 在收到并校验 `RemoveTlcFulfill` 后，先持久化并向所属付款客户端暴露原像或待结算证据，然后等待外部签名。证据应绑定原付款、通道 / TLC 和租户 namespace；付款状态仍保持 `Inflight`，不能提前标记 `Success`。

需要增加独立付款客户端的真实发票 E2E：付款方事前不知道 preimage，仅通过公开且有租户鉴权的 RPC 取得证据；保持正常 SDK commitment 校验，最后同时核对付款 `Success`、收款 `Paid`、双 lane 和双方余额。离线、错误原像、跨租户访问、提交丢失及重启恢复需分别覆盖。

本次 CLI 验证结果：

- `cargo test --locked --offline`：79 项测试通过，覆盖原像校验、收款方向和未发送付款拒绝、SDK 取消授权不可覆盖、持久化及重启恢复、远端无原像时保留证据，以及不重复发送。
- `cargo clippy --all-targets --locked --offline -- -D warnings`、格式检查和 release 编译通过；仓库入口 `./lsp-cli` 已使用重新编译的版本。
- 使用现存交易的真实原像执行关闭通道拒绝检查，确认返回明确错误，未输出原像，U1 / U2 账户文件、SDK store、付款记录及账户索引内容均未改变。

现存 U2 通道已强制关闭，因此没有验证本次 CLI 补录后整笔链下付款完成。内存诊断仅证明真实待签请求能在补充正确授权后通过 SDK 校验；完整自动出款、离线及链上追偿不在本次验证的通过范围内。
