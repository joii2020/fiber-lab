# fiber-lab

本地 Fiber LSP 联调环境，使用 `lsp-cli` 创建、恢复客户端账户并查询通道。

按下文可以跑完当前已实现的流程：**准备环境 → 生成管理凭证 → 启动 LSP → 注册账户 → 开通道 → 查询通道 → 协作关闭 → 停止或重启**。客户端已支持开通道、协作关闭和独立通道查询；付款和收款尚未实现，相关设计见 [收付款流程](docs/payment-flow.md)。

## 快速开始（首次配置）

以下命令均在 `fiber-lab` 根目录执行，首次配置步骤使用同一个 Shell。已有环境请直接看 [启动和状态检查](#5-启动和状态检查)。

`lsp-cli` 默认直接读取 offckb Node3 的配置，自动获取命令所需的信息。首次使用仍需手动生成 Biscuit 密钥并配置 LSP，再执行管理 token 签发命令；具体操作见步骤 3 和 4。

### 1. 安装依赖并编译

- Node.js 20+
- Rust 工具链
- `curl`
- `jq`

Fiber 源码默认放在同级目录 `../fiber`，必须包含 LSP 实现和 `crates/fiber-lsp-sdk`；
编译所需的系统依赖见 [Fiber README](../fiber/README.md)。

```bash
./scripts/build-fiber.sh
```

成功后，`./lsp-cli --help` 会列出 `admin`、`create-user`、`status`、`openchannel`、`closechannel` 、`listChannel`、`archive-account` 和 `list-accounts`。
更换 Fiber 源码目录的方式见 [编译与本地依赖](docs/devnet.md#编译与本地依赖)。

### 2. 初始化节点

先用 offckb 初始化 CKB devnet 和三个 Fiber 节点。offckb 会生成链与合约配置、节点 RPC 地址，并配置带资金的 CKB 出资账户，供 LSP 公共节点（Public T）等节点使用。

随后将 Node3 改为 LSP；Node1、Node2 继续由 offckb 管理，Node3 由 Shell 单独启动：

```bash
source scripts/config.sh

offckb node --daemon
offckb fiber start --nodes 3 --binary-path "$FNN_BINARY" --daemon
offckb fiber status
offckb fiber stop
```

### 3. 配置 Node3 / LSP

生成一对密钥：

```bash
biscuit keypair
```

用编辑器新建 `$LSP_DIR/biscuit-private-key`，只保存本次输出的完整 `Private key` 值：保留 `ed25519-private/` 前缀，去掉 `Private key:` 标签。然后执行：

```bash
chmod 600 "$LSP_DIR/biscuit-private-key"
```

编辑 `$LSP_DIR/config.yml`，保留已有配置，只修改以下项：

- `services`：在原列表中增加 `lsp`。
- `rpc.enabled_modules`：在原列表中增加 `lsp`、`pubsub`。
- `rpc.biscuit_public_key`：填入本次输出的完整 `Public key` 值，含 `ed25519/` 前缀，不含 `Public key:` 标签。
- `rpc.biscuit_private_key_path`：填入上面 `realpath "$LSP_DIR/biscuit-private-key"` 输出的私钥文件绝对路径。
- `fiber.announced_node_name`：设置为 `fiber-lab-lsp`。 (非必要，便于使用)

密钥与配置方式见 [Biscuit 凭证](docs/biscuit-auth.md)。

### 4. 生成管理Token

命令自动读取 Node3 配置中的 `rpc.biscuit_public_key` 和 `rpc.biscuit_private_key_path`，离线签发管理 token。默认无需传入公钥或私钥路径，也无需启动 LSP：

```bash
./lsp-cli admin create-token
```

成功后会提示凭证已保存到 `configs/keys/operator-token`。
已有凭证时直接复用，重启无需再次生成。其他参数和权限说明见 [管理凭证签发](docs/biscuit-auth.md#管理凭证签发)。

### 5. 启动和状态检查

```bash
bash scripts/start.sh
```

启动脚本会先检查 CKB RPC；已有 CKB 正常运行时直接复用，因此首次配置后只停止 Fiber、保留 CKB 运行也可以直接执行。

**Node3 配置为 LSP 后，不要再用 `--nodes 3` 或 `--fiber-nodes 3` 启动，以免覆盖其配置。后续统一使用 `scripts/start.sh`。**

状态检查：
```bash
offckb fiber status

curl --noproxy '*' -fsS http://127.0.0.1:21716 \
  -H 'Content-Type: application/json' \
  -H "Authorization: Bearer $(cat configs/keys/operator-token)" \
  --data '{"jsonrpc":"2.0","id":1,"method":"lsp_get_status","params":[]}' \
  | jq -e 'if .error then error(.error.message) else .result end'
```

`offckb fiber status` 应显示两个普通节点在运行；LSP RPC 应返回包含 `public_node_id`、`registered_tenants` 和 `active_tenants` 的结果。若 LSP 还在初始化，稍后重试 RPC；失败时查看 [故障排查](docs/troubleshooting.md)。

使用完后停止环境：

```bash
bash scripts/stop.sh
```

停止会保留已有数据，下次使用时重新执行 `bash scripts/start.sh`。

### 6. 注册账户并查询通道

```bash
# 首次注册；已有完整账户时恢复原账户
./lsp-cli create-user

# 独立查询当前账户的通道（只读）
./lsp-cli listChannel
```

`create-user` 和 `status` 默认直接从 offckb Node3 的 `config.yml` 读取 RPC 地址和 Biscuit 公钥，无需复制配置。默认路径为 `${XDG_DATA_HOME:-$HOME/.local/share}/offckb-nodejs/devnet/fiber/nodes/3/config.yml`，与管理 token 签发命令一致。

首次执行 `create-user` 时，SDK 自动生成并保存客户端身份和 `tenant_id`；客户端通过 RPC 获取 LSP 身份公钥、注册随机数和注册成功后的用户凭证。注册使用步骤 4 保存的管理 token，账户恢复后的通道查询使用已保存的用户凭证。

需要使用其他节点配置时，相关命令均可传入 `--config PATH`，相对路径以当前工作目录为准：

```bash
./lsp-cli admin create-token --config /path/to/node/config.yml
./lsp-cli create-user --config /path/to/node/config.yml
./lsp-cli status --config /path/to/node/config.yml
```

`--config` 只改变读取的配置文件，管理 token 和默认账户仍保存在项目目录。已有账户需要使用与保存记录一致的 RPC 地址和 Biscuit 公钥。

`create-user` 和 `status` 都会输出身份公钥、`tenant_id` 和通道数量。首次注册成功后，通道数量应为 `0`；再次执行时，身份公钥和 `tenant_id` 应保持一致。之后使用下节的通道命令完成链上生命周期。

`create-user` 和 `status` 运行时执行 `offckb --json accounts`，读取约定的默认出资账户 `offckb #5`。命令成功后，打印获取到的编号、lock args 和 address，便于对照排查。普通 devnet 的输出示例：

```text
默认出资账户：offckb #5
出资 lock args：0x0df9552f53d51a378aba267bc858dada447e8400
出资 address：ckt1qzda0cr08m85hc8jlnfp3zer7xulejywt49kt2rr0vthywaa50xwsqgdl92j7574rgmc4w3x00y93kk6g3lggqq23mmmd
```

lock args 和 address 使用 offckb 当前返回的值；`--config` 不改变默认选择的账户编号 `#5`。获取失败或账户缺失时，命令报错并在创建或恢复用户前停止。

原有账户仍保存在 `.state/fiber-lsp-client/default/`；新账户保存在 `.state/fiber-lsp-client/accounts/<ID>/`，默认位置与归档状态由 `accounts.json` 记录。请一起保留其中的 `root-key`、`signer.bin`、`account.json` 和开通后产生的 `channel.json`。注册、凭证和账户恢复的实现见 [客户端账户](docs/client-account.md)。

归档当前账户并创建下一轮联调账户：

```bash
./lsp-cli list-accounts
./lsp-cli archive-account
./lsp-cli create-user
./lsp-cli listChannel --account <旧账户ID>
./lsp-cli closeChannel --account <旧账户ID>
```

`<旧账户ID>` 直接使用 `list-accounts` 输出的账户 ID。`archive-account` 保留身份、密钥、凭证和通道历史，只释放默认账户位置；不关闭通道、不注销租户，也不自动创建账户。`list-accounts` 无需配置或 RPC，展示账户 ID、当前 / 归档状态、tenant_id 和本地阶段；本地阶段不代表远端或链上状态。所有客户端命令支持 `--account ID`，显式选择历史账户不会切换默认账户；未知 ID 会拒绝操作。重复归档可使用 `archive-account --account ID`。归档后尚未关闭的账户仍可协作关闭，关闭中的账户可继续恢复；原有通道归属和退款核验规则继续适用。

### 7. 开通、查询和协作关闭

```bash
./lsp-cli openchannel --funding-amount 1000
./lsp-cli listChannel
./lsp-cli closechannel
./lsp-cli listChannel
```

金额单位为 CKB，最多八位小数；首次开通必须显式提供金额。`1000` 是本方总注入额，含 99 CKB 保留金额，因此初始可用余额为 901 CKB。Node3 和客户端共用 offckb #5；默认对方另注入 99 CKB，钱包总出资为 1099 CKB 加交易费。费用上限为每笔 1 CKB，出资、commitment 和关闭费率为 1000 shannons/1000 bytes；所有条款先保存在本地，返回提案必须与之相符。

共用 #5 出资需至少两个普通 CKB live cells，余额须覆盖双方注入额和费用上限。新 devnet 的单个 genesis cell 需要先拆分；客户端会明确提示并保留原数据。开发资金准备可显式执行下列 Rust 检查：有多个 cells 时只检查，有一个大额 cell 时拆成两个同 #5 地址 cells，支付 1000 shannons 费用，等待链上确认：

```bash
cargo test --manifest-path fiber-lsp-client/Cargo.toml prepare_shared_devnet_wallet_cells -- --ignored --nocapture
```

`open-channel` / `close-channel` 是别名，`closeChannel` 也是 `closechannel` 的别名。三个通道命令均支持 `--config PATH`；开关通道支持 `--timeout SECONDS`，默认 120 秒。`closechannel --channel-id 0x...` 可指定当前账户的已绑定通道，只做协作关闭；有未结算 TLC、余额不一致或状态过期时拒绝关闭。

开通成功要求出资链上确认且达到 `ChannelReady`；关闭成功要求关闭交易确认并核对 #5 退款。`listChannel` 展示开通中、可用、关闭中、已关闭及失败状态、双方余额、交易 hash 与本地恢复阶段，不提交交易或签名。

超时、网络失败或退出后，保留账户目录全部文件，使用原配置重新执行同一命令：

```bash
./lsp-cli openchannel    # 恢复原开通；不需要再次提供金额
./lsp-cli closechannel   # 恢复原协作关闭
```

不同金额会被拒绝；重复开通返回原可用通道，重复关闭核对并返回原关闭结果。当前服务端 / SDK 限制同一租户关闭后再次开通，客户端保留旧 binding 和历史并明确提示，不自动清理或重新注册。

## 原理与实现

- [devnet 与节点管理](docs/devnet.md)：本地编译、节点分工、启动脚本、端口和运行数据。
- [Biscuit 凭证](docs/biscuit-auth.md)：公私钥、管理凭证、RPC 鉴权和用户凭证。
- [客户端账户](docs/client-account.md)：SDK 身份、注册过程、存储和恢复。
- [通道与收付款流程](docs/payment-flow.md)：已实现的通道生命周期，以及待实现的付款、收款。
- [故障排查](docs/troubleshooting.md)：启动、RPC、账户问题与出资账户核对。
