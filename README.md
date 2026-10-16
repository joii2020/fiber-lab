# fiber-lab

本地 Fiber LSP 联调环境，使用 `lsp-cli` 创建、恢复客户端账户，开通、查询和协作关闭 CKB 私有通道，以及双方在线的单笔收付款。

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

成功后，`./lsp-cli --help` 会列出 `admin`、`accounts`、`status`、`create-channel`、`close-channel`，以及 `pay` 和 `create-invoice`。
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

日常操作必须显式选择固定账户 `-u N`。status、close-channel 可加 `--round R` 选择该账户的历史轮次；轮次从 1 开始，按清单内创建顺序编号，包括当前轮次，只有已归档轮次允许通过历史入口操作。历史选择不切换当前轮次，并持有同一账户锁。当前轮次直接用 `-u N`；pay、create-invoice、create-channel 不允许选择历史轮次。兼容旧数据的 `--account ID` 入口仍保留，与 `-u N`、`--round R` 互斥。无通道关闭不归档，未结算 TLC、退款校验失败或链上未确认也不归档。同账户命令互斥，新开通另有全局锁，四账户链上开通串行。

历史直接保留在 `.state/fiber-lsp-client/` 下，通过清单关联固定账户与各轮内部身份；沿用已有目录，无需迁移。保留 `accounts.json` 及完整身份目录，包含 root-key、signer.bin、account.json、channel.json、payments.json；归档不移动或删除这些文件，也不注销服务端租户。清单结构损坏拒绝继续；单账户摘要异常在列表单独显示。详情见 [客户端账户](docs/client-account.md)。

### 7. 双方在线的 1v1 收付款

当前只实现经典在线场景：A 创建一张发票并保持运行，B 支付该发票，双方持续在线直到各自报告完成。每个账户一次只推进本次操作，不批量处理历史收付款。`watch`、`resume`、`payment-status`、`invoice-status`、`list-payments` 已移除。

双方需各有一条到 LSP 的私有通道。付款方本方可用余额须覆盖金额及费用，收款方 LSP 一侧可用余额须覆盖发票金额。命令不会自动开通道或补充资金。

```bash
# 先串行准备通道并核对余额；已有通道可以复用
./lsp-cli create-channel -u 1
./lsp-cli create-channel -u 2
./lsp-cli status -u 1
./lsp-cli status -u 2

# 终端 A：输出 invoice_address 后保持运行，等待这一笔收款完成
./lsp-cli create-invoice -u 1 --amount 1 --expiry 3600

# 终端 B：复制上面的完整地址并付款，保持运行直到完成
./lsp-cli pay -u 2 '<invoice_address>' --timeout 120
```

`create-invoice` 必须指定金额，发票有效期默认 3600 秒；创建后直接等待收款，不再使用 `--wait`。可用 `--timeout 120` 限制等待时间，默认一直等待。`pay` 使用 Fiber 默认费用预算（当前为金额的 0.5%，向上取整到 shannon），不提供费用参数，默认等待 120 秒；trampoline 可能花费完整费用预算。两条命令仅接受 `-u N` 当前轮次，并支持 `--config PATH`，不提供 `--account` 和 `--round` 参数。

快速测试：`./scripts/test_send_and_pay.sh` 使用已开通通道的账户 1 收款、账户 2 付款，金额 1 CKB、使用 Fiber 默认费用预算（0.005 CKB）；结束后输出双方日志和耗时。脚本不会清理或重建环境。

收付款期间每轮等待 500ms 后继续查询、签名和结算。SDK 校验本地授权及 commitment，收款在完整恢复签名、链上 live funding 和安全窗口检查通过后释放 preimage；只有远端 `Paid` / `Success`、SDK 双 lane TLC 移除和余额核对完成后才报告成功。

付款依赖同级 Fiber 的 `get_payment` 在 TLC 移除签名前提供可核验 preimage。需要使用包含该修改的 Fiber 节点；已启动的节点不会因重新编译自动升级。

收付款业务状态、preimage 和待提交签名只保存在本次进程内存中，不读写 `payments.json`，不额外保存账户 session；旧付款文件保留。账户、通道及 SDK 底层签名存储继续沿用。超时、Ctrl-C 或 RPC 错误会以失败退出，不自动重试或重发；退出不取消网络付款，也不能恢复本次内存状态。远端已有同一 hash 的付款时拒绝再次发送。

2026-10-16 沿用现有通道验证：账户 2 向账户 1 支付 1 CKB，双方均完成结算与余额核对，旧 `payments.json` 内容哈希保持不变。付款 hash：`0xeb57b09fb6c2c6b80f5a83161d82db0d630d6d34a8dbb7e5b17e7d1b1f81dc1a`。

收付款命令可加 `--timings` 查看分阶段耗时；“等待付款”与“收款处理耗时”分别统计，处理过程中的协议等待仍计入后者。`--timings-json` 提供机器可读计时，两者互斥。

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

也可执行 `bash scripts/reset-all.sh` 一次完成清理、重新编译、创建并启动环境，然后为账户 1、2 开通道。此命令会删除上述全部运行数据；启动后每秒检查一次带管理 token 的 `lsp_get_status`，确认 LSP RPC 返回有效节点身份后才开通道，最多等待 120 秒。等待超时或任一步骤失败会退出；LSP 启动日志位于 `.state/devnet/lsp.log`。

## 原理与实现

- [devnet 与节点管理](docs/devnet.md)：本地编译、节点分工、启动脚本、端口和运行数据。
- [Biscuit 凭证](docs/biscuit-auth.md)：公私钥、管理凭证、RPC 鉴权和用户凭证。
- [客户端账户](docs/client-account.md)：SDK 身份、注册过程、存储和恢复。
- [通道与收付款流程](docs/payment-flow.md)：已实现的通道生命周期，以及双方在线的单笔收付款。
- [故障排查](docs/troubleshooting.md)：启动、RPC、账户问题与出资账户核对。
