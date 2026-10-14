# fiber-lab

本地 Fiber LSP 联调环境，使用 `lsp-cli` 创建、恢复客户端账户，开通、查询和协作关闭 CKB 私有通道，以及收付款和离线恢复。

按下文可以跑完当前已实现的流程：**准备环境 → 生成管理凭证 → 启动 LSP → 注册账户 → 开通道 → 查询通道 → 协作关闭 → 停止或重启**。账户间在线收付款已通过当前本地 devnet 验证；离线与故障恢复仍需分别验证，完整流程及限制见 [收付款流程](docs/payment-flow.md)。

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
- `fiber.auto_accept_channel_ckb_funding_amount`：缺失或为 `null` 时设置为 `100000000000` shannons（1000 CKB），已有显式值（包括 0）保持不变。

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

固定账户 1～4 永久绑定 offckb #6～#9，通过 `-u N` 操作；LSP / Public T 使用 #5。初始化创建四份独立 SDK 身份，尚不注册租户。每个账户内部按轮次保留身份、租户和历史，`accounts` 展示固定账户及本地状态，不显示当前轮次，`accounts --history` 展示各账户历轮；日常操作无需使用内部 `account-…` ID。重复初始化复用身份、钱包、当前轮次及历史，已关闭的空闲账户不会自动创建下一轮。旧 v1 身份保留原目录和密钥，迁移后标为“旧身份未绑定”，不会自动归给账户 1。

`accounts` 和 `accounts --history` 将仅创建本地身份、尚未注册 LSP 的账户本地状态显示为 `unregistered`。

注册与开通合并为一个命令：

```bash
./lsp-cli create-channel -u 1 --funding-amount 1000
./lsp-cli status -u 1
./lsp-cli close-channel -u 1
./lsp-cli accounts --history
./lsp-cli create-channel -u 1
```

只有新开通意图默认出资 1000 CKB；恢复时省略金额会沿用原金额，显式指定须一致。开通须等待链上出资确认及 ChannelReady，关闭须等待链上关闭确认并核对双方最新结算余额退款。成功关闭后自动归档本轮，保留原身份、租户及收付款历史；下一次显式开通创建新身份和不同 tenant_id，仍归原用户及钱包。同一租户绑定过通道后无法再开新通道。

默认双方各有 100 CKB 保留金额（99 CKB 占用容量加 1 CKB 关闭手续费）。客户端钱包需覆盖本方出资及费用上限，LSP #5 需覆盖其注入额。新初始化环境默认用户和 LSP 各注入 1000 CKB，双方初始可用余额各 900 CKB；每笔钱包交易费上限 1 CKB。Node3 须启用自动接受且达到其出资阈值。钱包绑定用于 inputs、签名、找零和退款，旧通道始终按保存条款恢复。未执行新版初始化且配置缺省时，客户端与 fnn 仍按 100 CKB 处理，LSP 一侧可用余额为 0。已有环境可停止 Fiber 后执行 `./lsp-cli admin init-lsp` 再启动；显式出资配置不会覆盖，现有通道也不会补充资金。

旧版客户端可能保存了 99 CKB 的开通条款，而服务端返回 100 CKB；升级不会改写已冻结的条款或交易。此类失败不能靠重复 `create-channel` 修复。未提交的开通交易会占用 LSP 出资 cell，直到服务端开通超时释放，期间其他账户也可能因资金不足失败。需要保留数据时不要执行 `clean.sh`；仅在确认可丢弃整套 devnet 数据后，才重新 `clean.sh`、`create.sh`。

开关命令默认等待 120 秒，`--timeout SECONDS` 只停止本次等待，不撤销已提交请求、签名或交易。中断后执行原用户和配置的同一命令，不要删除数据：

```bash
./lsp-cli create-channel -u 1       # 恢复原开通，无需再填金额
./lsp-cli close-channel -u 1        # 恢复关闭；资金已关闭但归档失败时也重试此命令
./lsp-cli status -u 1 --round 1    # 查询账户 1 的历史第 1 轮
./lsp-cli close-channel -u 1 --round 1  # 核对历史关闭结果
```

