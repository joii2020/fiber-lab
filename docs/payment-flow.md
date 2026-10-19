# 通道与收付款流程

当前收付款范围为双方在线的 1v1 场景；通道开通、查询和协作关闭继续保留。双终端命令见 [快速开始](../README.md#7-双方在线的-1v1-收付款)。

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
./lsp-cli create-channel --funding-amount 1000 -u 1 --rpc-url http://127.0.0.1:21716
./lsp-cli status -u 1 --rpc-url http://127.0.0.1:21716
```

开通使用已有账户及 LSP 公钥，从本地普通 Node1 读取钱包链与合约部署，通过 LSP 的 `node_info` 查询出资条款并核对链一致，用户 1～4 分别从 offckb #6～#9 出资（示例用户 1 为 #6），LSP / Public T 从 #5 出资。示例中本方 1000 CKB（含 100 CKB 保留金额：99 CKB 占用容量和 1 CKB 关闭手续费）、新初始化环境对方默认 1000 CKB，通道总出资为 2000 CKB 加实际交易费，#6 无需承担对方注入额。钱包核验双方 inputs、各自找零、合约、实际扣款和费用后，仅签名 #6 的 inputs，SDK 核验开通条款和初始 commitment；最终等待链上出资确认及 `ChannelReady`。

`status` 同时展示账户信息和当前账户的通道、余额、出资 / 关闭 hash 和待恢复阶段；查询不提交任何签名。中断后 `./lsp-cli create-channel -u 1 --rpc-url http://127.0.0.1:21716` 核对并恢复原意图，不改变身份或重复出资。

```bash
./lsp-cli close-channel -u 1 --rpc-url http://127.0.0.1:21716
./lsp-cli status -u 1 --rpc-url http://127.0.0.1:21716
```

协作关闭要求本地已验证余额一致且没有未结算 TLC，退款使用开通时绑定的双方钱包脚本，新通道本方退回所选用户的 #6～#9、对方退回 #5；旧版共用 #5 的通道继续按原脚本核对。关闭费在本地独立计算，由发起方承担；只有关闭交易链上确认且退款匹配才报告成功。中断后重复 `close-channel` 继续原操作。当前同租户关闭后再开通受服务端 / SDK 单通道记录限制，旧记录保留。命令、金额及出资条件见 [README](../README.md#6-四用户与通道)。

## 双方在线的单笔收付款

当前仅支持 A 创建发票、B 付款，双方保持在线直到结算完成。每次命令只推进当前一笔操作；不创建或发送其他历史意图，不提供 watch、resume 或收付款查询命令。

```bash
# 终端 A：创建后直接在线等待
./lsp-cli create-invoice -u 1 --amount 1 --expiry 3600 --rpc-url http://127.0.0.1:21716
# 终端 B：复制 A 输出的完整地址
./lsp-cli pay -u 2 '<invoice_address>' --timeout 120 --rpc-url http://127.0.0.1:21716
```

双方需提前准备到 LSP 的私有通道及对应方向流动性。CLI 仅处理 devnet 固定金额 CKB 发票，使用单笔 trampoline 付款（max_parts=1）。RPC 不传 max_fee_amount 和 max_fee_rate，使用 Fiber 默认费用预算。SDK 的精确 TLC 授权按同级 Fiber 当前默认规则计算：ceil(金额 × 5 / 1000) shannons；付款 TLC 授权金额为发票金额加该预算。

### 收款

客户端在内存中生成 preimage、payment hash 和本地授权，使用 hash 创建 hold invoice，核对发票签名、金额、算法、有效期、租户发票公钥和 LSP 路由提示。输出并刷新 invoice_address 后持续推进本次收款。

当发票为 Received，且 SDK 最新本地 commitment 包含该 TLC 并有完整恢复签名时，通过 SDK 检查 funding 仍 live 及安全窗口，再调用 settle_invoice 释放秘密。每次释放尝试均重新检查，额外保留 30 秒 RPC 预算。

create-invoice 默认一直等待，不再提供 --wait；--timeout 可限制本次等待时间。

### 付款

客户端本地验证发票签名、网络、资产、金额、有效期及余额，在内存中授权后调用 send_payment。相同 hash 已有远端付款时拒绝发送，不自动重试。

轮询 get_payment 获取并验证 preimage，再向 SDK 授权 fulfillment，以签署 TLC 移除。所用 Fiber 节点需支持在移除签名前通过 get_payment 提供原像。

### 签名与完成

通过 `GET /lsp/events` 接收待签名请求，直接使用事件中的签名状态推进，无需轮询 `get_channel_signing_status` 或在签名步骤之间固定等待。另设独立的 500ms 定时器查询本次付款 / 发票状态并尝试收款结算；观察到成功后再核对恢复记录和余额。签名仅允许 commitment 和 revocation，拒绝包含其他付款 hash 的 TLC 请求。请求必须通过 SDK 校验；签名响应缓存在本次内存中，相同请求重复出现时核验并使用同一响应，不额外落盘。

SSE 使用已有用户 Bearer token；传输中断后退避重连并重放待签名快照，续接使用原 `X-Lsp-Session-Id`。同租户新连接会接管签名会话，旧客户端收到 409 后退出；401 / 403 同样退出。当前 SSE 不推送付款结果和余额，因此保留状态定时查询。

远端 Paid / Success 只是完成证据之一。最终还需核对双 lane 中本次 TLC 曾被接受、已移除、最新 SDK 余额与远端余额一致。收款还须确认本地已释放秘密。

超时、Ctrl-C、付款失败、发票过期或 RPC 错误会停止本次推进并以失败退出。网络付款不会因此取消；本次业务状态和秘密不额外落盘，退出后丢失；已有付款文件不读取、不改写或删除，SDK 底层 signer 存储沿用。当前不处理离线恢复、批量推进或失败 TLC 的自动清理。

### 耗时

--timings 输出中文摘要；--timings-json 提供机器可读计时，两者互斥。单张发票分别统计等待付款、收款处理和其中的协议等待，后续签名与结算等待计入处理耗时。

`--timings` 还会在退出时按本端首次观察的顺序输出收付款时序，每步以 Tab 分隔输出 `[index]\t[收付款]\t本次时间ms\t总共耗时ms\t描述`，时间固定为毫秒、保留一位小数；本次时间为距上一步的间隔，总共耗时从本次操作开始累计：

- 收款端：账户就绪、生成秘密及授权 → hold invoice 创建、校验并输出 → 首次提交包含本次 TLC 的签名 / 观察到 Received → 本地承诺及完整恢复签名就绪 → funding 与安全窗口检查通过 → settle_invoice 返回 → 观察到 Paid → TLC 移除与余额核对完成。
- 付款端：账户就绪、发票和余额校验及授权 → send_payment 返回 → 首次提交包含本次 TLC 的签名 → 获取并验证 preimage、授权结算 → 观察到 Success → TLC 移除与余额核对完成。

Received 与签名步骤按实际观察顺序显示，重复查询或 SSE 重放不会重置计时。区间包含其中的 RPC、签名和协议等待，表示本端观察延迟，不能直接当作对端内部执行耗时；收款端等待也包含人工复制发票时间。超时、中断或失败时保留已记录步骤，另显示最后一步至退出的耗时。原有函数、RPC、签名和磁盘累计统计继续保留，与时序区间有包含关系。

JSON 在原有 `metrics` 外增加 `payment_steps` 数组（`id`、`label`、`elapsed_us`、`since_previous_us`）与 `payment_elapsed_us`。累计时间从对应收款或付款函数入口起算，第一步区间包含账户恢复；两端独立计时，不跨终端合并。时序不记录发票、hash 或 preimage 等业务内容。

`bash scripts/test_send_and_pay.sh` 自动执行同机用户 1 收款、用户 2 付款，并通过 `payment_started_unix_us` 对齐两端的时序数据。先打印 A 的原始日志、本端时序及耗时统计，再打印 B 的对应报告，最后追加合并时序。合并输出按实际观察时间排序，每步注明参与方，区间为距全流程上一步的时间，累计从流程最早的操作入口起算。脚本使用 Python 3 汇总，单端报告直接使用 JSON 内的 `human_report`，与 CLI 的中文输出保持一致。普通双终端命令仍各自输出本端统计。任一端失败或缺少可合并的计时数据时，保留业务输出及错误，并以非零状态退出。

SDK 边界见 [fiber-lsp-sdk README](../../fiber/crates/fiber-lsp-sdk/README.md)。历史运行数据不删除，payments.json 不再参与收付款流程。
