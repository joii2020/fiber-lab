# Shell 脚本规范

Shell 只用于组合少量明确命令，不用于实现复杂程序逻辑。

## General

- 保持脚本短、线性、直接。
- 优先直接写命令，不做抽象。
- 优先最小修改。
- 不为通用性、兼容性或理论异常增加复杂度。
- 默认环境明确，不主动探测或适配环境。
- Shell 可以只包含一条较长命令。

## Avoid

除非任务明确需要，否则不要使用：

- `if` / `case`
- `for` / `while` / `until`
- 函数
- 嵌套控制流
- helper/wrapper 函数
- 多文件 Shell 相互调用
- `trap` / cleanup / rollback
- 临时文件和事务式更新
- 自动环境探测
- 多发行版兼容逻辑
- 复杂参数解析
- 为 DRY 而引入循环或抽象

少量重复代码优于增加控制流或抽象。

## Prefer

优先利用命令自身语义：

```bash
mkdir -p dir
rm -f file
cp src dst
systemctl restart service
```

不要先检查再执行。

依赖缺失、命令失败等情况，通常直接让命令失败即可。

## Complexity Limit

Shell 应主要是：

```text
command
command
command
```

如果实现开始需要多个条件、循环、函数、复杂状态处理或多个 Shell 文件协作：
**停止扩展 Shell，交由开发者选择。**
不要试图把复杂逻辑继续塞进 Shell。

## Files and Permissions

- 优先使用 mkdir、cp、mv、rm、重定向等直接命令。
- 不要默认使用 install。
- 只有明确需要时才使用 chmod / chown。
- 不要为了“确保权限正确”重复修改 owner 或 mode。
- 不要无条件使用 chmod -R / chown -R。
- 尽量保持已有文件和目录的权限及所有者不变。
- 不要为了文件操作引入临时目录、原子替换或复杂权限流程，除非任务明确需要。

## Modifying Existing Scripts

- 只修改完成当前任务所需的部分。
- 不要顺便重构、抽象或增加“健壮性”。
- 不要顺便增加兼容性、环境检测、日志或错误恢复。
- 保持现有脚本风格。
- 优先减少代码，而不是继续增加逻辑。
- 可以删除代码解决时，不要新增 helper、条件或包装层。
- 保持 diff 尽可能小。