日常操作必须显式选择固定账户 `-u N`。status、close-channel、resume、payment-status、invoice-status、list-payments 可加 `--round R` 选择该账户的历史轮次；轮次从 1 开始，按清单内创建顺序编号，包括当前轮次，只有已归档轮次允许通过历史入口操作。历史选择不切换当前轮次，并持有同一账户锁。当前轮次直接用 `-u N`；pay、create-invoice、watch、create-channel 不允许选择历史轮次。兼容旧数据的 `--account ID` 入口仍保留，与 `-u N`、`--round R` 互斥。无通道关闭不归档，未结算 TLC、退款校验失败或链上未确认也不归档。同账户命令互斥，新开通另有全局锁，四账户链上开通串行。

历史直接保留在 `.state/fiber-lsp-client/` 下，通过清单关联固定账户与各轮内部身份；沿用已有目录，无需迁移。保留 `accounts.json` 及完整身份目录，包含 root-key、signer.bin、account.json、channel.json、payments.json；归档不移动或删除这些文件，也不注销服务端租户。清单结构损坏拒绝继续；单账户摘要异常在列表单独显示。详情见 [客户端账户](docs/client-account.md)。

### 7. 收付款及离线恢复

当前多用户开关、退款及账户间在线收付款已通过真实 devnet 验证。在线付款要求同级 Fiber 的 `get_payment` 在 TLC 移除签名前返回已校验、已持久化的本租户 preimage；本次验证使用工作区已有的该修改。旧版本可能在 TLC 移除时拒签并随后 ACK 超时强关；离线、重启及故障恢复仍需单独验证。

前台等待改用 LSP 的 `wait_channel_update` HTTP 长轮询：`create-invoice --wait`、`pay`、`watch` 和 `resume` 在状态变化时立即继续签名及结算，不再每轮固定等待 500ms。没有变化时服务端挂起请求，最多 5 秒返回一次以便补查；这不是每步等待 5 秒。退出命令后不保留后台监听，账户锁和原有恢复方式保持不变。不带 `--wait` 的 `create-invoice` 仍在输出发票后退出。

需要同时重新编译 Fiber 节点和 CLI，并在节点使用新二进制启动后使用等待命令。旧节点不支持新接口时会明确报错，保留原 hash 与恢复资料，不自动重发付款。已启动的节点不会因编译自动升级；无需清理已有运行数据。实际支付耗时需分别测量余额更新及双方结算完成，不承诺固定毫秒数。

2026-10-14 使用新版 Release 节点和 CLI，沿用现有通道完成账户 2 → 账户 1 的 1 CKB devnet 支付：从启动付款 CLI 计时，收款 CLI 完成（远端状态、SDK TLC 移除及余额核对）约 691ms，付款 CLI 完成约 970ms；付款 hash 为 `0x491d4b05a49cf68a80a8fb813ff6da93b43323bbf8f8e3b1e793e23723a94025`。这是一次端到端实测，包含 CLI 启动、外部签名、持久化和结算核对，不是 Fiber 协议的独立延迟基准。

账户之间互付只需双方各有一条到 LSP 的私有通道，并有对应方向的可用余额；付款方本方余额覆盖金额及费用，收款方 LSP 一侧余额覆盖发票金额。新初始化默认双方各注入 1000 CKB。与普通节点互付时，另外准备普通节点到 LSP 的通道和对应流动性。命令不会自动连接节点、开通道或补充现有通道资金。

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


账户间在线收付款使用两个终端。先串行开通双方通道并查询余额，然后让收款终端保持运行：

```bash
./lsp-cli create-channel -u 3
./lsp-cli create-channel -u 4
./lsp-cli status -u 3
./lsp-cli status -u 4

# 终端 A：输出 invoice_address 后保持运行
./lsp-cli create-invoice -u 3 --amount 1 --expiry 3600 --wait
# 终端 B：复制上面的完整地址，由账户 4 支付
./lsp-cli pay -u 4 '<invoice_address>' --max-fee 0.01 --timeout 120
```

