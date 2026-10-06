# fiber-lab

本地 Fiber LSP 联调环境与 **Fiber LSP 联调工具**：offckb 使用本地编译的 `fnn` 启动 CKB devnet 和普通 Fiber 节点，Shell 单独启动 LSP；`lsp-cli` 提供管理操作与客户端操作，客户端通过 LSP RPC 注册用户，使用本地 `fiber-lsp-sdk` 管理身份和签名。

**当前可用：启动 devnet / LSP、离线签发管理 token、创建和恢复客户端账户、查询通道数量。客户端开通道、付款和收款尚未实现。** 下文先给出可执行的启动步骤，再说明客户端完成一笔付款和收款需要的最短流程。

## 1. 启动 Fiber LSP devnet

依赖：**offckb 0.5.0**、Node.js 20+、Rust 工具链，以及 `curl`、`jq`。

```bash
npm i -g @offckb/cli@0.5.0-canary-ee0ad6b
offckb --version
```
(目前 offckb 0.5.0 还处于 canary)

Fiber 源码默认位于同级目录 `../fiber`，必须包含本项目使用的 LSP 实现和 `crates/fiber-lsp-sdk`。offckb 默认下载的普通 FNN 不能替代这里本地编译的 LSP。Fiber 的编译依赖见其 [README](../fiber/README.md)。以下命令都在 `fiber-lab` 根目录执行。

### 编译

脚本依次编译 Fiber 的 `fnn` 和 `fiber-lsp-client`。编译完成后，项目根目录会有 `./lsp-cli` 入口，方便直接进行管理与客户端联调。

```bash
bash scripts/build-fiber.sh
```

- `./lsp-cli` 这里的软链只是便于使用。
- lsp-cli 关于 `fiber-lsp-sdk` 和 `fiber-json-types`  的依赖使用本地路径。
- 若更换 Fiber 源码目录，需要同步修改 `scripts/config.sh` 的 `FNN_BINARY` 和 `fiber-lsp-client/Cargo.toml` 的两个依赖路径，再重新编译。

### 首次初始化三个 Fiber 节点

本节使用 **Node1、Node2 作为普通节点，Node3 作为 LSP**。
先由 offckb 启动三个普通节点，自动生成当前 devnet 的链、合约、RPC、端口、CKB 私钥和密码，再停止 Fiber，将 Node3 改成 LSP。
（CKB 在配置期间继续运行，不需要手动填合约哈希或导入 CKB 私钥。）

以下命令用于首次初始化。在同一个 Shell 中执行，并保持工作目录为项目根目录：

```bash
source scripts/config.sh

# 启动 CKB devnet；首次运行会下载 CKB 并创建链数据
offckb node --daemon

# 生成并启动三个普通 Fiber 节点
offckb fiber start --nodes 3 --binary-path "$FNN_BINARY" --daemon
offckb fiber status

# 停止三个 Fiber 节点，保留配置、密钥和数据；CKB 继续运行
offckb fiber stop
```

### 将 Node3 配置为 LSP

编辑 `$LSP_DIR/config.yml`，修改以下项：

1. 在 `services` 中增加 `lsp`。
2. 在 `rpc.enabled_modules` 中增加 `lsp`、`pubsub`。
3. 生成一对 Biscuit 密钥，并将公钥填入 `rpc.biscuit_public_key`。在项目根目录执行：

   如需安装 Biscuit，执行 `cargo install biscuit-cli`。

```bash
   biscuit keypair
```

   将输出的 `Public key` 完整值（含 `ed25519/` 前缀）填入配置。

4. 将同一次输出的 `Private key` 完整值保存到 `$LSP_DIR/biscuit-private-key`，即 Node3 数据目录下的 `nodes/3/biscuit-private-key`。获取其绝对路径，填入 `rpc.biscuit_private_key_path`：

   ```bash
   chmod 600 "$LSP_DIR/biscuit-private-key"
   realpath "$LSP_DIR/biscuit-private-key"
   ```

   将 `realpath` 的输出填入配置。私钥文件包含完整的 `ed25519-private/<私钥>` 字符串，不包含 `Private key:` 标签。

5. 将 `fiber.announced_node_name` 设置为 `fiber-lab-lsp`。 （便于后续调试）

### 启动前两个普通节点，再单独启动 Node3

完成首次配置后，用启动脚本启动 CKB、两个普通节点和 Node3 / LSP：

```bash
bash scripts/start.sh
```

