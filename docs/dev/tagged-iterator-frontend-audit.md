# 具体迭代结果源码接入验收

`IterationStep(Item)` 已作为编译期工厂接入 root bootstrap 与 std builtin。
应用根据已解析的工厂绑定身份检查，不根据调用名称猜测。导入、重命名及
工厂别名保持身份，同名用户声明使用普通语义；工厂自身不能作为运行时值。

具体实例支持类型注解、类型值、done/yielded 构造、载荷匹配和准确反射。
Item 可为具体 Tuple、函数或嵌套结果；null/Unit 元素不表示结束。
实例别名及直接工厂应用可作为 impl 目标，静态、实例和 trait 方法真实执行。
错误载荷与非法类型应用产生诊断或受检运行错误，无效源码不生成归档。

运行时只保存具体类型及已有 Enum 指令/常量，无新增工厂 ABI 或固定 intrinsic
索引。三组新进程回归先执行源码，再 build、删除源码、独立运行 NSBC，覆盖
null/反射、String 载荷及直接实例 impl 方法。另有真实 GC（至少三次完成
收集）和多次 continuation 恢复的堆载荷保活检查，最终结果为 42，栈段释放。

最终全仓 1239 项测试通过、0 失败、1 项原有忽略。fmt、全仓编译、严格
Clippy 和 diff 空白检查通过。日志位于
`/tmp/nessa-step-frontend-{target,archive,workspace,check,clippy,fmt}.log`。

新上下文独立代理重跑源码测试、三组归档执行和全仓检查，固定 C1–C12 中
11/11 适用项通过；C3 因单一写入者不适用。C1、C2、C7、C8 为运行期间
追加记录的自述证据，其余以实际产物和独立重跑核查。运行记录位于
`/tmp/nessa-step-frontend-graph.json`。

符号关联 Item 目前明确拒绝，依赖模板与专化路径尚需实现；单次 next 的
Iterator/IntoIterator 契约、for 和 List 接入仍未完成。本次具体源码能力
不能替代完整迭代协议验收。后续要求见 [协议计划](tagged-iterator-plan.md)。
