# 2 lsp-cli 开通道与关通道

审核：已确认；依据本次开发者要求执行 2.1.1，按既定需求及技术路线实施，范围不变；实现细节在 2.1.1 记录。
授权：已授权；本次开发者明确要求执行 2.1.1，覆盖阶段一至四的客户端实现、验证与文档。
进度：待验收

## 规划方式

拆分子方案。开通道、必要签名、协作关闭和恢复共同构成一次完整的通道生命周期交付，技术要点由一个二级方案管理；原有四个实施阶段保留在一个三级方案内，不按步骤继续拆文件。

- 本文件保留一级编号 `2`，维护产品需求、范围、业务规则、产品验收及整体进度。
- [2.1 通道生命周期技术要点](2.1-lsp-cli-channel-lifecycle.md) 承接原“当前依据”和技术路线，管理关键决策及技术交付。
- [2.1.1 通道生命周期详细方案](2.1.1-lsp-cli-channel-lifecycle.md) 承接原“实施阶段”和具体验证方式，管理详细设计、实施及恢复点。

整理原因是原文件混合了三个层级的职责。一级编号及原草案记录保留，本次执行要求已覆盖实施授权；开发者本次确认通道查询改由独立的 `listChannel` 子命令提供，相关下层草案同步调整。本次开发者要求执行 2.1.1 后按既定范围落实需求、技术设计和实现，详细结果见三级记录。

## 背景和总体目标

`lsp-cli` 逐步发展为服务 Fiber LSP 探索与教程的联调工具，明确管理操作与客户端操作的职责，保持已有账户、节点配置和运行数据可复用。客户端现已复用账户注册、恢复和通道查询能力，实现完整开关通道流程。

本方案让已有客户端账户完成 devnet CKB 私有通道的链上出资、启用和协作关闭，通过独立的 `listChannel` 查询通道，并在命令中断后核对、恢复同一次操作。整个流程复用账户身份、节点配置和运行数据。

## 产品需求与范围

首版仅支持 devnet CKB 私有通道，使用原客户端账户和 offckb 出资账户 #5，包含协作关闭。强制关闭、链上争议结算、UDT、支付、收款、独立 `watch`、多账户管理、服务诊断和自动配置不加入本次范围。

以下使用流程已实现并完成在线生命周期验证：

```bash
./lsp-cli create-user
./lsp-cli openchannel --funding-amount 1000
./lsp-cli listChannel
./lsp-cli closechannel
```

TODO Output:
```
joii@dev-debian:~/code/fiber-dev/fiber-lab$ ./lsp-cli create-user
用户注册成功，账户已保存
身份公钥：0304c56e1ad9d2e472594028dc2661695a0487cb4be643fd679a0072799fe4edee
tenant_id：761d0892cf493e9de802b927f3bab1730aa8188768f255e0ed4bbbaf2a9d10bb
通道数量：0
默认出资账户：offckb #5
出资 lock args：0x0df9552f53d51a378aba267bc858dada447e8400
出资 address：ckt1qzda0cr08m85hc8jlnfp3zer7xulejywt49kt2rr0vthywaa50xwsqgdl92j7574rgmc4w3x00y93kk6g3lggqq23mmmd
joii@dev-debian:~/code/fiber-dev/fiber-lab$ ./lsp-cli listChannel
tenant_id：761d0892cf493e9de802b927f3bab1730aa8188768f255e0ed4bbbaf2a9d10bb
当前账户通道列表为空
joii@dev-debian:~/code/fiber-dev/fiber-lab$ ./lsp-cli openchannel --funding-amount 1000
本方注入 1000.00000000 CKB，对方注入 99.00000000 CKB；共用 #5 总注入 1099.00000000 CKB，另付交易费（上限 1.00000000 CKB）
开通成功（ChannelReady，链上出资已确认）
channel ID：0xa8eb9cbd0a2b2fd6fc28d2fdfd957fc4c2d3c33493f0a90ddb20bcc2cdc1f2f8
出资交易：0x478c7ab5155e47eabd08b8f6a0721afc69bf84285d4319a4dc6b45725470e155
实际出资费：625 shannons
joii@dev-debian:~/code/fiber-dev/fiber-lab$ ./lsp-cli listChannel
tenant_id：761d0892cf493e9de802b927f3bab1730aa8188768f255e0ed4bbbaf2a9d10bb
channel ID：0xa8eb9cbd0a2b2fd6fc28d2fdfd957fc4c2d3c33493f0a90ddb20bcc2cdc1f2f8
状态：可用（链上出资已确认）
双方通道余额：901.00000000 / 0.00000000 CKB
出资交易：0x478c7ab5155e47eabd08b8f6a0721afc69bf84285d4319a4dc6b45725470e155
本地阶段：Ready
joii@dev-debian:~/code/fiber-dev/fiber-lab$ ./lsp-cli listChannel
tenant_id：761d0892cf493e9de802b927f3bab1730aa8188768f255e0ed4bbbaf2a9d10bb
channel ID：0xa8eb9cbd0a2b2fd6fc28d2fdfd957fc4c2d3c33493f0a90ddb20bcc2cdc1f2f8
状态：可用（链上出资已确认）
双方通道余额：901.00000000 / 0.00000000 CKB
出资交易：0x478c7ab5155e47eabd08b8f6a0721afc69bf84285d4319a4dc6b45725470e155
本地阶段：Ready
joii@dev-debian:~/code/fiber-dev/fiber-lab$ ./lsp-cli closechannel
协作关闭成功（链上确认，#5 退款已核对）
channel ID：0xa8eb9cbd0a2b2fd6fc28d2fdfd957fc4c2d3c33493f0a90ddb20bcc2cdc1f2f8
关闭交易：0x07015bd2a4a5c12931002980b5cf543af7b174906be56639056f44dc65a5b621
实际关闭费：528 shannons
joii@dev-debian:~/code/fiber-dev/fiber-lab$ ./lsp-cli listChannel
tenant_id：761d0892cf493e9de802b927f3bab1730aa8188768f255e0ed4bbbaf2a9d10bb
channel ID：0xa8eb9cbd0a2b2fd6fc28d2fdfd957fc4c2d3c33493f0a90ddb20bcc2cdc1f2f8
状态：已关闭（链上退款已核对）
双方通道余额：901.00000000 / 0.00000000 CKB
出资交易：0x478c7ab5155e47eabd08b8f6a0721afc69bf84285d4319a4dc6b45725470e155
关闭交易：0x07015bd2a4a5c12931002980b5cf543af7b174906be56639056f44dc65a5b621
本地阶段：Closed
再次开通受当前服务端 / SDK 的同租户单通道记录限制；旧记录保留
```