脚本使用 `offckb fiber start --nodes 2` 管理普通节点，再用 `nohup` 单独启动 Node3，复用其目录中的配置、密码和密钥，并将 PID 保存到 `.state/devnet/lsp.pid`。脚本要求首次配置已完成，不生成或复制 LSP 密钥。

`--nodes 2` 会将 Node3 移出 `fiber/nodes.yml` 的管理列表，保留其目录，且只重新生成前两个节点的配置。**配置为 LSP 后，不要再用 `--nodes 3` 或 `--fiber-nodes 3` 启动它**，否则 offckb 会覆盖 Node3 的配置并将它作为普通节点启动。后续重启跳过首次初始化步骤，直接使用启动脚本。

| 服务 | RPC | Fiber P2P | CKB 账户 |
| --- | --- | --- | --- |
| CKB devnet | `http://127.0.0.1:8114` | — | — |
| 普通 Fiber Node1 | `http://127.0.0.1:21714` | `8344` | `#3` |
| 普通 Fiber Node2 | `http://127.0.0.1:21715` | `8345` | `#4` |
| LSP / Node3 | `http://127.0.0.1:21716` | `8346` | `#5` |


### 生成 RPC 管理凭证

在项目根目录执行管理命令，用配置 Node3 时保存的私钥离线签发管理 token：

```bash
./lsp-cli admin create-token
```

该命令直接读取 `${XDG_DATA_HOME:-$HOME/.local/share}/offckb-nodejs/devnet/fiber/nodes/3/config.yml`，以及其中 `rpc.biscuit_private_key_path` 指向的私钥文件；无需启动 LSP、`source scripts/config.sh` 或提前复制 `configs/lsp.yml`，也不要求配置 RPC 地址。私钥必须由当前用户拥有、仅当前用户可访问，保存完整 `ed25519-private/...` 字符串（允许末尾换行）；签发前会检查公私钥是否匹配。

默认保存到项目内 `configs/keys/operator-token`，文件权限为 `600`。已有文件或符号链接目标会被拒绝，不覆盖、不自动轮换。成功输出实际配置路径、保存路径和权限，不显示私钥或 token 正文。固定权限为：

```text
read("lsp");
write("lsp");
read("node");
```

这允许读写 LSP、读取节点信息，不授予 `write("node")`。密钥和 token 的生成只在首次配置时执行，重启复用原文件。公钥、私钥和管理 token 必须匹配；接口说明见 Fiber 的 [Biscuit 文档](../fiber/docs/biscuit-auth.md)。

三个参数均为可选覆盖项，用于联调其他配置或保存独立的测试凭证：

```bash
./lsp-cli admin create-token --help
./lsp-cli admin create-token \
  --config /path/to/node/config.yml \
  --private-key-file /path/to/biscuit-private-key \
  --output configs/keys/test-operator-token
```

`--config` 覆盖默认 Node3 配置，缺失或无效时直接报错；仍需其中的 `rpc.biscuit_public_key`。`--private-key-file` 覆盖配置内私钥路径；未指定且配置字段缺失时，需补齐字段或指定参数。`--output` 覆盖默认保存位置。所有显式相对路径（包括配置内的私钥路径）均相对于当前工作目录；教程使用私钥绝对路径。

<details>
<summary>原理：通过 biscuit CLI 签发的等价操作</summary>

以下权限文件和 Shell 命令与管理命令签发的权限相同。两种方式选择一种执行，仅在尚未创建凭证时使用；Shell 重定向会覆盖已有文件。此示例沿用初始化 Shell 中的 `$LSP_DIR`：

```bash
mkdir -p .state configs/keys
cat > .state/operator-permissions.bc <<'EOF'
read("lsp");
write("lsp");
read("node");
EOF

(umask 077; biscuit generate \
  --private-key "$(cat "$LSP_DIR/biscuit-private-key")" \
  .state/operator-permissions.bc > configs/keys/operator-token)
```

管理命令在 Rust 中使用相同的 Biscuit 权限构造和 Base64 编码，避免在主流程中通过 Shell 命令行传递私钥。`biscuit-cli` 仍用于上文的 `biscuit keypair` 密钥生成。

</details>

### 确认就绪

