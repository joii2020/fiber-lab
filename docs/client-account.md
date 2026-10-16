# 客户端用户、账户与恢复

固定账户 1～4 永久使用 offckb #6～#9，CLI 通过 `-u N` 操作，LSP / Public T 使用 #5。账户编号与钱包不随通道轮次变化；SDK 身份和出资钱包是独立密钥。每轮内部身份及其 tenant_id 永久归属创建它的账户。当前 SDK/服务端仍限制每租户一条通道，因此新轮次继续创建独立身份和租户，固定账户作为长期操作和历史管理单位。

```bash
./lsp-cli admin init-lsp
./lsp-cli admin init-accounts
./lsp-cli accounts
./lsp-cli create-channel -u 1 --funding-amount 1000
./lsp-cli status -u 1
./lsp-cli close-channel -u 1
./lsp-cli accounts --history
./lsp-cli status -u 1 --round 1
./lsp-cli create-channel -u 1
```

`init-lsp` 只配置 LSP 和 Biscuit 密钥。`init-accounts` 离线读取已初始化配置及 offckb 公开钱包，创建四份独立本地 SDK 身份，不连接 RPC、不注册、不读取管理 token、不更改 YAML 或 LSP 密钥。重复初始化保留当前/空闲状态及历史。初始化中断会保留已写入绑定、预留 ID 和身份；重试补全完成标记，若 SDK 文件不完整则拒绝重新生成密钥。

默认配置为 `${XDG_DATA_HOME:-$HOME/.local/share}/offckb-nodejs/devnet/fiber/nodes/3/config.yml`，可用 `--config PATH` 指定。恢复核对保存的 RPC 地址、Biscuit 公钥、身份、tenant_id 及用户归属；不允许重绑另一服务或转移租户。

`create-channel -u N` 在同一账户锁下注册并开通。已注册身份恢复不再读取管理 token；新注册使用 `configs/keys/operator-token`。注册请求发送前落盘等待标记；响应丢失先核对原租户，服务端存在而本地凭证缺失时明确停止，不盲目另注册。新意图默认 1000 CKB，恢复省略金额沿用原值。

开通核对链、原条款、inputs、出资、找零和费率，出资签名仅签对应用户钱包的 inputs。提交前查链，已知交易不重复提交；签名响应丢失复用相同请求和本地签名，SDK 拒绝冲突签名。

`close-channel -u N` 继续使用 SDK 双 lane 最新结算余额，阻止未结算 TLC；链上关闭确认和双方退款核验完成后才归档。无通道不归档。归档只更新清单，保留目录、密钥和恢复资料；写清单失败时原关闭记录保留，重试关闭核验原结果后补归档。成功归档后用户空闲，重复关闭通过最近关闭账户核对原结果。下一次显式开通分配新身份和租户，旧租户始终属于原用户。归档不注销租户、不释放服务端名额。

`accounts` 离线只读展示四用户和本地状态，不要求配置、节点或 offckb 可用；`--history` 显示历轮和旧未绑定账户。本地 Ready 不代表实时可用，`status -u N` 再核对远端及链上；初始化未注册时 status 展示本地状态而不注册，空闲时读取最近关闭摘要。查询不写回清单或 SDK。

所有日常命令必须选择固定账户 `-u N`。status、close-channel 支持 `-u N --round R` 历史入口；pay、create-invoice、create-channel 只处理当前轮次。轮次按清单内该账户身份的创建顺序从 1 编号，包括当前轮次；只有已归档轮次允许用 `--round`，当前轮次直接用 `-u N`。历史查询使用该轮原凭证和 signer，不切换当前指针、不跨账户查 hash；历史归档重试不影响已创建的新轮次。兼容旧数据的 `--account ID` 入口仍保留，与 `-u N`、`--round R` 互斥，旧未绑定身份继续用此入口。

`accounts` 展示固定账户、钱包、tenant_id、本地状态和历史数，不展示内部时间戳 ID 或当前轮次。`accounts --history` 展示各账户轮次及状态；旧未绑定身份仍展示 ID 以便恢复。历史直接存储于 `.state/fiber-lsp-client/`，清单关联账户与历轮，沿用原身份目录和 v2 格式，无需搬迁已有数据。

锁顺序为用户锁 → 短清单锁 → 账户锁，清单锁不跨 RPC。关闭核验后释放账户锁，保持用户锁，再按清单锁 → 账户锁归档。锁非阻塞，竞争明确报错；新开通另持全局开通锁，避免共享 LSP #5 cell 冲突。

v1 查询保留原格式和数据，不自动迁移；只有 init-accounts 升级为 v2，旧账户的 user 为 null，原 current 仅作为 legacy_current 展示。新账户目录是 `.state/fiber-lsp-client/accounts/<ID>/`，旧 default 目录不移动。root-key、signer.bin、account.json 和 channel.json 必须一起保留，私密文件为 600。旧 payments.json 保留，当前收付款流程不再读取或写入该文件。

双方在线的收付款流程见 [付款流程](payment-flow.md)。
