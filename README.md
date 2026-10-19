# Fiber Lab

本地 Fiber LSP 联调工具。环境与启动方式见 [devnet 与节点管理](docs/devnet.md)，通道和收付款步骤见 [通道与收付款流程](docs/payment-flow.md)。

## 在线收付款与 SSE

使用同级 `../fiber` 源码编译支持 `GET /lsp/events` 的 LSP，并重新编译客户端：

```bash
bash scripts/build-fiber.sh
```

双方提前开通私有通道、准备对应方向流动性，然后分别在两个终端执行：

```bash
./lsp-cli create-invoice -u 1 --amount 1 --expiry 3600 --rpc-url http://127.0.0.1:21716
./lsp-cli pay -u 2 '<invoice_address>' --timeout 120 --rpc-url http://127.0.0.1:21716
```

客户端自动使用用户凭证连接 SSE，收到待签名请求立即经过 SDK 校验并提交签名；每 500ms 查询付款或发票状态，成功后核对 TLC 移除和余额。不需要新增 CLI 参数。

SSE 中断时自动退避重连，并用原会话 ID 续接、获取待签名快照。同租户新客户端会接管签名流，旧客户端收到 409 后停止；401 / 403 也会停止。每个租户同时只运行一个在线收付款客户端。

双方须保持在线直到结算完成。超时或中断不会取消网络付款，当前版本不支持退出后的收付款恢复；已有运行数据不会自动清理。

查看各步骤耗时，在两端命令中分别加上 `--timings`：

```bash
./lsp-cli create-invoice -u 1 --amount 1 --expiry 3600 --rpc-url http://127.0.0.1:21716 --timings
./lsp-cli pay -u 2 '<invoice_address>' --timeout 120 --rpc-url http://127.0.0.1:21716 --timings
```

退出时 stderr 会先按本端观察顺序打印收付款时序，每步以 Tab 分隔输出 `[index]\t[收付款]\t本次时间ms\t总共耗时ms\t描述`，耗时固定为毫秒、保留一位小数，再输出原有累计统计。步骤覆盖发票创建、付款提交、TLC 签名、收款确认、释放秘密、付款端验证原像、远端成功及最终余额核对；每端只显示自己参与的步骤。收款端等待付款包含人工复制发票时间。超时或失败也会显示已完成步骤及最后一步后的耗时。机器读取可使用互斥的 `--timings-json`，原有 `metrics` 保留，新增 `payment_steps` 与 `payment_elapsed_us`，单位为微秒。

同机自动联调时，下面的脚本会让用户 1 收取用户 2 的 1 CKB，并将两端日志合并为一条完整时序（需要 Python 3）：

```bash
bash scripts/test_send_and_pay.sh
```

输出依次为 A 的原始日志、本端时序及耗时统计，B 的原始日志、本端时序及耗时统计，最后追加合并后的完整时序。完整时序每步标注“收款方 A / 付款方 B”，按共同时间基准排序，显示距全流程上一步的耗时和从流程开始的累计耗时。原始业务日志和错误保留并标注来源。JSON 的 `payment_started_unix_us` 用于对齐两端，`human_report` 保留 CLI 原有中文耗时报告；请先重新编译客户端。脚本仅进行收付款，需提前准备通道和余额。
