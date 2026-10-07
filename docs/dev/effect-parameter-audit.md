# Effect 参数协议验收

本阶段实现静态已知 effect 的普通参数绑定；完整 Nessa 设计目标继续未完成。

## 实现

调用共享声明参数计划，压缩掉运行时提供的 catch 槽；具名参数保持源码求值顺序，
显式值先快照，再按声明顺序绑定固定、可选和单 List 变参参数。默认值随后按声明
顺序求值，使用声明词法作用域及此前的调用者绑定；显式提供值不执行默认表达式。
catch 在完整声明中的位置仍存于 handler 元数据，运行时插入 continuation，不能
由调用者按名称提供，也不能被任何默认值引用。变参后的 catch 不影响位置检查。

名称解析补齐重复参数、自身/后续绑定、catch 和控制边界检查。签名收集移动到
类型别名与关联类型准备之后，参数注解与默认表达式检查分离，所有 effect 头先
完成，普通函数体和模块值的返回推断完成后，再带期望类型检查所有默认值；未使用的默认值同样检查。later
function/effect 的调用、窄数字、optional、lambda 捕获、tuple 绑定走现有类型规则。

NIR 的共用参数降级直接产生固定 EffectCall 实参，不添加 opcode、NSBC 版本或
runtime 默认值协议。默认值加入既有选择默认值的递归展开和初始化依赖扫描。
continuation 恢复不重算已经完成的默认值，捕获/恢复仍按 delimiter 栈段链接切换。

动态 effect 调用、双 List/Map 变参、泛型变参元素、完整 async 调度、effect 传播
义务和精确 continuation answer 类型等原目标仍待完成。效应 trait 参数的物理
proof ABI 仍沿用已有明确拒绝边界；本阶段不声称补齐它。

## 检查与证据

- resolver 116 项通过，包括参数计划压缩 catch 槽、具名实参源序、alias i8
  期望和未使用默认值的 later header/catch 拒绝。
  `/tmp/nessa-effect-parameters-resolution-final2.log`。
- 隔离行为作者原 12 组及新增推断返回回归组，共 13 组通过：33 个正常 fixture、32 个具体诊断/无产物拒绝及
  2 个 runtime TypeError。覆盖源序快照、默认值顺序和抑制、词法 globals、later
  signatures、catch 所有位置、80 源码变参、heterogeneous/null/Unit、narrow/tuple/
  lambda/loops/nested handler、未使用非法声明、递归展开和模块依赖/真实环。
  `/tmp/nessa-effect-parameters-behavior-round1-repair.log`、`behavior-round1-clippy.log`。
- 隔离 GC 作者 4 组通过，至少 21 次真实完成的收集，结果 42、活动栈 0。
  heap 默认值、packed List/null/Unit、捕获默认值完成后才 dispatch、延迟 clone
  多次恢复及源序快照均检查具体数据；调用计数证明不重复默认值。
  `/tmp/nessa-effect-parameter-gc.log`、`gc-clippy.log`。
- 归档原 8 组及新增推断返回回归组，共 9 组通过：catch 所有位置、40 源码变参、源序/默认序 trace213、later
  headers、模块依赖、clone 默认调用次数1、gradual TypeError 和编译拒绝。
  成功与 runtime 错误在删除源码后的 fresh CLI process 中保持一致。
  `/tmp/nessa-effect-parameter-archive-repair.log`、`archive-repair-clippy.log`。
- Root 修复后全 workspace 1493通过、0失败、1项已有忽略；fmt 和 diff 检查通过。
  all-targets check 和 strict Clippy 通过，独立验收进行中，未声称验收通过。
  `/tmp/nessa-effect-parameters-{workspace,fmt,check,clippy,diff}-repair.log`。

未执行 baseline-red。首轮 resolver 测试把 alias TypeIndex 与 intrinsic ID 直接
比较而失败，修正为 canonical i8 判断，同时检查显式值和默认值的窄类型。
行为首轮 10/12 通过；handler 按值捕获标量，两个源序 fixture 改为调用者在
elimination 后检查自身变量，仍断言 handler 实参的准确快照。窄字面量溢出的
预期消息修正为实际的 out-of-range 诊断，未修改生产规则。失败日志保留。

## 并行和验收

使用 graph-engineering-workflow。先只读 investigator，root 读回证据，再冻结
C1-C12、所有者与正预算。root 独占生产文件并唯一合并，behavior 和 archive/GC
两个 writer 在独立副本并行写测试，共享 Cargo target 的构建通道依次使用。
graphify CLI 不可用且没有已存在的图，诚实跳过该工具并使用源码读取。

记录 `/tmp/nessa-effect-parameters-graph.json`，基线 `baseline.tar`，最终差异将
写入 `integrated.patch`。独立 grader 只接收 rubric、最终文件、基线、diff 和实际
日志，不接收构建会话；验收和必要修复重评循环最多 2 轮。
C11 为参数绑定、类型/默认值边界和无效产物拒绝；C12 为 source/NSBC/GC/
continuation/init/default recursion/质量门。C1/C2/C7/C8 为运行记录核对，C4 无
外部研究不适用。首轮独立验收 10/11 适用项通过，C11 有 major 缺陷：默认值调用 later 的
无返回注解函数时仍使用旧 Any 类型，非法未使用默认值生成 NSBC。对调声明顺序
或提供显式 bool 返回注解会正确拒绝。复现与失败证据保留在
`/tmp/nessa-effect-parameters-grader-inference-repro.log`。

生产修复由 root 负责，将 effect 默认值检查推迟到普通函数体的返回推断完成后，
同时保留所有 effect 头在任何默认值之前完成的约束。behavior 与 archive 作者
独立补齐 inferred bool/i64 的声明顺序、selected/unused、无产物/无归档回归。
本轮修复后，behavior13/13、archive9/9、全 workspace1493/0/1和所有质量门通过。第二轮独立复跑同为1493/0/1、behavior13/13、GC4/4、archive9/9及所有质量门通过，
但 C11 仍失败：`wrapper(){leaf()}` 位于无返回注解 `leaf(){true}` 之前时，wrapper
保留旧 Any，非法 i64 默认值仍会生成 NSBC；将 leaf 移到 wrapper 前则正确拒绝。
复现见 `/tmp/nessa-effect-parameters-grader-inference-round2.log`；独立质量与专项
日志为 `/tmp/nessa-effect-parameters-grader-{workspace,fmt,check,clippy,diff,behavior,gc,archive}-round2.log`。

本专项以 10/11 适用项结束既定 2/2 轮验收，gate 为 capped，未验收通过。
没有弱化 C11、隐藏链式推断缺陷或标记完整目标完成。C1/C2/C7/C8 是记录核对；
C4 不适用。5/5角色、峰值3/4并发、4/4波次、0/1重试、1/1深度、7/9派发turn
均在限制内。后续从一般函数返回类型的依赖推断修复根因，再验证参数协议。

后续的[函数依赖推断专项](function-inference-audit.md)已修复这项无注解函数链
漏检根因，并通过独立11/11适用项验收。该验收独立复跑原参数行为13、GC4、
归档9及原始错误链，错误默认值正常诊断拒绝且无归档；全workspace1525/0/1
和全部质量门通过。本文件保留原两轮capped历史，完整Nessa设计仍在实施。