### 命令与配置

- 正式名称为 `openchannel` / `closechannel`，提供 `open-channel` / `close-channel` 别名，兼容已有设计文档中的命名。
- 新增独立子命令 `listChannel`，按此大小写使用；本方案的通道查询由该命令完成，`status` 不承担本需求。
- `openchannel`、`closechannel` 和 `listChannel` 支持 `--config PATH`，默认读取 offckb Node3 配置。
- `openchannel` / `closechannel` 提供 `--timeout`，默认等待 120 秒；达到最终状态后退出，超时或失败返回非零退出码，并输出当前阶段和恢复命令。

### 开通道行为

- 首次开通道必须提供 `--funding-amount`，单位 CKB，最多八位小数。示例 1000 CKB 不作为隐式出资默认值。
- 已有未完成开通道时，不传金额可恢复；传入不同金额或其他不一致参数时报错，保留原记录。
- 已有可用通道时显示原通道，不再次出资。开通成功表示链上出资已确认且通道可用，不能仅以请求受理或交易提交作为成功。

### 关闭与查询行为

- `closechannel` 默认选择当前账户唯一活动通道，可用 `--channel-id` 显式指定并核对所属账户；没有活动通道时给出明确结果，已关闭通道可查询原关闭结果。
- 开通未完成、状态过期或存在未结算 TLC 时拒绝开始协作关闭，并说明原因。
- 协作关闭退款到开通时绑定的 #5 脚本；只有关闭交易链上确认且退款与本地条款一致才报告完成。
- 重复关闭继续原操作或返回原关闭结果；关闭后保留账户、通道密钥、交易和恢复历史。
- 当前本地服务端及 SDK 的单通道记录限制同一租户关闭后直接再次开通；命令需明确展示限制，不清除旧记录或自动创建新账户规避，保留旧通道查询结果。
- `listChannel` 查询当前账户的通道列表，展示通道 ID、状态、双方余额、出资 / 关闭交易标识和本地待恢复阶段，明确区分可用、开通中、关闭中、已关闭和失败；没有通道时明确展示空列表结果。
- `listChannel` 只查询和展示结果，不推进开关通道或提交签名。

### 中断与失败行为

命令等待期间处理本操作所需签名。网络超时、响应丢失或进程退出后，重新执行先核对原操作，再继续同一意图；不重复出资、不生成冲突签名、不改动原账户身份。失败和超时保留数据，不输出私钥或 token。

## 约束与依赖

