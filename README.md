# Fiber Lab

在本地 CKB devnet 上测试 Fiber LSP 的通道和收付款。SDK 接入说明见 [Fiber LSP SDK](docs/fiber-lsp-sdk.md)。

## 准备

将配套的 Fiber 源码放在同级 `../fiber`，安装其所需的 Rust 工具链与系统构建依赖，以及 `offckb`、`curl`、`jq`、Python 3.9+。以下命令均在本项目根目录执行。

```bash
# 编译本地 fnn 和客户端，创建 ./lsp-cli 链接
bash scripts/build-fiber.sh
```

脚本配置见 [scripts/config.sh](scripts/config.sh)：默认使用 `../fiber/target/release/fnn`，CKB RPC 为 `http://127.0.0.1:8114`，LSP HTTP / WebSocket RPC 分别为 `http://127.0.0.1:21716`、`ws://127.0.0.1:21717`。offckb 数据位于 `${XDG_DATA_HOME:-$HOME/.local/share}/offckb-nodejs/devnet`；创建和启动时保持相同的 `XDG_DATA_HOME`。

## 启动并开通通道

首次创建环境（同时启动节点、初始化四个用户和管理 token）：

```bash
bash scripts/create.sh
```

已有环境在停止后重新启动，复用原数据；已运行时跳过：

```bash
bash scripts/start.sh
```

offckb 管理 devnet 和两个普通 Fiber 节点，Shell 单独启动 Node3 作为 LSP。启动后等待 HTTP / WebSocket RPC 就绪，LSP 日志在 `.state/devnet/lsp.log`。可执行下面的检查，失败时稍等后重试：

```bash
curl --noproxy '*' -fs --max-time 2 http://127.0.0.1:21716 \
  -H 'Content-Type: application/json' \
  -H "Authorization: Bearer $(cat configs/keys/operator-token)" \
  --data '{"jsonrpc":"2.0","id":1,"method":"lsp_get_status","params":[]}' \
  | jq -e '.error == null and .result.public_node_id != null'
```

测试前按顺序为用户 1、2 开通通道，已有通道可复用：

```bash
./lsp-cli create-channel -u 1 --rpc-url http://127.0.0.1:21716
./lsp-cli create-channel -u 2 --rpc-url http://127.0.0.1:21716
```

首次开通默认本方和 LSP 各出资 1000 CKB。用户 1～4 可分别并行操作，同一用户只运行一个命令；并行开通还要求 LSP 出资钱包 #5 有足够的独立可用 cells。`admin init-accounts` 需等全部用户命令退出后执行。

## 跑测试

在线收付款：用户 1 收取用户 2 的 1 CKB，输出双方日志及合并后的收付款时序。

```bash
bash scripts/test_send_and_pay.sh
```

模拟收款方创建发票后暂停，付款提交后等待 5 秒再恢复：

```bash
bash scripts/test_offline_receive_and_pay.sh
# 自定义暂停时间，需小于两端的 120 秒等待超时
OFFLINE_SECONDS=10 bash scripts/test_offline_receive_and_pay.sh
```

这两个脚本均不启动节点，需保持 CKB、LSP 和两个通道可用，并有对应方向的流动性。暂停测试保留进程内存，不测试退出进程后重启恢复。

无需启动 devnet 的测试：

```bash
cargo test --locked --manifest-path fiber-lsp-client/Cargo.toml
python3 scripts/test_payment_timings.py
```

Rust 单元测试统一放在 `fiber-lsp-client/tests/unit/`，CLI 集成测试放在 `fiber-lsp-client/tests/` 根目录。

更多命令可运行 `./lsp-cli --help` 或 `./lsp-cli <命令> --help`；手动收付款可加 `--timings` 查看耗时。

## 停止

```bash
bash scripts/stop.sh
```

停止后保留节点、账户和通道数据，下次使用 `start.sh` 继续。`scripts/reset-all.sh` 会清理并重建整个环境，再按用户 1、2 的顺序开通通道，仅在明确需要清空数据时使用。