```bash
# 确认 CKB 和 offckb 管理的前两个节点
offckb fiber status

# 确认单独启动的 LSP；必须携带管理凭证
curl --noproxy '*' -fsS http://127.0.0.1:21716 \
  -H 'Content-Type: application/json' \
  -H "Authorization: Bearer $(cat configs/keys/operator-token)" \
  --data '{"jsonrpc":"2.0","id":1,"method":"lsp_get_status","params":[]}' \
  | jq -e 'if .error then error(.error.message) else .result end'

# 核对实际出资账户
curl --noproxy '*' -fsS http://127.0.0.1:21716 \
  -H 'Content-Type: application/json' \
  -H "Authorization: Bearer $(cat configs/keys/operator-token)" \
  --data '{"jsonrpc":"2.0","id":1,"method":"node_info","params":[]}' \
  | jq -e 'if .error then error(.error.message) else .result | {pubkey, default_funding_lock_script} end'
```

`lsp_get_status` 成功结果包含 `public_node_id`、`registered_tenants` 和 `active_tenants`。将 `default_funding_lock_script` 与 `offckb accounts` 的账户 `#5` 对照；其 `args` 应为 `0x0df9552f53d51a378aba267bc858dada447e8400`。`pubkey` 是 Public T 的 Fiber 身份公钥。

启动脚本写入 PID 不代表 LSP 已经就绪；连接失败时查看 `.state/devnet/lsp.log`，待初始化完成后重试 RPC。启动服务不会自动注册客户端、连接普通节点与 LSP，或建立支付通道。

### 查看日志、停止和重启

```bash
tail -f .state/devnet/lsp.log

# 停止手动启动的 LSP，再停止普通 Fiber 节点和 CKB；保留数据
bash scripts/stop.sh
```

重启时，在项目根目录执行 `bash scripts/start.sh`，不再执行初始化三个普通节点的步骤。`stop.sh` 根据保存的 PID 停止 `nohup` 启动的 LSP，删除 PID 文件，再停止 offckb 管理的普通 Fiber 节点和 CKB。

CKB 和节点数据位于 `${XDG_DATA_HOME:-$HOME/.local/share}/offckb-nodejs/devnet/`；LSP 使用其中的 `fiber/nodes/3/`。停止和重启保留这些数据，密码与加密后的 CKB 密钥需要一起保留。

已有 Node4 数据保留，不自动迁移或清理。项目客户端使用同步后的 `configs/lsp.yml` 连接 Node3；原先关联 Node4 的客户端账户也不自动迁移。

### 首次启动失败时

- offckb 不识别 Fiber 命令或参数：核对安装版本以及 `offckb fiber start --help`。
- 找不到 `fnn` 或 `fiber-lsp-sdk`，或 LSP 服务不可用：检查同级 Fiber 源码、编译结果和 `FNN_BINARY` 路径。
- 首次下载 CKB 失败：查看 offckb 输出，确认下载源可访问。
- 端口已占用或节点已经运行：用 `offckb fiber status` 查看现有服务，先停止对应服务再启动。
- LSP 退出或 RPC 拒绝访问：查看 `.state/devnet/lsp.log`，核对 Node3 的密码、Biscuit 密钥对、私钥绝对路径及管理凭证。

合约和链配置由首次运行的 offckb 自动生成；排查时保留已有数据，不要用清理 devnet 来替代配置检查。

## 2. 使用 Fiber LSP 联调工具

`lsp-cli admin create-token` 是 LSP 操作者的管理命令；`create-user` 和 `status` 是客户端命令。管理命令显式读取服务端私钥，客户端命令只读取所需的管理 token 或用户凭证，不读取服务端私钥，也不自动签发管理 token。

确认 LSP RPC 就绪后，在项目根目录同步 LSP 配置，供客户端读取 RPC 地址和 Biscuit 公钥。以下命令沿用启动步骤中设置的 `$LSP_DIR`：

```bash
cp "$LSP_DIR/config.yml" configs/lsp.yml
```

然后执行以下命令，测试账户创建和通道查询：

```bash
# 查看命令说明
./lsp-cli --help

# 创建或恢复客户端账户
./lsp-cli create-user

# 查询当前账户的通道数量
./lsp-cli status
```

直接执行 `./lsp-cli` 或使用 `-h` / `--help` 都会显示用法、命令和选项说明，无需连接 LSP。

`create-user` 首次生成身份并注册到 LSP，已有完整账户时恢复原账户；`status` 恢复账户并用用户凭证查询自己的通道。两个命令都输出身份公钥、`tenant_id` 和通道数量。刚注册时通道数量为 `0`，还需要开通道才能收付款。

