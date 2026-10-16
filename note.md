## 使用

以下命令均在 `fiber-lab` 项目根目录执行，仅记录用法，运行输出后续补充。

### 准备

需要 Linux / Bash、Node.js 20+、已安装且可直接调用的 `offckb`、Rust 工具链，以及 `curl`、`jq`。
Fiber 源码放在同级目录 `../fiber`，需包含 LSP 实现和 `crates/fiber-lsp-sdk`；系统编译依赖见 [Fiber README](../fiber/README.md)。

编译 Fiber 节点和本项目 CLI，生成根目录的 `lsp-cli` 入口：

```bash
./scripts/build-fiber.sh
./lsp-cli --help
```

### 环境搭建

首次创建环境，或清理后重新创建：

```bash
./scripts/create.sh
```

自动初始化 CKB devnet、两个普通 Fiber 节点、一个 LSP、四个客户端账户和管理凭证，并启动环境。已有环境无需重复创建。

已有环境停止后，再次启动：

```bash
./scripts/start.sh
```

`create.sh` 已启动环境，完成后无需再执行 `start.sh`。后续统一使用此启动脚本，避免用 `offckb fiber start --nodes 3` 覆盖 LSP 配置。

查看普通节点状态、LSP 状态和日志：

```bash
offckb fiber status

curl --noproxy '*' -fsS http://127.0.0.1:21716 \
  -H 'Content-Type: application/json' \
  -H "Authorization: Bearer $(cat configs/keys/operator-token)" \
  --data '{"jsonrpc":"2.0","id":1,"method":"lsp_get_status","params":[]}' \
  | jq -e 'if .error then error(.error.message) else .result end'

tail -f .state/devnet/lsp.log
```

使用完后停止（数据自动保存）；下次执行 `start.sh`：

```bash
./scripts/stop.sh
```

需要丢弃整套环境时才执行清理。此命令会先停止环境，再删除 devnet、全部 Fiber 节点、项目 `.state/` 和管理凭证；重建使用 `create.sh`：

```bash
./scripts/clean.sh
```

### 账户&通道

固定账户编号为 1～4，日常操作使用 `-u N` 选择账户。金额单位均为 CKB。

查看账户列表、历史轮次及单个账户的通道和余额：

```bash
./lsp-cli accounts
./lsp-cli accounts --history
./lsp-cli status -u 1
```

为账户 1、2 分别开通到 LSP 的通道。首次开通默认出资 1000 CKB，如需自定义可使用 `--funding-amount`。按顺序执行，等待前一条完成：

```bash
./lsp-cli create-channel -u 1
./lsp-cli create-channel -u 2
./lsp-cli status -u 1
./lsp-cli status -u 2
```

不再使用通道时协作关闭；完成后可查看历史，或再次开通新一轮通道：

```bash
./lsp-cli close-channel -u 1
./lsp-cli accounts --history
./lsp-cli status -u 1 --round 1
# 需要再次使用时执行
./lsp-cli create-channel -u 1
```

### 收付款

以下以账户 1 收款、账户 2 付款为例。双方需已开通通道，付款方本方可用余额需覆盖金额及费用，收款方通道的 LSP 一侧需有足够可用余额。

终端 A：账户 1 创建 1 CKB 发票，有效期 3600 秒。复制输出中的完整 `invoice_address`，保持命令运行，等待收款完成：

```bash
./lsp-cli create-invoice -u 1 --amount 1 --expiry 3600
```

终端 B：将 `<invoice_address>` 替换为上面的完整发票地址，由账户 2 支付，使用 Fiber 默认费用预算（1 CKB 对应 0.005 CKB）：

```bash
./lsp-cli pay -u 2 '<invoice_address>' --timeout 120
```

双方命令结束后，查看余额：

```bash
./lsp-cli status -u 1
./lsp-cli status -u 2
```

创建发票后默认保持在线，双方都需持续运行到完成。当前只处理本次单笔收付款，不提供离线或中断恢复；业务状态仅保存在进程内存中，不读写 payments.json。超时或 Ctrl-C 不取消网络付款，不要直接重复付款。

完整用法见 [README](README.md#7-双方在线的-1v1-收付款)。
