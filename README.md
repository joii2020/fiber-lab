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

成功后，`./lsp-cli --help` 会列出 `admin`、`create-user`、`status`、`open-channel`、`close-channel`、`archive-account`、`list-accounts`，以及 `pay`、`create-invoice`、`watch`、`resume`、`payment-status`、`invoice-status` 和 `list-payments`。
更换 Fiber 源码目录的方式见 [编译与本地依赖](docs/devnet.md#编译与本地依赖)。

### 2. 初始化节点

首次创建或执行完 `clean.sh` 后，在项目根目录执行：

```bash
bash scripts/create.sh
```

脚本先用本地编译的 `fnn` 初始化三个 Fiber 节点，保留 offckb 生成的链、合约、RPC 和带资金的出资账户配置；停止 Fiber 后，用 `admin init-lsp` 将 Node3 配置为 LSP、签发管理 token，再启动 Node1、Node2 和独立 LSP。

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

### 6. 账户与通道

**同一个账户（租户）无法二次创建通道（channel），即使旧通道已经关闭。**

因此，后续联调采用以下流程：**注册账户 → 准备出资 → 开通与查询通道 → 协作关闭 → 归档原账户**。需要再次开通时，注册新账户并重复上述流程；原账户及通道历史保留，可通过 `--account ID` 查询。

首次注册账户并打开通道
```bash
./lsp-cli create-user
```

Output:
```
账户 ID：account-18dc7f76b0980555
  用户注册成功，账户已保存
  身份公钥：028120e082f4f4851ead2cbcad87c2cb76e55f0ed05ce1b272089ad09a39a3b190
  tenant_id：b288c7a83e21cb5bd4f2c8890a0a0ffa0c79a43a8a8a9809092df306fecde051
  通道数量：0

默认出资账户：offckb #6
  出资 lock args：0x683188bb29502ba6e702b7c650cc80707c3f8b4b
  出资 address：ckt1qzda0cr08m85hc8jlnfp3zer7xulejywt49kt2rr0vthywaa50xwsqtgxxytk22s9wnwwq4hcegveqrs0slckjc2gkygd
```

之后，用户可以立刻打开通道（使用testnet/mainnet的时候，打开时间会比较长，这笔交易需要上链）：
```bash
./lsp-cli open-channel    # 默认出资 1000 CKB；可用 --funding-amount 指定金额
```

Output:
```
账户 ID：account-18dc7f76b0980555
未指定 --funding-amount，本次默认出资 1000.00000000 CKB
本方 #6 注入 1000.00000000 CKB，对方注入 99.00000000 CKB；通道总注入 1099.00000000 CKB，另付交易费（上限 1.00000000 CKB）
开通成功（ChannelReady，链上出资已确认）
channel ID：0x19b73440c6dfbfcaab918da3bf2a69fb480b1cad0d3c34c695eabb98907c058e
出资交易：0x0573942bd1014fc819697e353bd2defb91b73fb244ebd0bab3681cdfefce6449
实际出资费：677 shannons
```

这时候可以用 status 查看当前状态，当前可以查看通道状态。
```bash
./lsp-cli status
```

Output:
```
账户 ID：account-18dc7f76b0980555
  已恢复原账户
  身份公钥：028120e082f4f4851ead2cbcad87c2cb76e55f0ed05ce1b272089ad09a39a3b190
  tenant_id：b288c7a83e21cb5bd4f2c8890a0a0ffa0c79a43a8a8a9809092df306fecde051
  通道数量：1

默认出资账户：offckb #6
  出资 lock args：0x683188bb29502ba6e702b7c650cc80707c3f8b4b
  出资 address：ckt1qzda0cr08m85hc8jlnfp3zer7xulejywt49kt2rr0vthywaa50xwsqtgxxytk22s9wnwwq4hcegveqrs0slckjc2gkygd

通道状态：可用（链上出资已确认）
  channel ID：0x19b73440c6dfbfcaab918da3bf2a69fb480b1cad0d3c34c695eabb98907c058e
  双方通道余额：901.00000000 / 0.00000000 CKB
  出资交易：0x0573942bd1014fc819697e353bd2defb91b73fb244ebd0bab3681cdfefce6449
  本地阶段：Ready
```


当用户使用完成后，可以关闭通道：
```bash
./lsp-cli close-channel
```

Output:
```
账户 ID：account-18dc7f76b0980555
协作关闭成功（链上确认，双方退款已核对）
channel ID：0x19b73440c6dfbfcaab918da3bf2a69fb480b1cad0d3c34c695eabb98907c058e
关闭交易：0x01951f0e0b03b50c0470271759dd617f526d4dd1dacf25b2e5b1c3aee8ec663a
实际关闭费：528 shannons
```


#### 流程要点

- **账户与配置**：`create-user` 首次注册，已有完整账户时恢复原身份和凭证。命令默认读取 offckb Node3 的 `config.yml`，管理 token 使用 `configs/keys/operator-token`；可用 `--config PATH` 指定其他配置，但恢复时 RPC 地址和 Biscuit 公钥须与账户记录一致。省略 `--account ID` 时使用默认账户。
- **出资条件**：Node3 必须启用自动接受通道。本方注入额须大于 99 CKB，并达到 `fiber.open_channel_auto_accept_min_ckb_funding_amount`（默认 100 CKB；设为 `0` 时关闭自动接受）。客户端从 offckb #6 出资，Node3 / Public T 从 #5 出资；双方各自需要足额的普通 CKB live cells。#6 余额须覆盖本方注入额和交易费上限，无需承担 LSP 注入额，也无需为共用钱包拆分 cell。
- **金额与余额**：`--funding-amount` 单位为 CKB，最多八位小数；首次开通不提供时默认出资 1000 CKB，并在输出中明确提示默认金额，恢复已有开通时沿用原记录金额。无论使用默认还是指定金额，输出都会显示实际采用的双方注入额。注入 1000 CKB 含本方 99 CKB 保留金额，初始可用余额为 901 CKB；对方注入额取自 Node3 的 `fiber.auto_accept_channel_ckb_funding_amount`，默认 99 CKB，初始可用余额为 0。默认 #6 注入 1000 CKB、#5 注入 99 CKB，通道总出资为 1099 CKB，另付交易费；每笔钱包交易费用上限为 1 CKB。
- **完成条件**：开通须等出资交易链上确认并达到 `ChannelReady`；协作关闭须等关闭交易确认并分别核对双方退款。`status` 只读，不推进未完成操作；“可用（远端；链上待核对）”仅表示远端报告可用，完成核验后才显示“可用（链上出资已确认）”。开通和关闭默认最多等待 120 秒，可用 `--timeout SECONDS` 调整。
- **协作关闭与退款**：存在未结算 TLC、余额不一致或状态过期时拒绝关闭。退款按 SDK 最新已验证余额加回保留金额计算，本方承担关闭费；新通道本方退款回到 #6，对方退款回到 #5。无收付款时对应原注入额；收付款后按结算余额退款。
- **保留数据**：保留 `.state/fiber-lsp-client/accounts.json` 和完整账户目录。新账户位于 `.state/fiber-lsp-client/accounts/<ID>/`，旧账户位于 `.state/fiber-lsp-client/default/`；密钥、凭证、SDK 签名状态及 `channel.json`、`payments.json` 等记录须一起保留。配置、注册与恢复细节见 [客户端账户](docs/client-account.md)。

默认出资钱包改为 #6 只影响新开通道。已有开通记录继续使用保存的出资与退款脚本；旧版共用 #5 的通道仍按原条款恢复或关闭，不自动迁移资金或清理账户。

#### 为什么一个账户只能开通一个通道

这是当前 LSP / SDK 的实现约束：一个客户端账户对应一个租户（`tenant_id`），服务端租户记录只有一个私有通道字段 `private_channel_id`，绑定后会拒绝改绑其他通道；SDK 同时保存原通道与签名密钥的绑定，只允许重试原开通请求。关闭通道完成的是资金结算，当前实现没有解除租户绑定、重置 SDK 状态并重新开通的流程，所以旧通道关闭后仍不能为同一账户创建新通道。再次联调需要归档原账户，再注册新账户；原绑定和签名历史保留，用于恢复与核验原操作。

#### 开通或关闭中断后恢复

这里的中断指 `open-channel` 或 `close-channel` **尚未报告成功就停止**：例如等待超过 `--timeout`（默认 120 秒）、RPC 断连或请求失败、按 Ctrl+C 退出、客户端进程崩溃，或 LSP 重启导致本次调用失败。超时限制本次命令的等待时间；退出仅停止客户端继续处理，不会撤销已提交的请求、签名或链上交易。因此，命令没有返回成功，并不代表服务端未受理或交易未上链。

客户端在提交请求或签名前保存原意图、签名状态和处理进度。中断后保留完整账户目录，待连接恢复后，使用原配置、原账户重新执行对应命令；恢复会先核对服务端和链上状态，再复用原请求、交易或签名继续处理，避免重复出资或对冲突内容签名。可先运行 `status` 查看状态，但查询本身不会推进操作：

```bash
./lsp-cli status
./lsp-cli open-channel    # 开通中断时执行；恢复原开通，无需再次提供金额
./lsp-cli close-channel   # 关闭中断时执行；恢复原协作关闭
```

使用过 `--config PATH` 或 `--account ID` 时，恢复命令须带上原参数。恢复开通时若再次指定金额，必须与原金额一致；重复开通返回原可用通道，重复关闭核对并返回原关闭结果。

若停止原因是配置或身份不匹配、账户文件损坏、交易或余额校验失败，则是客户端主动拒绝继续提交或签名，以免操作错误账户或接受不符合原授权的交易。应先按报错排查原因，保留原数据；仅重复执行命令不能解决这类问题，也不要通过删除账户或通道记录绕过检查。

#### 归档账户，开始下一轮联调

完成关闭后，归档原账户并注册新账户：

```bash
./lsp-cli list-accounts
./lsp-cli archive-account
./lsp-cli create-user
./lsp-cli status --account <旧账户ID>
```

`<旧账户ID>` 取自 `list-accounts` 输出。`archive-account` 保留原身份、凭证和通道历史，只释放默认账户位置；它不会关闭通道、注销租户或自动创建新账户。`list-accounts` 只读本地记录，其本地阶段不代表远端或链上状态。通过 `--account ID` 查询或恢复历史账户不会切换默认账户；若归档时旧通道尚未关闭，仍可用 `./lsp-cli close-channel --account <旧账户ID>` 关闭或恢复原关闭。

### 7. 收付款及离线恢复

先准备普通节点与 LSP 的通道及对应方向流动性，再复用第 6 节已绑定的私有通道；命令不会自动连接节点、开通道或调整出资。默认 LSP 注入 99 CKB 时收款流动性为 0，建议客户端先付出 1 CKB，再由普通节点付回 1 CKB。

```bash
# 普通节点先生成有签名的 devnet 固定金额发票，将地址传入
./lsp-cli pay '<普通节点 invoice_address>' --max-fee 0.01 --timeout 120

# 在线收款：先输出可分享地址，然后前台签名及结算
./lsp-cli create-invoice --amount 1 --expiry 3600 --wait --timeout 120

# 离线演示：输出发票后退出，停止本账户其他推进进程，对端随后付款
./lsp-cli create-invoice --amount 1 --expiry 3600
# 在 TLC 有效恢复窗口内重新上线；不应等待对端成功后才上线
./lsp-cli resume --payment-hash 0x... --timeout 120
# 或持续推进所有本账户已授权操作
./lsp-cli watch

./lsp-cli payment-status --payment-hash 0x...
./lsp-cli invoice-status --payment-hash 0x...
./lsp-cli list-payments
```

上述命令均支持 `--account ID`、`--config PATH`。`pay` 首次必须指定费用上限，trampoline 可能用完整费用预算；恢复不能修改金额或费用。付款等待默认 120 秒。`create-invoice` 必须显式指定金额，发票有效期默认 3600 秒；不带 `--wait` 时输出后退出，带 `--wait` 时默认持续等待，`--timeout` 仅允许与 `--wait` 一起使用。等待超时或 Ctrl-C 只停止本次推进，不取消网络付款或发票；保留原 hash 和恢复资料。

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