客户端自动读取 `configs/lsp.yml` 的 RPC 地址和 Biscuit 公钥，注册时读取 `configs/keys/operator-token`，后续使用已保存的用户凭证。首次注册使用管理凭证是当前实验环境的接入方式：由联调操作者执行注册，服务端返回用户凭证，用户后续操作使用该凭证。两者职责不同，面向普通用户的注册服务不在本期范围。重启时复用管理 token 和完整账户中的用户凭证。

账户保存在 `.state/fiber-lsp-client/default/`，当前只支持一个账户，无需额外配置参数。

## 3. 客户端最快完成一笔付款和收款

**本节是待实现的客户端使用流程，下面的 `open-channel`、`pay`、`create-invoice`、`watch` 命令当前还不能执行。** 现有客户端命令只有 `create-user` 和 `status`，还不能完成端到端支付。

最短联调路径是：

```text
普通 Fiber 节点 ⇄ LSP ⇄ 客户端用户的私有通道
```

先准备普通节点与 LSP 之间的可用通道，再为客户端开一条到 LSP 的私有通道。付款需要客户端一侧有可用余额，收款需要 LSP 一侧有可用余额；两个方向都要扣除通道保留金额和所需费用。第一次演示建议固定支付 **1 CKB**，即 `100000000` shannons，RPC 十六进制金额为 `0x5f5e100`。

### 一次准备，复用同一条客户端通道

```bash
./lsp-cli create-user     # 已实现；已有账户会直接恢复
./lsp-cli open-channel    # 待实现；出资、签名并等待 ChannelReady
```

`open-channel` 应自动使用已保存的 LSP 公钥、项目中的 CKB RPC 和合约配置，以及约定的 devnet 出资账户。开通道时完成链上出资，后续付款和收款复用这条通道。

### 付款：拿到对端发票，执行一个命令

对端普通节点通过 `new_invoice` 创建 devnet 发票（`currency: "Fibd"`），将 `invoice_address` 交给客户端：

```bash
./lsp-cli pay '<对端的 invoice_address>'   # 待实现
```

`pay` 应在内部完成发票检查、本地付款授权、调用 `send_payment`、处理 SDK 签名请求和查询 `get_payment`。命令等待结果后退出，以 `status: "Success"` 确认付款完成。

### 收款：创建发票，保持客户端在线

```bash
./lsp-cli create-invoice   # 待实现；建议默认创建 1 CKB 发票并输出 invoice_address
./lsp-cli watch            # 待实现；持续处理签名和收款结算
```

将输出的 `invoice_address` 交给对端普通节点，由对端调用 `send_payment`。客户端在 `watch` 中处理入账签名和结算，查询 `get_invoice`，以 `status: "Paid"` 确认收款完成。`Received` 表示已收到但尚未结算。

### 客户端还需要补齐的操作

| 命令 | 必须完成的工作 |
| --- | --- |
| `open-channel` | 通过 SDK 的 `HostedSession::open_tenant_channel` 调用 `open_tenant_channel` RPC；核对并保存开通道结果，使用 CKB 钱包签名出资交易，调用 `submit_signed_funding_tx`，处理通道签名直到 `ChannelReady` |
| `pay` | 保存 SDK 付款授权与付款进度，调用 `send_payment`，持续核验并提交通道签名，查询最终付款状态 |
| `create-invoice` | 本地生成并保存 preimage 和 SDK 收款授权，通过 `new_invoice` 创建只提供 `payment_hash` 的 hold invoice，保存并输出发票 |
| `watch` | 查询 `get_channel_signing_status`，经 SDK 核验后调用 `submit_channel_signature`；入账满足 SDK 的 preimage 释放检查后调用 `settle_invoice`，持续查询最终收款状态 |

这些操作使用注册返回的 **用户凭证** 访问 LSP RPC，用户身份由凭证确定。签名和收款秘密保留在客户端，操作进度及时落盘；命令中断后先核对原操作，避免重复出资或付款。

客户端实现范围见 [PLAN.md](PLAN.md)，SDK 接口以同级 Fiber 源码中的 [fiber-lsp-sdk README](../fiber/crates/fiber-lsp-sdk/README.md) 为准。

## 运行数据

`configs/lsp.yml` 和 `configs/keys/` 保存本地配置与凭证，已加入 Git 忽略规则；按上述步骤生成或同步。

停止和重启保留链、节点和客户端账户数据，不自动清理。客户端账户中的 `root-key`、`signer.bin` 和 `account.json` 需要一起保留，以恢复同一个身份及其签名状态。
