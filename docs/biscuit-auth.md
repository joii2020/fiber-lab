# Biscuit 凭证

本章解释 [快速开始](../README.md#3-配置-node3--lsp) 中的密钥配置、管理凭证和 RPC 鉴权。

## 密钥与凭证的关系

`lsp-cli admin init-lsp` 通过 Rust Biscuit 库生成用于 RPC 授权的一对 Ed25519 密钥。它们与 Fiber 节点身份公钥、客户端 SDK 身份不同。也可手工运行 `biscuit keypair`，按下表配置完整公私钥。

| 内容 | 存放位置 | 用途 |
| --- | --- | --- |
| Biscuit 公钥 | Node3 配置的 `rpc.biscuit_public_key` | LSP 验证 RPC 凭证的签名 |
| Biscuit 私钥 | `rpc.biscuit_private_key_path` 指向的文件 | 管理命令签发管理凭证；LSP 签发用户凭证 |
| 管理 token | `configs/keys/operator-token` | 操作者管理 LSP，并在本实验环境中执行用户注册 |
| 用户 token | 客户端 `account.json` 的会话状态 | 用户访问自己的通道等数据 |

私钥文件保存完整的 `ed25519-private/...` 字符串，不包含 `Private key:` 标签，允许末尾换行。该文件必须由当前用户拥有、仅当前用户可访问，不能是符号链接。教程使用绝对路径，避免工作目录改变后读错文件。

公私钥必须匹配：管理命令在签发前检查它们，LSP 在启动时也会检查。启用 LSP 服务后需要这对密钥，服务端才能在首次注册成功时签发用户凭证。

## 离线初始化 LSP

```bash
./lsp-cli admin init-lsp
./lsp-cli admin init-lsp --config /path/to/node/config.yml
```

命令读取 offckb 已生成的配置，保留其他配置字段，在 `services` 中增加 `lsp`，在 `rpc.enabled_modules` 中增加 `lsp`、`pubsub`，并将节点名称设为 `fiber-lab-lsp`。这两个列表和 `fiber`、`rpc` mapping 必须已存在；不创建链或节点配置。YAML 注释和排版会重新生成，配置文件权限保留。

首次初始化时，私钥默认保存到配置同目录的 `biscuit-private-key`，权限为 `600`，公钥与私钥绝对路径写入配置。已有私钥时校验并复用；重复执行不添加重复项、不轮换密钥。若配置已有公钥但不匹配，或已配置的私钥缺失、格式无效、权限不符合要求，命令拒绝修改配置；符号链接配置和私钥也拒绝使用。配置中既有的相对私钥路径按当前工作目录解析，与管理 token 命令一致，保存时改为绝对路径。

命令无需启动 LSP，不签发 token、不清理节点或客户端数据，也不输出私钥正文。`scripts/create.sh` 会串起节点初始化、本命令、管理凭证签发和启动流程。

## 管理凭证签发

在项目根目录执行：

```bash
./lsp-cli admin create-token
```

命令默认读取 `${XDG_DATA_HOME:-$HOME/.local/share}/offckb-nodejs/devnet/fiber/nodes/3/config.yml`，再读取其中 `rpc.biscuit_private_key_path` 指向的私钥。它使用 Rust 的 Biscuit 库离线构建、签名并进行 Base64 编码，无需连接 LSP。

签发的权限固定为：

```text
read("lsp");
write("lsp");
read("node");
```

这允许读写 LSP 管理接口并读取节点信息，不授予 `write("node")`，也不提供普通节点的通道、付款或连接管理权限。

默认输出到项目的 `configs/keys/operator-token`，权限为 `600`。命令拒绝覆盖已有文件或符号链接，不自动轮换凭证。成功时输出配置路径、保存路径和权限，不显示私钥或 token 正文。重启时复用原凭证。

### 可选参数

```bash
./lsp-cli admin create-token --help
./lsp-cli admin create-token \
  --config /path/to/node/config.yml \
  --private-key-file /path/to/biscuit-private-key \
  --output configs/keys/test-operator-token
```

| 参数 | 行为 |
| --- | --- |
| `--config PATH` | 覆盖默认 Node3 配置；仍需有效的 `rpc.biscuit_public_key` |
| `--private-key-file PATH` | 覆盖配置中的私钥路径；未指定时读取 `rpc.biscuit_private_key_path` |
| `--output PATH` | 覆盖默认 token 保存路径；目标仍必须不存在 |

所有显式相对路径，包括配置中的私钥路径，都相对于当前工作目录。另存的测试凭证不会自动替换客户端使用的 `configs/keys/operator-token`。

## RPC 如何验证凭证

请求通过 `Authorization: Bearer <token>` 携带凭证。LSP 用配置中的 Biscuit 公钥验证签名，再检查当前 RPC 方法要求的权限，通过后才执行接口。

例如，`lsp_get_status` 和 `lsp_list_tenants` 要求 `read("lsp")`；`lsp_get_tenant_registry_nonce` 和 `lsp_register_tenant` 要求 `write("lsp")`；`node_info` 要求 `read("node")`。

用户凭证还携带可信的 `tenant("<tenant_id>")` 身份事实。服务端据此选择用户对应的运行实例和存储空间，并限制其可调用的方法。客户端查询自己的通道时使用用户凭证，无需传入 `tenant_id`。

## 管理操作与客户端操作

`lsp-cli admin create-token` 是操作者的管理命令，显式读取服务端私钥。`create-user` 和 `status` 只读取操作所需的 token，不读取服务端私钥，也不自动签发管理凭证。

当前实验环境由联调操作者持管理凭证执行首次注册；注册成功后，服务端返回用户凭证，客户端保存它并用于后续用户操作。面向普通用户的注册服务尚未实现。注册步骤与恢复限制见 [客户端账户](client-account.md)。

更完整的鉴权规则见同级 Fiber 源码中的 [Biscuit 文档](../../fiber/docs/biscuit-auth.md)。
