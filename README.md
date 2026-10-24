# Fiber Lab

本地 Fiber LSP 联调工具。环境与启动方式见 [devnet 与节点管理](docs/devnet.md)，通道和收付款步骤见 [通道与收付款流程](docs/payment-flow.md)。

## 在线收付款与 WebSocket

使用同级 `../fiber` 源码编译支持 WebSocket 签名订阅的 LSP，并重新编译客户端：

```bash
bash scripts/build-fiber.sh
```

LSP 需要独立的 WebSocket RPC 监听。`admin init-lsp` 会在未配置时设置 `rpc.ws_listening_addr: 127.0.0.1:21717`，保留已有自定义地址。已有环境升级时，停止 LSP 后运行 `./lsp-cli admin init-lsp --config <Node3/config.yml>`，再启动升级后的 LSP；不需要重建 devnet 或清理账户数据。也可手动添加该配置后重启。

双方提前开通私有通道、准备对应方向流动性，然后分别在两个终端执行：

```bash
./lsp-cli create-invoice -u 1 --amount 1 --expiry 3600 --rpc-url http://127.0.0.1:21716 --ws-url ws://127.0.0.1:21717
./lsp-cli pay -u 2 '<invoice_address>' --timeout 120 --rpc-url http://127.0.0.1:21716 --ws-url ws://127.0.0.1:21717
```

`--ws-url` 默认 `ws://127.0.0.1:21717`；远程或自定义监听需显式指定，可使用 `wss://`。HTTP RPC 和签名订阅使用独立端口。客户端使用已保存的用户 Bearer token 订阅 `channel_signing`，仅接收当前通道，收到待签名请求立即经过 SDK 校验并提交签名；每 500ms 查询付款或发票状态，成功后核对 TLC 移除和余额。

WebSocket 中断时自动退避重连并重新订阅，服务端自动重发当前待签名请求；不再使用 SSE 或会话 ID。401 / 403 或订阅被拒绝时停止，并提示检查地址和权限。用户 token 需有 `write("subscriptions")` 及通道读取权限，新版租户注册已包含订阅权限，旧租户 token 缺少权限时需由服务端重新签发，管理 token 不能替代用户 token。每个租户仍只运行一个在线收付款客户端，避免重复签名。

双方须保持在线直到结算完成。超时或中断不会取消网络付款，当前版本不支持退出后的收付款恢复；已有运行数据不会自动清理。

查看各步骤耗时，在两端命令中分别加上 `--timings`：

```bash
./lsp-cli create-invoice -u 1 --amount 1 --expiry 3600 --rpc-url http://127.0.0.1:21716 --timings
./lsp-cli pay -u 2 '<invoice_address>' --timeout 120 --rpc-url http://127.0.0.1:21716 --timings
```

退出时 stderr 会先按本端观察顺序打印收付款时序，每步以 Tab 分隔输出 `[index]\t[收付款]\t本次时间ms\t总共耗时ms\t描述`，耗时固定为毫秒、保留一位小数，再输出原有累计统计。步骤覆盖发票创建、付款提交、TLC 签名、收款确认、释放秘密、付款端验证原像、远端成功及最终余额核对；每端只显示自己参与的步骤。收款端等待付款包含人工复制发票时间。超时或失败也会显示已完成步骤及最后一步后的耗时。机器读取可使用互斥的 `--timings-json`，原有 `metrics` 保留，新增 `payment_steps` 与 `payment_elapsed_us`，单位为微秒。

同机自动联调时，下面的脚本会让用户 1 收取用户 2 的 1 CKB，并将两端日志合并为一条完整时序（需要 Python 3.9+，仅用于解析计时 JSON、对齐两端时间和输出报告；收付款由 Rust 客户端执行）。已有环境需先用 `bash scripts/start.sh` 启动节点，并等待 LSP 的 HTTP / WebSocket RPC 就绪：

```bash
bash scripts/test_send_and_pay.sh
```

输出依次为 A 的原始日志、本端时序及耗时统计，B 的原始日志、本端时序及耗时统计，最后追加合并后的完整时序。完整时序每步标注“收款方 A / 付款方 B”，按共同时间基准排序，显示距全流程上一步的耗时和从流程开始的累计耗时。原始业务日志和错误保留并标注来源。JSON 的 `payment_started_unix_us` 用于对齐两端，`human_report` 保留 CLI 原有中文耗时报告；请先重新编译客户端。脚本仅进行收付款，需提前准备通道和余额。

模拟收款方创建发票后离线、付款提交后再上线，先重新编译客户端，再运行：

```bash
cargo build --locked --release --manifest-path fiber-lsp-client/Cargo.toml
bash scripts/test_offline_receive_and_pay.sh
# 付款提交成功后继续离线 10 秒（默认 5 秒，需小于两端 120 秒等待超时）
OFFLINE_SECONDS=10 bash scripts/test_offline_receive_and_pay.sh
```

该脚本不会启动节点。创建发票前需连接 CKB RPC `http://127.0.0.1:8114` 校验链，LSP 的 HTTP / WebSocket RPC 也须就绪。若仅 CKB 已停止、LSP 仍在运行，执行 `offckb node --daemon` 恢复 CKB 即可；全部节点停止时使用 `bash scripts/start.sh` 启动，复用已有数据。

脚本同样让用户 1 收取用户 2 的 1 CKB，输出两端日志及合并时序。收到发票后立即用 `SIGSTOP` 暂停收款进程，启动付款方；收到 `send_payment` 成功后输出的“付款已提交，等待结算”，再等待指定秒数并用 `SIGCONT` 恢复收款方，双方继续完成签名和结算。“付款后”指付款已提交，最终付款成功仍需收款方上线。暂停保留内存中的收款秘密，只模拟停止处理签名和结算，不会主动断开 WebSocket；不测试退出进程后重启恢复。已有运行数据不会清理。
