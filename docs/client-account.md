# 客户端账户

本章解释 [注册账户并查询通道](../README.md#6-注册账户并查询通道) 使用的配置、SDK 身份和恢复流程。

## 配置与命令

当前客户端支持一个默认账户，不需要额外账户参数：

```bash
./lsp-cli create-user
./lsp-cli status
./lsp-cli list-channel
./lsp-cli open-channel --funding-amount 1000
./lsp-cli close-channel
```

客户端默认直接从 `${XDG_DATA_HOME:-$HOME/.local/share}/offckb-nodejs/devnet/fiber/nodes/3/config.yml` 读取 `rpc.listening_addr` 和 `rpc.biscuit_public_key`，与管理命令使用同一个 Node3 配置。管理 token 位于项目的 `configs/keys/operator-token`。

需要使用其他配置时，所有客户端命令都可传入 `--config PATH`，相对路径以当前工作目录为准：

```bash
./lsp-cli create-user --config /path/to/node/config.yml
./lsp-cli status --config /path/to/node/config.yml
```

`--config` 只改变读取的配置文件；管理 token 和默认账户仍保存在项目目录。

`create-user` 首次创建身份并注册，已有完整账户时恢复原账户。`status` 恢复账户，使用已保存的用户凭证调用 `list_channels`，输出身份公钥、`tenant_id` 和通道数量。账户不存在时，`status` 提示先执行 `create-user`。

直接执行 `./lsp-cli` 或使用 `-h` / `--help` 可查看命令说明，无需连接 LSP。

## 首次注册

客户端通过本地 `fiber-lsp-sdk` 管理身份和签名，通过自己的 HTTP 传输调用 LSP RPC。注册顺序是：

1. 使用 SDK 的 `RootSigner::create_random` 创建身份，保存根密钥备份、SDK 存储及初始账户记录。
2. 使用管理凭证调用 `lsp_get_status`，保存目标 LSP 的 Fiber 身份公钥。
3. 调用 `lsp_list_tenants`，确认服务端尚未注册此身份。
4. 调用 `lsp_get_tenant_registry_nonce`，由 SDK 检查随机数并通过 `begin_registration` 生成注册参数。
5. 先保存“等待注册结果”状态，再调用 `lsp_register_tenant`。
6. 核对返回的身份和用户记录，通过 SDK 的 `finish_registration` 保存用户凭证及会话状态。
7. 使用用户凭证查询通道数量。

注册调用使用管理凭证，通道查询使用用户凭证。初次注册不会创建支付通道，正常通道数量为 `0`。

## 本地存储

旧账户目录为 `.state/fiber-lsp-client/default/`，新账户使用 `.state/fiber-lsp-client/accounts/<ID>/`；清单 `.state/fiber-lsp-client/accounts.json` 记录默认账户和归档状态。每个账户目录包含：

| 文件 | 内容 |
| --- | --- |
| `root-key` | 32 字节 SDK 根密钥备份 |
| `signer.bin` | SDK 签名存储的持久化快照 |
| `account.json` | 身份公钥、`tenant_id`、关联 LSP、注册进度、用户记录及会话状态 |
| `channel.json` | 开通后创建；可信网络和钱包条款、原始请求、冻结 / 签名交易、hash、处理阶段、关闭授权及待重发签名 |

这些文件需要一起保留，才能恢复同一身份及其签名状态。账户目录权限为 `700`，账户文件权限为 `600`；目录锁防止多个进程同时修改同一账户。写入时保存完整快照，SDK 存储写入失败后停止继续操作。

## 账户恢复与中断处理

再次执行 `create-user` 或 `status` 时，客户端导入原根密钥并打开原 SDK 存储，检查账户身份、`tenant_id`、用户凭证和关联 LSP 的一致性。

已完成注册的账户直接复用用户凭证，不需要重新生成管理 token 或重复注册。服务重启后，只要原节点、配置和账户数据仍在，就可以继续查询。

账户绑定保存时的 RPC 地址和 Biscuit 公钥。当前配置与记录不一致时，客户端拒绝将原账户用于另一套服务；未完成的注册还会核对目标 LSP 的 Fiber 身份公钥。原先关联 Node4 的账户不会自动迁移到 Node3。

注册中断后再次执行 `create-user`，客户端先核对服务端记录。只有确认用户尚不存在时，才继续注册。若服务端已经注册但本地没有保存用户凭证，命令会停止并保留原身份与进度：当前 RPC 不提供凭证找回接口，重复注册也不会重新签发用户 token。

账户文件缺失、损坏或彼此不匹配时，客户端保留原文件并报错，不重新生成身份。具体排查见 [故障排查](troubleshooting.md#客户端账户问题)，SDK 接口说明见 [fiber-lsp-sdk README](../../fiber/crates/fiber-lsp-sdk/README.md)。

## 通道意图与恢复

通道命令读取同一账户，整个操作期间持有账户锁。SDK signer 和 session、原开通请求、签名交易及关闭条款会在相应外部操作前持久化；写入失败时停止。旧版 `account.json` 仍使用格式版本 1，无需迁移、注册或生成新根密钥。

开通时从 Node3 的 CKB RPC、合约部署、链 spec 和 genesis 获取可信参数，保存 genesis hash、双方总注入额、默认 lock dep、费率及钱包费用上限。钱包在 Rust 进程中读取 #5 私钥并验证其公钥哈希，捕获 offckb 的 stdout / stderr；不打印、保存或传送私钥。通道 MuSig2 密钥仍来自 SDK，与出资钱包密钥独立。

`open-channel` 恢复时先查租户所有通道及 pending / failed 记录，签名前解析全部链上 inputs，核对通道输出、找零、费用和合约。出资提交前查链：`unknown` 可在仍等待外部出资时重发已保存的相同交易；`pending`、`proposed`、`committed` 不重复出资，`rejected` 停止。签名响应丢失后按当前 request ID 与完整请求复用本地签名，SDK 历史防止冲突签名。

`close-channel` 从本地已验证的两条 commitment 核对余额和 TLC，独立计算关闭大小及客户端承担的费用，在请求前保存授权。恢复先查询状态：已在关闭中时处理原签名，已关闭时独立核对链上退款。原账户、binding、交易和失败记录均保留。

阶段包括 `Opening`、`FundingSaved`、`FundingSubmitted`、`Ready`、`CloseRequested`、`Closing`、`Closed`。超时或失败打印当前阶段和恢复命令；`list-channel` 只读查询，不代替恢复命令推进操作。具体用法见 [通道生命周期](../README.md#7-开通查询和协作关闭)。

## 账户归档与历史账户

`./lsp-cli list-accounts` 只读展示本地账户 ID、当前 / 归档状态、tenant_id 及本地恢复阶段，不调用 RPC，不打印密钥或 token。旧 default 目录可以直接识别，无需移动文件；记录不完整时列表显示待恢复。

`./lsp-cli archive-account` 归档当前账户并释放默认位置，保留所有原文件、SDK 安全状态和通道记录，不关闭通道或注销服务端租户。之后显式执行 `create-user` 才创建新身份。再次归档相同账户可使用 `archive-account --account ID`。

`create-user`、`status`、`list-channel`、`open-channel`、`close-channel`均支持 `--account ID`。ID 直接取自 `list-accounts`，指定历史账户不会改变默认选择；`create-user --account ID` 只恢复已有身份。归档但尚未关闭的通道使用 `close-channel --account ID` 关闭，关闭中的操作也通过同一命令恢复。当前服务端仍限制同一租户关闭后再次开通。

清单与账户分别持锁，按清单锁 → 账户锁顺序取得，网络操作期间只持有账户锁；并发归档或操作同一账户时明确拒绝并提示重试，不移动或清理目录。未知 ID、损坏清单或缺失目录拒绝操作并保留数据。