- 复用现有账户注册与恢复能力、本地 `../fiber` 的 SDK / JSON 类型 / LSP RPC，以及 offckb devnet 和 #5 出资账户。接口事实与技术依赖见 [2.1 的当前事实](2.1-lsp-cli-channel-lifecycle.md#当前事实)。
- 外部出资使用 #5 的 CKB 默认 lock。Node3 公共节点同样使用 #5，钱包总支出还包含对方注入额；本地支出授权与源码可行性结论见 [2.1 共用 #5 的支出边界](2.1-lsp-cli-channel-lifecycle.md#共用-5-的支出边界)，本次已落实双方总注入额加每笔 1 CKB 的费用上限。
- offckb 使用本地编译的 fnn 管理 devnet 和普通节点，Shell 单独启动 Fiber LSP；本方案不改变节点管理方式，不把钱包、签名或复杂恢复逻辑放进 Shell。
- 优先复用现有 SDK 与 RPC，不修改 Fiber 服务端。发现缺少必要能力时，保留成果与恢复点，准备前置方案及顺序调整建议，待开发者明确确认后再调整和实施。
- 不自动清理已有运行数据，保留已有工作区修改；实际使用方式变化时同步更新 README、客户端账户和收付款流程文档，未实现的支付、收款仍标为待实现。
- 方案 1 的独立验收不作为本方案前置；所需现有能力按实际可用结果核对。本次没有新增必要前置，也没有待执行方案之间的顺序冲突，保留一级编号 `2`。

## 产品验收条件

以下条件均适用于现有 devnet、原客户端账户和 offckb #5；通道查询入口使用已确认的 `listChannel`，本次开发者要求按本方案执行实现与验证：

1. **P1 完整生命周期**：注册或恢复账户 → 开通 → `ChannelReady` → 协作关闭 → 关闭交易链上确认；核对 #5 的出资、退款与实际费用。
2. **P2 命令与状态**：正式命令、别名、金额边界、配置和超时行为符合上述规则；独立执行 `listChannel` 能展示当前账户的通道及待恢复阶段，包括无通道和已关闭结果，不把中间状态报告为完成，查询不推进操作；本条件通过 `listChannel` 验证，`status` 不承担本需求。
3. **P3 中断恢复**：开通请求、出资提交、签名提交及关闭受理后响应未保存时中断，重新执行能继续原操作，不重复出资、不生成冲突签名、不改动账户身份。
4. **P4 重复操作**：重复 open 返回原活动通道，重复 close 返回原结果；若服务端支持关闭后重新开通，则新通道生命周期与旧记录共存，否则明确展示限制。
5. **P5 隔离与数据保留**：错误 tenant / channel ID 不可操作其他用户通道；失败和超时保留数据，不输出私钥或 token。
6. **P6 联调文档与整体验证**：实际命令、金额单位、关闭方式、超时恢复和状态解释有对应文档；客户端相关检查与完整在线验证均完成，交易 hash、测试结果和未验证项记录可核对。在线环境不可用时只能记录离线结果，不能标记整体完成。

## 子方案进度

| 方案 | 实施授权 | 执行进度 | 依赖与恢复点 |
| --- | --- | --- | --- |
| [2.1 通道生命周期技术要点](2.1-lsp-cli-channel-lifecycle.md) | 已授权；开发者要求执行 2.1.1 全阶段 | 待验收 | 完整交付及适用验证通过；等待开发者确认结果 |

## 执行记录与恢复点

已完成：既定开通、独立 listChannel、协作关闭与恢复需求已实施，原 devnet 账户完成完整链上生命周期；重复操作及单通道限制已验证，使用文档已同步。实现与证据由 [2.1](2.1-lsp-cli-channel-lifecycle.md#执行记录与恢复点) 和 [2.1.1](2.1.1-lsp-cli-channel-lifecycle.md#执行记录与恢复点) 管理。

剩余工作：开发者整体验收确认。

验证结果：P1 完整生命周期、P2 命令及只读查询、P3 短超时和进程退出后的同意图恢复、P4 重复与再开通限制已在线验证；P5 错误输入 / 费用 / 退款、身份隔离和数据保留已通过离线检查。P3 的各提交响应丢失定向测试和 P6 的 release 交付检查已通过，共 42 项客户端测试通过；最终查询 / 重复 / 错误 channel ID / 再开通拒绝检查前后账户文件 checksum 不变。

未验证项：无（本方案范围内）；支付、收款、UDT、强制关闭不属于本方案。

验收确认：待确认。

恢复点：本级与 2.1 已待验收；开发者确认整体结果后再清理，不把技术测试通过当作用户验收确认。
