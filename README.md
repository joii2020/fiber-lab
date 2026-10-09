# fiber-lab

本地 Fiber LSP 联调环境，使用 `lsp-cli` 创建、恢复客户端账户，开通、查询和协作关闭 CKB 私有通道，以及收付款和离线恢复。

按下文可以跑完当前已实现的流程：**准备环境 → 生成管理凭证 → 启动 LSP → 注册账户 → 开通道 → 查询通道 → 协作关闭 → 停止或重启**。客户端已实现收付款及前台恢复命令；实际 devnet 收付款与离线场景尚待验证，完整流程及限制见 [收付款流程](docs/payment-flow.md)。

## 快速开始（首次配置）

以下命令均在 `fiber-lab` 根目录执行。已有环境请直接看 [启动和状态检查](#5-启动和状态检查)。

`lsp-cli` 默认直接读取 offckb Node3 的配置，自动获取命令所需的信息。编译完成后，执行 `bash scripts/create.sh` 即可初始化 devnet、配置 LSP、生成管理 token 并启动节点，无需手工编辑 YAML 或运行 `biscuit keypair`。

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

成功后，`./lsp-cli --help` 会列出 `admin`、`accounts`、`status`、`create-channel`、`close-channel`，以及 `pay`、`create-invoice`、`watch`、`resume`、`payment-status`、`invoice-status` 和 `list-payments`。
更换 Fiber 源码目录的方式见 [编译与本地依赖](docs/devnet.md#编译与本地依赖)。

### 2. 初始化节点

首次创建或执行完 `clean.sh` 后，在项目根目录执行：

```bash
bash scripts/create.sh
```

脚本先用本地编译的 `fnn` 初始化三个 Fiber 节点，保留 offckb 生成的链、合约、RPC 和带资金的出资账户配置；停止 Fiber 后，用 `admin init-lsp` 将 Node3 配置为 LSP、用 `admin init-accounts` 离线初始化四用户、签发管理 token，再启动 Node1、Node2 和独立 LSP。

`create.sh` 在已有 Node3 目录或管理 token 时提前退出，不覆盖已有环境，也不执行清理。它用于首次创建；已有环境的启动和状态检查见步骤 5。创建完成后可直接执行步骤 5 的状态检查，以及步骤 6 的账户操作。

### 3. 配置 Node3 / LSP

`create.sh` 已自动执行此步骤。若已有 offckb 生成的普通 Node3，需要单独配置为 LSP，先停止 Fiber，然后执行以下离线命令：

```bash
./lsp-cli admin init-lsp
```

命令保留其他配置字段，只修改以下项；YAML 注释和排版会重新生成：

- `services`：在原列表中增加 `lsp`。
- `rpc.enabled_modules`：在原列表中增加 `lsp`、`pubsub`。
- `rpc.biscuit_public_key`：写入自动生成或复用的 Biscuit 公钥。
- `rpc.biscuit_private_key_path`：写入私钥文件绝对路径；新私钥默认位于配置同目录的 `biscuit-private-key`，权限为 `600`。
- `fiber.announced_node_name`：设置为 `fiber-lab-lsp`。

可用 `--config PATH` 指定其他 offckb 节点配置。重复执行不会添加重复项或轮换密钥；已有私钥会复用，公私钥不匹配、已配置的私钥缺失或私钥权限不符合要求时拒绝修改。命令不会启动节点或签发管理 token。密钥与配置方式见 [Biscuit 凭证](docs/biscuit-auth.md)。

### 4. 生成管理Token

`create.sh` 已自动执行此步骤。单独配置 LSP 后，首次生成管理 token 时执行以下命令；已有凭证无需重复签发。

命令自动读取 Node3 配置中的 `rpc.biscuit_public_key` 和 `rpc.biscuit_private_key_path`，离线签发管理 token。默认无需传入公钥或私钥路径，也无需启动 LSP：

```bash
./lsp-cli admin create-token
```

成功后会提示凭证已保存到 `configs/keys/operator-token`。
已有凭证时直接复用，重启无需再次生成。其他参数和权限说明见 [管理凭证签发](docs/biscuit-auth.md#管理凭证签发)。

### 5. 启动和状态检查

`create.sh` 已启动节点，创建后直接执行下方状态检查。已有环境停止后，再次使用时执行：

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

### 6. 四用户与通道

`admin init-lsp` 只初始化 LSP。已有环境升级时，单独执行离线用户初始化（不要求节点在线或管理 token 可用）：

```bash
./lsp-cli admin init-accounts
./lsp-cli accounts
./lsp-cli accounts --history
./lsp-cli status -u 1
```

用户 1～4 永久绑定 offckb #6～#9；LSP / Public T 使用 #5。初始化创建四份独立 SDK 身份，尚不注册租户。重复初始化复用身份、钱包、当前轮次及历史，已关闭的空闲用户不会自动创建下一轮。旧 v1 账户保留原目录和密钥，迁移后标为“旧账户未绑定”，不会自动归给用户 1。

注册与开通合并为一个命令：

```bash
./lsp-cli create-channel -u 1 --funding-amount 1000
./lsp-cli status -u 1
./lsp-cli close-channel -u 1
./lsp-cli accounts --history
./lsp-cli create-channel -u 1
```

只有新开通意图默认出资 1000 CKB；恢复时省略金额会沿用原金额，显式指定须一致。开通须等待链上出资确认及 ChannelReady，关闭须等待链上关闭确认并核对双方最新结算余额退款。成功关闭后自动归档本轮，保留原身份、租户及收付款历史；下一次显式开通创建新身份和不同 tenant_id，仍归原用户及钱包。同一租户绑定过通道后无法再开新通道。

双方各有 99 CKB 保留金额。客户端钱包需覆盖本方出资及费用上限，LSP #5 需覆盖其注入额。默认用户注入 1000、LSP 注入 99 CKB 时，用户初始可用余额 901 CKB，LSP 收款方向可用余额为 0；每笔钱包交易费上限 1 CKB。Node3 须启用自动接受且达到其出资阈值。钱包绑定用于 inputs、签名、找零和退款，旧通道始终按保存条款恢复。

开关命令默认等待 120 秒，`--timeout SECONDS` 只停止本次等待，不撤销已提交请求、签名或交易。中断后执行原用户和配置的同一命令，不要删除数据：

```bash
./lsp-cli create-channel -u 1       # 恢复原开通，无需再填金额
./lsp-cli close-channel -u 1        # 恢复关闭；资金已关闭但归档失败时也重试此命令
./lsp-cli status --account <旧账户ID>
./lsp-cli close-channel --account <旧账户ID>
```

日常操作必须显式选择 `-u N`，没有默认用户。`--account ID` 与用户参数互斥，仅允许 status、close-channel、resume、payment-status、invoice-status、list-payments 的历史入口，不切换用户当前轮次。用户当前轮次必须通过 `-u N` 关闭；历史入口不能绕过用户锁。无通道关闭不归档，未结算 TLC、退款校验失败或链上未确认也不归档。同用户命令互斥，新开通另有全局锁，四用户链上开通串行。

保留 `.state/fiber-lsp-client/accounts.json` 及完整账户目录，包含 root-key、signer.bin、account.json、channel.json、payments.json；归档不移动或删除这些文件，也不注销服务端租户。清单结构损坏拒绝继续；单账户摘要异常在列表单独显示。详情见 [客户端账户](docs/client-account.md)。

### 7. 收付款及离线恢复

当前多用户开关及退款已通过真实 devnet 验证；完整收付款验收尚未通过。实际在线付款在 TLC 移除时缺少提前 preimage 证据，随后服务端 peer ACK 超时强关；必要前置见 [验证记录与草案](plans/draft-payment-resolution-prerequisite.md)。以下为已实现命令及目标流程。

先准备普通节点与 LSP 的通道及对应方向流动性，再复用第 6 节已绑定的私有通道；命令不会自动连接节点、开通道或调整出资。默认 LSP 注入 99 CKB 时收款流动性为 0，建议客户端先付出 1 CKB，再由普通节点付回 1 CKB。

```bash
# 普通节点先生成有签名的 devnet 固定金额发票，将地址传入
./lsp-cli pay '<普通节点 invoice_address>' --max-fee 0.01 --timeout 120 -u 1

# 在线收款：先输出可分享地址，然后前台签名及结算
./lsp-cli create-invoice --amount 1 --expiry 3600 --wait --timeout 120 -u 1

# 离线演示：输出发票后退出，停止本账户其他推进进程，对端随后付款
./lsp-cli create-invoice --amount 1 --expiry 3600 -u 1
# 在 TLC 有效恢复窗口内重新上线；不应等待对端成功后才上线
./lsp-cli resume --payment-hash 0x... --timeout 120 -u 1
# 或持续推进所有本账户已授权操作
./lsp-cli watch -u 1

./lsp-cli payment-status --payment-hash 0x... -u 1
./lsp-cli invoice-status --payment-hash 0x... -u 1
./lsp-cli list-payments -u 1
```

所有命令支持 `--config PATH`；仅 resume、payment-status、invoice-status、list-payments 支持互斥 `--account ID` 历史入口。`pay` 首次必须指定费用上限，trampoline 可能用完整费用预算；恢复不能修改金额或费用。付款等待默认 120 秒。`create-invoice` 必须显式指定金额，发票有效期默认 3600 秒；不带 `--wait` 时输出后退出，带 `--wait` 时默认持续等待，`--timeout` 仅允许与 `--wait` 一起使用。等待超时或 Ctrl-C 只停止本次推进，不取消网络付款或发票；保留原 hash 和恢复资料。

`Success` / `Paid` 是远端证据；只有 SDK 双 commitment lane 的 TLC 移除及余额核对完成才报告付款 / 收款完成。发送响应丢失时保留“结果未知”，恢复只查原 hash，不自动再次发送。收款秘密在本地保存，仅通过 SDK 完整恢复证明、链上 live funding 和恢复窗口检查后释放。查询不签名、释放秘密或发送付款；普通用户查询可能唤醒租户，不能查询管理侧 delivery 或保证看到 `Deferred`。

CLI 退出只是客户端停止签名，不保证 LSP 私有通道离线，也不承诺长期离线期间最终到账。LSP 暂存付款需要 LSP 持续运行；真实通道离线通过显式管理操作构造。账户锁使同账户一次仅能运行一个命令，`watch` / `--wait` 在线时需先停止它再查询。实际离线、超时及重启场景的验收状态见 [方案索引](PLAN.md)。

### 8. 停止和重启

使用完后停止环境：

```bash
bash scripts/stop.sh
```

停止会保留已有数据，可重复执行；PID 文件缺失或记录的 LSP 进程已退出时也能正常停止。下次使用时重新执行 `bash scripts/start.sh`。

需要重建整个系统时执行：

```bash
bash scripts/clean.sh
```

清理脚本先执行 `stop.sh`，成功后删除 offckb 的整个 CKB devnet 和全部 Fiber 节点（含 Node3 / LSP 的配置与密钥）、项目 `.state/` 下的所有数据，以及 `configs/keys/operator-token`。若停止失败，清理中止。删除后执行 `bash scripts/create.sh` 自动重建并启动整个环境。

## 原理与实现

- [devnet 与节点管理](docs/devnet.md)：本地编译、节点分工、启动脚本、端口和运行数据。
- [Biscuit 凭证](docs/biscuit-auth.md)：公私钥、管理凭证、RPC 鉴权和用户凭证。
- [客户端账户](docs/client-account.md)：SDK 身份、注册过程、存储和恢复。
- [通道与收付款流程](docs/payment-flow.md)：已实现的通道生命周期，以及收付款、前台推进与离线恢复。
- [故障排查](docs/troubleshooting.md)：启动、RPC、账户问题与出资账户核对。
