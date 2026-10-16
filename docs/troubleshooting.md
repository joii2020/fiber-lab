# 故障排查

按 [快速开始](../README.md) 操作遇到问题时，先核对配置、凭证和服务状态，保留已有运行数据。

## 启动失败或 RPC 未就绪

查看普通节点状态和 LSP 日志：

```bash
offckb fiber status
tail -f .state/devnet/lsp.log
```

`offckb fiber status` 检查 offckb 管理的普通节点，单独启动的 LSP 需要用 README 中的 `lsp_get_status` 请求确认。PID 文件存在不代表 LSP RPC 已经就绪；连接失败时查看日志，待初始化完成后重试。

| 现象 | 检查方式 |
| --- | --- |
| offckb 不识别 Fiber 命令或参数 | 核对 `offckb --version` 和 `offckb fiber start --help`，使用 README 指定的 canary 版本 |
| 找不到 `fnn` 或 `fiber-lsp-sdk`，或 LSP 服务不可用 | 检查同级 Fiber 源码、编译结果和 `scripts/config.sh` 的 `FNN_BINARY` 路径 |
| 首次下载 CKB 失败 | 查看 offckb 输出，确认下载源可访问 |
| 端口占用或节点已经运行 | 查看现有服务，停止对应服务后再启动，避免重复运行启动脚本 |
| LSP 退出或 RPC 拒绝访问 | 查看日志，核对 Node3 密码、Biscuit 密钥对、私钥绝对路径、私钥权限及管理凭证 |
| 管理 token 目标已经存在 | 复用原凭证；需要独立测试凭证时用 `--output` 指定新路径 |

合约和链配置由首次运行的 offckb 生成。排查时不要用清理 devnet 替代配置检查，也不要重新用 `--nodes 3` 启动已配置为 LSP 的 Node3。

## 核对 LSP 出资账户

在项目根目录执行：

```bash
curl --noproxy '*' -fsS http://127.0.0.1:21716 \
  -H 'Content-Type: application/json' \
  -H "Authorization: Bearer $(cat configs/keys/operator-token)" \
  --data '{"jsonrpc":"2.0","id":1,"method":"node_info","params":[]}' \
  | jq -e 'if .error then error(.error.message) else .result | {pubkey, default_funding_lock_script} end'

offckb accounts
```

将 `default_funding_lock_script` 与 offckb 的账户 `#5` 对照。本项目默认 Node3 账户的 `args` 应为 `0x0df9552f53d51a378aba267bc858dada447e8400`。`pubkey` 是 LSP 公共节点（Public T）的 Fiber 身份公钥，与 Biscuit 公钥不同。

## 客户端账户问题

| 现象 | 处理方式 |
| --- | --- |
| 无法读取 LSP 配置 | 核对 offckb Node3 的 `config.yml` 及 `XDG_DATA_HOME` / `HOME`；使用其他配置时传入 `--config PATH` |
| 固定用户尚未初始化 | 先执行 `./lsp-cli admin init-accounts`；随后 `create-channel -u N` |
| 注册尚未完成 | 再次执行 `./lsp-cli create-channel -u N`，让客户端先核对服务端记录 |
| 服务端已有用户，但本地缺少用户凭证 | 保留原账户与进度；当前 RPC 无法找回凭证，重复注册也不会重新签发 |
| RPC 地址或 Biscuit 公钥与保存的账户不一致 | 检查是否连接原来的 LSP；原账户不能直接用于另一套服务 |
| 账户文件缺失、损坏或不匹配 | 核对并恢复同一份备份中的 `root-key`、`signer.bin`、`account.json`，保留原文件 |
| 账户正在被其他进程使用 | 等待正在使用该账户的命令结束后重试 |

账户恢复规则见 [客户端账户](client-account.md)，节点和凭证的保存位置见 [运行数据](devnet.md#运行数据)。

2026-10-09 真实收付款验证发现：客户端 get_payment 尚为 Inflight 且无 preimage 时，移除 TLC 签名被 SDK 拒绝；服务端一般 peer ACK 超时还可能强关通道。此时保留原 hash、账户目录与用户指针，查询实际链上状态，不将远端 Closed 当作已核验协作退款或通过归档/新付款绕过。当前在线付款要求 Fiber 在 TLC 移除签名前通过 get_payment 提供可核验 preimage；请使用包含该修改的节点版本。历史诊断可通过 Git 查询。