收款端 Ctrl-C 后需在另一个终端 `resume -u 3 --payment-hash 0x...` 或 `watch -u 3`，付款端也需保持推进；只恢复付款端不能替收款端签名和结算。

`pay`、`create-invoice --wait`、`resume`、`watch` 会显示远端状态、通道可用余额，以及 LSP 暂存、转发、结算、成功或失败的进度和原因。投递接口每 5 秒查询，状态变化时提示，无变化时每 15 秒提示仍在等待；超时或中断显示最后核对的进度及恢复命令。`Deferred` 和最近错误表示仍可重试，不能等同最终付款失败；`SettlingUpstream` 表示上游 TLC 尚在结算。最终付款失败仍需核对 TLC 清理和余额。收款端遇到最终投递失败停止本次等待，但不取消发票，不推断其他付款结果。付款端若投递已失败而付款 RPC 仍为 `Inflight`，明确停止本次等待并提示上游结果、TLC 清理和余额待核对；不把一次投递失败当作整笔付款终态，也不据此授权取消。

`invoice-status`、`payment-status` 同样展示最新投递进度。用户凭证调用 `get_payment_delivery`，只能查看自己作为收款人或由其绑定通道付出的 delivery；不读取管理 token，不暴露对端租户、通道或原像。无可见记录不能证明没有付款到达。旧节点不支持接口时明确提示未核对，不把查询错误当作付款失败。原发票、付款 hash、授权及运行数据继续保留；不要因超时重复支付或清理环境。

所有命令支持 `--config PATH`；仅 resume、payment-status、invoice-status、list-payments 支持互斥 `--account ID` 历史入口。`pay` 首次必须指定费用上限，trampoline 可能用完整费用预算；恢复不能修改金额或费用。付款等待默认 120 秒。`create-invoice` 必须显式指定金额，发票有效期默认 3600 秒；不带 `--wait` 时输出后退出，带 `--wait` 时默认持续等待，`--timeout` 仅允许与 `--wait` 一起使用。等待超时或 Ctrl-C 只停止本次推进，不取消网络付款或发票；保留原 hash 和恢复资料。

`Success` / `Paid` 是远端证据；只有 SDK 双 commitment lane 的 TLC 移除及余额核对完成才报告付款 / 收款完成。发送响应丢失时保留“结果未知”，恢复只查原 hash，不自动再次发送。收款秘密在本地保存，仅通过 SDK 完整恢复证明、链上 live funding 和恢复窗口检查后释放。查询不签名、释放秘密或发送付款；普通用户的发票和付款查询可能唤醒租户；投递进度查询本身不唤醒租户，且仅返回本人参与的投递摘要。

CLI 退出只是客户端停止签名，不保证 LSP 私有通道离线，也不承诺长期离线期间最终到账。LSP 暂存付款需要 LSP 持续运行；真实通道离线通过显式管理操作构造。账户锁使同账户一次仅能运行一个命令，`watch` / `--wait` 在线时需先停止它再查询。实际离线、超时及重启场景尚未通过验收，历史记录可通过 Git 查询。

本次验证（2026-10-14）：保留已有 U1、U2 及其原付款记录，在 U3、U4 各开一条双方注入 1000 CKB 的新通道，初始双方可用余额各 900 CKB。U4→U3、U3→U4 分别支付 1 CKB，费用均为 0.01 CKB；双方均报告收付款完成，远端为 `Paid` / `Success`，SDK 双 lane TLC 移除及余额完成核对，最终 U3、U4 本方可用余额均为 899.99 CKB。付款 hash 分别为 `0xee73aee1ea90c8fe8d66de9d4e3a8938a70139f9a407f92351da680c5e3a22a7`、`0xe3e12566ee2019245d116d876dfee9a4d08bb574c0be56cbe12d8acea1568496`。

原 U1、U2 的余额不足付款现在可从双方用户接口看到 `Insufficient balance` 和最终投递失败原因；原付款 RPC 仍为 `Inflight`，U2 通道已被服务端非协作关闭，不能继续链下恢复，TLC 清理及链上结算未完成核对，资料继续保留。它不属于本次双向成功验证，也不能仅凭 `accounts` 的 `ready` 认定已退款。

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
