# fiber-lab

Linux 本地 Fiber + hosted LSP 联调环境。offckb 管理 CKB devnet 和初始节点配置，fiber-lab 管理实际运行的 fnn 进程。默认 Node1 为普通节点，Node2 为 hosted LSP；支持每个节点运行不同项目编译出的 fnn。

## 快速开始

依赖：Node.js >= 20、pnpm、Python >= 3.10、Git；编译 Fiber 时还需该项目的 Rust 工具链。

```bash
cd ~/code/fiber-dev/fiber-lab
./scripts/setup-offckb.sh
# 已有最新编译产物时可跳过
./scripts/build-fiber.sh ../fiber
./scripts/doctor.sh
./scripts/start.sh
./scripts/status.sh
./scripts/logs.sh 2 -f
```

setup 从官方源码构建已核对的 offckb 版本 cd16d015，而不是安装缺少 fiber 命令的 npm 0.4.13。可通过 OFFCKB_REVISION 指定其他 revision。源码在 .tools/offckb；不修改全局 offckb。

start 首先启动隔离的 CKB daemon，再由 offckb 启动普通节点以生成链配置、账户、密码和合约依赖；随后停止这些普通节点，按 profile 启动真实节点。Node2 通过命令行启用 lsp 与 Biscuit 鉴权，最后调用 connect_peer 连接节点。重复 start 会拒绝覆盖运行中的节点。

## 常用命令

```bash
./scripts/rpc.sh 1 node_info
./scripts/rpc.sh 2 node_info
./scripts/rpc.sh 1 list_channels '[{}]'
./scripts/stop.sh          # 停止 fnn，保留 CKB
./scripts/start.sh         # 保留状态重启
./scripts/stop.sh --ckb    # 停止 fnn 和 CKB，保留数据
```

rpc 自动给 LSP 请求附加 profile 指定的 operator token。输出的 RPC 结果可能包含敏感租户数据，请按实际用途保管。

## 配置不同 Fiber 项目

编辑 profiles/devnet.json。路径相对于 fiber-lab 根目录解析，也支持绝对路径和 ~/ 路径。

- nodes[].binary：各节点 fnn 可执行文件，例如 ../fiber/target/release/fnn 或 ../fiber-other/target/release/fnn。
- nodes[].lsp：开启 hosted LSP。
- nodes[].args：额外 fnn 参数数组。
- biscuit_public_key / biscuit_private_key / operator_token_file：LSP 鉴权配置。
- operator_token_key：从 .bru 文件读取指定键；省略时把 token 文件全部内容视为 token。
- auto_accept_shannons：LSP 自动接受通道时出资；默认 500 CKB。

可复制 devnet.json 为 other.json，所有命令都支持 --profile other：

```bash
./scripts/start.sh --profile other
./scripts/status.sh --profile other
./scripts/stop.sh --profile other --ckb
```

节点 ID 必须从 1 连续递增，最多 16 个。offckb 固定端口，因此各 profile 应依次运行，不能同时启动。不同二进制需兼容同一链上合约与协议。

## 目录与端口

运行数据在 .state/<profile>/，工具源码在 .tools/，均被 Git 忽略。XDG 路径为各 profile 单独设置，不使用已有 offckb 的全局数据。默认节点文件在：

```
.state/devnet/data/offckb-nodejs/devnet/fiber/nodes/<id>/
```

| 服务 | RPC | P2P |
| --- | --- | --- |
| CKB | 8114（代理 28114） | offckb 默认配置 |
| Node1 | 21714 | 8344 |
| Node2 / LSP | 21715 | 8345 |

节点日志在 .state/devnet/node-1.log 和 node-2.log。停止脚本通过 Linux /proc 的进程启动时间校验 PID，避免误杀复用该 PID 的进程。启动失败会停止本次启动的 fnn 并保留日志及 CKB 供排查。

## 支付与租户测试

启动成功表示链、Fiber/LSP RPC 可用且已发起 peer 连接。不会自动创建租户或支付通道。接下来用 LSP SDK 注册租户，创建有流动性的通道，再测试 invoice/send_payment。

默认使用 ../fiber/tests/nodes/biscuit-test-private-key 与 tests/bruno/environments/test.bru 中的开发测试凭据。仅用于本地 devnet，不能用于生产。不要在 shell 中全局设置 RPC_BISCUIT_* 等 Fiber 配置环境变量；它们会影响 offckb 的初始化探测。

offckb 内置合约基于 FNN v0.9.0 / e6cb7ac。更换 fnn 二进制不会自动更换链上合约；修改合约的分支需另行部署匹配合约并调整配置。仓库现有 Bruno LSP 测试还依赖其自身节点与 SDK agent，不能直接当作本环境的测试命令运行。

不提供自动清理链数据的脚本。确认不再需要状态后，先执行 stop.sh --ckb，再手动清理对应 .state/<profile>。
