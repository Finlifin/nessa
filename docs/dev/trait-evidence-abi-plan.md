# Trait evidence ABI：设计与实现缺口

本计划落实 `docs/type-system/trait-implementation-extension/dynamic-dispatching.md`
规定的调用方隐式 vtable 参数。旧实现只保留原始 TaggedValue，在每次 TypeAssert
和按名称调用时按当前 PC 的词法作用域重新选择实现；这种行为不等于传递证明。

## 已复现的错误

- a 中 `extend Read for P` 返回 40，a.make 返回 Read；b 有返回 2 的另一个
  实现，b.consume(Read) 实际返回 2。传输 trait 值丢失了原选择。
- a 将同一个 Read 存入 struct 字段后交给 b，b 读字段也选成自己的实现。
- a 的局部实现传给根作用域 consume(Read)，参数 prologue 重查根作用域，
  因无可见实现而 TypeError；现已由 caller proof 与入口配对检查修复。
- P 同时实现 Left.value 和 Right.value，通过各自 trait 参数调用时，当前
  CallMethod 只保留 receiver 与名称，因两个候选报 AmbiguousMethod；现已
  改用 TraitCall 的冻结接口槽，源码与独立归档回归验证各自调用。
- Read.value 声明返回 i64，impl 却返回 String；本轮修复前 CLI check 仍输出 ok，
  consume(Read)->i64 的运行结果是字符串 bad。required 方法的接口签名当时未
  与实现比对；现已补声明、真实 Self 路径和实现签名检查，错误程序明确拒绝。

调查输入保存在 `/tmp/nessa-trait-proof-reselect.ns`、
`/tmp/nessa-trait-proof-field-reselect.ns`、
`/tmp/nessa-trait-proof-global-param.ns` 和
`/tmp/nessa-trait-collision-probe.ns`，以及
`/tmp/nessa-trait-method-bad-signature.ns`。前两项返回/字段证明传输仍待决策及
实现；参数作用域与同名槽问题已修复，签名不匹配已拒绝，
对应回归已纳入源码与归档自动化测试。临时文件只用于复现。

## 先决元数据

TypePool 必须保存稳定的 trait 方法描述，而非让 loader 从任意 func_id 猜测
接口。方法 key 包含声明 trait 和名称，schema 按父 trait 在前、声明方法在后
的顺序建立，保留当前按名称去重及 child 自有方法覆盖同名父槽的规则。
bootstrap trait 使用明确的固定接口；不同实现不能自行改变同一 trait 的槽顺序。

受检 vtable 描述由 exact (implementor, trait, visible_scope) 的 record 和该
schema 导出；父方法按创建 table 时的 scope 选择，逐槽检查对应的真实函数身份。
声明槽的 trait owner 与实际覆盖方法的 implementation trait 分别保存和校验。
无 schema 的 legacy table 仍可归档，但不能凭 func_id 列表授权新 evidence 调用。
TPOL 新 revision 明确保存 schema，旧 revision 不补造该证明。

元数据已覆盖槽身份、实现选择和签名契约。TPOL5 已保存并校验源码接口声明签名、真实 Self 路径和
参数类别，并逐实现/目标函数验证 Self 替换后的精确签名。没有声明的 bootstrap
槽以及旧格式仍保留缺签名状态。NSAM4 的 FunctionAbi 已明确逻辑/物理
数量，裸参数与 capture proof 可执行；默认方法的具体 adapter 使用 NSAM5
TraitSelf 接收原冻结证明，独立目标避免多个 implementor 覆盖。无签名或
derive sentinel 目标明确 Unsupported。名称一致不证明签名一致。

静态方法调用已使用声明的 Self 路径特化真实 receiver view：Child 参数调用
父接口的 `clone()->Self` 返回 Child，显式 `clone()->Base` 仍返回 Base。
嵌套函数、tuple 和 Optional 保留同样区别；不兼容的 child 重声明拒绝，
不改变已持久化父槽的契约。裸参数的运行时 parent proof 投影亦保留原
table 选择；这不表示 Self 返回或嵌套存储的证明传输已经完成。

具体入口布局及调用/GC/归档消费者见
`docs/dev/trait-parameter-abi-plan.md`；bootstrap 缺签名、native 目标和关联返回
类型的后续要求见 `docs/dev/bootstrap-trait-signatures-plan.md`。

## 参数 ABI 与数据流

遵循既定设计：每个 trait 参数传入原始数据及隐藏 evidence 参数。函数入口
验证 evidence 的 trait view、table 身份与数据实际类型，不按 callee 的 scope
重新寻找实现。普通具体 impl 的 self 只接收数据；trait 默认方法和 Self 参数
按真实签名及 ABI 描述传递证明，不能以 param_count 猜测。

NIR 必须显式跟踪数据与证明的对应关系，局部复制、转发、闭包捕获和 continuation
保存都必须保留这对值。trait 方法调用使用接口方法槽及 evidence，进入普通调用帧，
不能退回按对象类型和名称重新选取方法。Trait→parent 的转换投影原 table 中的
固定父视图，包括创建时已选定的覆盖；不能在接收作用域重新查父 record。

FunctionCode/归档显式区分逻辑参数、证明参数、captures 及返回布局。普通调用、
闭包与 CallIndirect、native adapter、effect 和 continuation 共用该描述；不能
继续用 physical_param_count - source_param_count 直接推导 captures。

## 返回与存储语义

原动态分派文档将 trait 返回值标为 TODO。已向用户提出统一语义：转换取得的
证明随 trait 值传输，原 scope 只控制新证明的取得，已有 trait 能力只授权接口
内的方法；普通实例不会得到扩展权限。该选择尚待明确，不能将未回复视为批准。
参数隐式 vtable 和可信元数据的实现不依赖此未决返回值语义。

若采用上述统一语义，trait 返回需要同时返回数据与 evidence；global、field、
Tuple、Enum、Any、闭包单字 capture 及非 null Optional 等位置需要保存二者的
内部 carrier。普通参数传递不必堆分配；carrier 用于需要单 TaggedValue 存储
或逃逸的位置，不能替代规范中的隐式 vtable 参数。null 无证明也不创建 carrier。
不能改共享对象 header，也不能以对象地址作为唯一证明缓存键：同一对象可同时
持有多个 trait 与不同 scoped implementation，GC 也可能影响地址。

取得新 evidence 时检查作用域与所选方法权限，传输的能力不能授予接口以外的
普通或私有方法权限。GC 必须保留原始 receiver 与 carrier，证明 handle 本身
是受检非指针元数据身份；非法 handle、slot、view 或布局都应明确报错。

## 兼容与完成证据

旧 metadata、opcode、单返回和参数布局保留原解释。新增可执行证明使用明确
格式/ABI 标记，不偷偷更改 NSAM1–3 或普通 UInt/Symbol 常量的含义。归档验证、
函数/常量搬迁、PC 上下文表和实际 runtime 消费必须一起验证。

完成回归必须覆盖不同 module 的 40/2 选择经同一外部函数保持、同名 Left/Right
方法、父视图与 diamond、Self 参数/返回、默认方法、多 trait 参数与寄存器边界、
闭包/多次 effect 恢复、存储和 Any 转换、真实 GC 及删除源码后的 NSBC 执行。
损坏 schema/table/witness/signature/ABI/parent mapping 必须拒绝。旧归档往返
不伪造证明。只有这些行为贯穿 source→NIR→bytecode→runtime→archive 后，
才能声称隐式 vtable 动态分派完成。
