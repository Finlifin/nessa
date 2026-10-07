# 依赖迭代结果模板验收

关联 Item 与源 `Self` 可出现在 `IterationStep(...)` 声明中，包括关联默认
结果、关联别名、嵌套 Function/Tuple/Optional 和默认方法体。签名保存显式
Item 路径；默认方法按所选 impl/scope 重新检查构造、模式与类型值，产生
准确的具体 Enum。不同作用域的 String/i64 Item 保持独立，普通同名 Enum
不能获得 Item/Self 替换能力。

编译期专用 `IterationStepTemplate` 没有可执行对象布局。所有未专化模板，
包括不含 AssociatedType 叶子的 Self-only 模板，均被可执行签名、常量、
全局和类型操作验证拒绝。错误返回 Item、普通 Enum 替代和抽象运行时 Type
值产生错误，编译失败不输出产物。

TPOL9 保存专用 kind14、Item 路径8和默认表达式6；受检往返、缺失来源、
非法/循环/具体 Item、错误路径及旧版本降版拒绝均有回归。具体结果池仍写8，
普通默认/关联/基础池保持7/6/5。三组独立进程先运行源码，再 build、删除
源码、运行 NSBC，覆盖依赖默认结果、精确 scoped Item、闭包与默认体类型值。
默认方法的堆 String 载荷经历至少三次真实 GC 和多次 continuation 恢复，
值为42且活动栈全部释放。

全仓测试1247通过、0失败、1项原有忽略；fmt、全仓编译、严格 Clippy 和
diff 空白检查通过。当前证据位于
`/tmp/nessa-step-template-{target,metadata,archive,full,fmt,check,clippy,diff}.log`。
运行期间追加的任务记录位于 `/tmp/nessa-step-template-graph.json`。
新上下文独立代理重跑全部上述检查，同样1247通过、0失败、1项原有忽略。
固定 C1–C12 中11/11适用项通过；C3因单一写入者不适用。C1、C2、C7、C8
为运行期间追加记录的自述证据，其余通过实际产物与独立重跑核查。
独立日志位于 `/tmp/nessa-step-template-independent-*.log`。

本轮完成结果类型的依赖专化；bootstrap Iterator/IntoIterator 契约、单次
next 的 for 降低和 List 接入仍未完成。公共动态 trait carrier 仍待独立设计，
本轮不改变其限制。完整协议要求见 [实施计划](tagged-iterator-plan.md)。
