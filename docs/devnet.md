# devnet 与节点管理

本章解释 [快速开始](../README.md#快速开始首次配置) 中的环境组成和脚本行为。

## 编译与本地依赖

`scripts/build-fiber.sh` 默认从同级目录 `../fiber` 编译 `fnn`，再编译本项目的 `fiber-lsp-client`。Fiber 构建使用 `--locked --release -p fiber-bin --features fnn/portable`，客户端也使用 `--locked --release`。

项目根目录的 `lsp-cli` 是指向 `fiber-lsp-client/target/release/fiber-lsp-client` 的软链接，便于执行客户端命令。脚本仅在入口不存在时创建它。

客户端的 `fiber-lsp-sdk` 和 `fiber-json-types` 使用本地路径依赖。offckb 默认下载的普通 FNN 不能替代本项目使用的 LSP 构建。

若更换 Fiber 源码目录：

1. 修改 `scripts/config.sh` 的 `FNN_BINARY`，指向新目录的 `target/release/fnn`。
2. 修改 `fiber-lsp-client/Cargo.toml` 中两个 Fiber 依赖的路径。
3. 将源码路径传给构建脚本，重新编译：

   ```bash
   bash scripts/build-fiber.sh /absolute/path/to/fiber
   ```

## 节点分工与首次初始化

本项目使用 Node1、Node2 作为普通 Fiber 节点，Node3 作为 LSP。

首次初始化时，执行 `bash scripts/create.sh`。脚本通过 offckb 先启动三个普通节点，生成当前 devnet 的链配置、合约配置、RPC 地址、端口、CKB 私钥和密码。随后停止 Fiber，执行 `lsp-cli admin init-lsp` 将 Node3 配置为 LSP，再执行 `admin create-token` 生成管理凭证，最后调用 `start.sh` 启动普通节点与 LSP。CKB 在配置期间继续运行，无需手动填写合约哈希或导入 CKB 私钥。

创建脚本要求 Node3 目录与管理 token 均不存在，已有环境时提前退出，避免 offckb 重写 Node3 配置。`clean.sh` 成功后可再次运行创建脚本；创建脚本不会自动清理任何数据。

配置完成后，offckb 只管理 Node1、Node2；Shell 单独启动 Node3，复用其已有配置、密码和密钥。

| 服务 | RPC | Fiber P2P | offckb CKB 账户 |
| --- | --- | --- | --- |
| CKB devnet | `http://127.0.0.1:8114` | — | — |
| 普通 Fiber Node1 | `http://127.0.0.1:21714` | `8344` | `#3` |
| 普通 Fiber Node2 | `http://127.0.0.1:21715` | `8345` | `#4` |
| LSP / Node3（Public T） | `http://127.0.0.1:21716` | `8346` | `#5` |
| 客户端 lsp-cli | 连接 Node3 RPC | — | `#6` |

## 启动与停止脚本

`scripts/start.sh` 按顺序执行：

1. 检查 CKB RPC；正常响应时复用已有 CKB，否则使用 `offckb node --daemon` 启动 CKB devnet。
2. 使用 `offckb fiber start --nodes 2 --binary-path "$FNN_BINARY" --daemon` 启动普通节点。
3. 从 Node3 的 `password` 文件读取密码，作为 `FIBER_SECRET_KEY_PASSWORD`，用 `nohup` 单独启动 LSP。
4. 将日志追加到 `.state/devnet/lsp.log`，将 PID 保存到 `.state/devnet/lsp.pid`。

脚本要求首次配置已经完成，不生成或复制 LSP 密钥。PID 写入只表示进程已启动，不代表 RPC 已就绪，仍需执行 README 中的 RPC 检查。服务启动也不会自动注册客户端、连接普通节点与 LSP，或建立支付通道。

`--nodes 2` 会将 Node3 移出 `fiber/nodes.yml` 的管理列表，保留其目录，只重新生成前两个节点的配置。若再次用 `--nodes 3` 或 `--fiber-nodes 3` 启动，offckb 会重新生成 Node3 的配置并将它作为普通节点启动，因此首次配置后统一使用启动脚本。

`scripts/stop.sh` 根据保存的 PID 停止 LSP，删除 PID 文件，再停止 offckb 管理的普通 Fiber 节点和 CKB。再次执行启动脚本前应停止普通 Fiber 节点和 LSP；CKB 可以保持运行。

## 运行数据

| 路径 | 内容 |
| --- | --- |
| `${XDG_DATA_HOME:-$HOME/.local/share}/offckb-nodejs/devnet/` | CKB 链与 Fiber 节点数据 |
| 上述目录的 `fiber/nodes/3/` | LSP 配置、密码、加密后的 CKB 密钥及 Biscuit 私钥 |
| `.state/devnet/` | LSP 日志和 PID |
| `configs/keys/operator-token` | 管理凭证 |
| `.state/fiber-lsp-client/accounts.json` | 四用户固定钱包、当前轮次与永久历史归属 |
| `.state/fiber-lsp-client/accounts/<ID>/` | 各轮身份、租户凭证、SDK 与通道恢复资料；已有旧付款文件保留 |
| `.state/fiber-lsp-client/default/` | 旧版历史账户；升级不移动 |

客户端默认直接读取 offckb Node3 的 `config.yml`，也可用 `--config PATH` 指定配置。`configs/keys/` 和 `.state/` 已加入 Git 忽略规则，由使用者在本地生成。

停止和重启不清理链、节点或账户数据。节点的密码与加密后的 CKB 密钥需要一起保留；客户端账户文件的关联见 [客户端账户](client-account.md)。

已有 Node4 数据也会保留，不自动迁移或清理。当前客户端连接 Node3，原先关联 Node4 的账户不自动迁移。
