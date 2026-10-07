# 带标签迭代结果基础验收

本次完成具体 `IterationStep(Item)` 的内部类型身份、VM 检查和归档支持。
精确 Item 和别名复用、不同 Item 及普通同名 enum 身份隔离、准确反射、
`yielded(null)` 与结束区分、错误载荷拒绝均有回归覆盖。
显式结构来源需要 TPOL8，普通类型池继续选择 TPOL5/6/7；旧版本不补造
结果身份。损坏布局、降版和重复结构实例被拒绝。

归档校验对已知结构来源直接检查描述符，避免逐项重复扫描来源列表。
公开查询仍检查来源，不按名字授权类型身份。

最终检查：workspace 1233 通过、0 失败、1 项原有忽略；fmt、全仓编译、
严格 Clippy、diff 空白检查通过。可检索的执行日志：
`/tmp/nessa-step-{workspace,check,clippy,fmt}.log`。

独立新上下文审查覆盖固定 C1–C12：11/11 适用项通过，C3 因单一写入者
不适用。C1、C2、C7、C8 根据运行时追加的记录核查，属于自述证据；其余
通过实际产物读取及独立重跑验证。最终受影响库重跑：interpreter 92、
nsbc_io 77、type_pool 68 通过。运行记录：
`/tmp/nessa-step-foundation-graph.json`。

这只验收具体类型基础。源码类型工厂、符号 Item 模板路径、新
Iterator/IntoIterator 契约、for 和 List 接入、对应源码删除归档及真实
GC/多次 continuation 恢复的完整协议测试仍需实现，项目整体尚未完成。
实施约束见 [协议计划](tagged-iterator-plan.md)。
