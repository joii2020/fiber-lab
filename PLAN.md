# fiber-lab 执行进度

总方案：[3 lsp-cli 账户归档与指定账户管理](plans/3-lsp-cli-account-archive.md)。方案 1 历史见 Git 提交 `eeed91a`；方案 2 的规划与在线验证记录见 Git 提交 `cde9aa2`，已由开发者验收并清理。

## 当前进度

| 方案 | 实施授权 | 执行进度 | 已执行内容 | 依赖与恢复点 |
| --- | --- | --- | --- | --- |
| 1 lsp-cli 联调工具定位与管理 token 签发（历史见 `eeed91a`） | 已授权；用户已审核通过原方案 1 的实施范围 | 待验收 | 原方案实现及离线验证已完成 | 无新增前置；原记录 Node3 RPC 未运行，环境恢复后验证在线兼容性并验收 |
| [3 lsp-cli 账户归档与指定账户管理](plans/3-lsp-cli-account-archive.md) | 已授权；本次用户要求实现账户归档、列表及历史账户关闭 | 待验收 | 已实现账户清单、归档 / 列表、统一 --account 与 closeChannel 别名，完成本地回归验证及文档更新 | 下一步真实 devnet 多账户关闭 / 退款联调与开发者验收；方案 2 历史见 `cde9aa2` |

## 本次已执行

- 实现 archive-account 与 list-accounts，归档释放默认位置并保留原目录、身份和通道记录。
- 客户端命令统一支持 --account ID，closeChannel 可指定归档账户并恢复关闭，显式选择不会切换默认账户。
- 更新 README 与客户端账户文档；完成本地测试、格式检查、Clippy 和 release 构建；真实 devnet 多账户关闭与退款仍待验证。
