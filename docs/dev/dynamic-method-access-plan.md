# 动态方法权限：已验证缺口与实施要求

本文件记录动态方法授权的实现依据和剩余范围。源码、解释器和独立归档路径
已接通调用点授权；完整 scoped trait/包链接等能力仍不能由普通方法测试推断。

## 已查明的原始缺口

- `type_pool::MethodSlot`仅包含name、func_id、trait_impl和visible_scope；
  visible_scope描述extend作用域，并不描述private/public。
- resolution/traits.rs登记普通方法前剥离PubDef/PrivateDef，普通impl方法的
  visible_scope均为None。不能从None反推出public。
- TPOL1只保存上述四项；FunctionCode/CompiledFunction不保存词法scope，
  artifact也没有scope-parent/package表。VM按名称取首个方法，没有访问筛选。
- docs/type-system/trait-implementation-extension/extension.md规定extend仅在
  当前scope和子scope可见，不同模块可以独立定义同名扩展。可见候选重叠时
  规范没有给出任意登记顺序优先的依据，必须报告歧义或先明确选择规则。
- 现有动态方法调用已经校验函数存在、self+实参数量和寄存器边界。这些检查
  解决调用正确性，不解决访问权限。

## 必需元数据及职责

方法元数据需要显式的Public/Package(package ID)/Private(defining scope)访问
等级，另保留独立extend scope。Scope表保存父scope和真实package身份；不能
用文件名、模块名或StrId代替package ID。编译器按源声明保存权限，而不是在
VM中根据concat/apply等名字猜权限。

这部分基础现已实现：MethodAccess 显式区分 Public/Package/Private/LegacyUnknown，
ScopeContext 保存 parent、归档局部 package ID 和 canonical assoc_type。普通模块
继承包身份，显式已加载 package root 才建立独立身份；静态 same_package 与
元数据发布共用规则。snapshot 与 TPOL2 保留这些字段，旧 TPOL1 方法标记
LegacyUnknown，不从 visible_scope 推断权限。TypePool::method_accessible 提供
受检查询，同时叠加 extend 范围；private 保留同包同类型跨 impl 的既有语义。
该查询现已用于解释器动态分派：授权后再检查歧义、函数身份、arity和调用帧。
集合 intrinsic 协议在授权后保留原受检整数索引 ABI，不通过 std 的 usize
wrapper 提前拒绝既有 signed index，也不能在权限筛选前走 native 绕过。

前端 `extend self/Self` 缺口已修复：扩展 scope 预准备并绑定 canonical 所属类型，
候选按声明 scope及后代、访问等级筛选，互不相交的模块和块扩展不再污染全局。
局部函数收集进入函数体，局部扩展方法有真实 FuncId；lambda 的 free-variable
分析补上 self。具名扩展方法捕获外层 local 无环境 ABI，现明确诊断而不伪读槽。

仅给函数保存一个scope不足：函数内部block中的extend也有词法边界。每个
动态method调用点需要保存scope（可用func_id+instruction PC映射）。普通函数、
闭包及生成initializer/adapter也必须有确定的scope来源。VM从当前执行调用点
取得scope，不借用动态caller或handler的权限；continuation恢复依然使用保存的
函数和PC。宽方法调用、闭包apply fallback和update必须沿用同一授权路径。

这些调用点现通过 NIR ScopedCall 和 CodegenOutput.method_call_scopes 保存。
codegen 拒绝无 scope 的新动态调用，按真实发出的 PC 记录；VM 按当前 task 的
current_func 和执行 PC 查询，不依赖函数范围或动态caller。换装类型池清除旧表，
无效新表原子拒绝，不能残留部分授权。

分派先筛选访问等级及extend作用域，再检查剩余同名候选是否歧义；通过后再
执行签名/arity验证和普通调用帧转移。静态直接调用的编译许可仍在resolution
处理；编译器决定的权限与运行时动态筛选不能互相冒充。

## 持久化与兼容性

类型池 metadata 已采用 TPOL5，新完整上下文使用 NSAM3，外层仍为 NSBC3。
旧 tag/字段的解释不变。
读取已验证 scope parent IDs、无环、package引用、方法访问/extend scope引用；
已保存并验证所有动态method call PCs的scope覆盖。方法函数ID、常量搬迁和调用点PC映射
必须共同验证；当前 constant permutation 不增加指令 word，窄/Far 搬迁回归已
证明 PC 保持。将来改变指令宽度或链接布局时必须重定位该表。

旧metadata丢失private信息，不能通过补默认Public完成迁移。NSAM1读写保持
None上下文，旧 TPOL1 方法保持 LegacyUnknown，不能授权该动态方法能力，须
从source重编译；纯 ordinary 和 closure调用仍按既有ABI运行。新词法图含动态
调用却标为无完整上下文时在加载前拒绝，不能降级绕过检查。NSAM2明确Some
完整表（允许空），搬迁保持同长度；版本兼容及损坏输入已有具体回归。

TPOL4 新增持久化 trait slot schema，TPOL5 又保存接口签名、参数类别与真实 Self 路径。
受检描述与 artifact validator 校验声明 owner、原 record scope、
父槽与函数 ID；旧1–3不推导新证明。它是隐式 vtable ABI 的前置元数据，还不是
运行时传输 evidence。实际跨作用域 trait 参数/返回目前仍会重新选择实现，
有已复现的错误，后续路径与完成条件见 `trait-evidence-abi-plan.md`。

## 仍需落实的范围

同类型同 trait 的多个词法扩展已按 (type,trait,scope) 独立登记与查询；
重复的同 scope 实现拒绝，可见重叠明确报歧义。NSAM3 还覆盖显式类型检查、
转换与隐式存储/字段/枚举类型检查，旧 NSAM2 保留仅动态调用的覆盖含义。

具名扩展方法捕获环境、完整 trait evidence/boxing ABI、扩展关联值/hook初始化和
跨独立包链接尚未完成，不能借普通动态方法授权扩大已实现范围。

## 验收

- static/private内部可调用、外部拒绝；Any路径在相同实际调用权限下行为一致。
- Package授权使用真实包身份；嵌套模块和文件名不产生额外授权。
- 两个模块的同名extend独立可见，block退出后不可见，可见重叠显式诊断。
- Closure、普通method、apply/update、宽调用及continuation恢复使用正确调用点。
- 删除source后，新archive保留访问行为；损坏scope/PC/package引用明确拒绝。
- 旧归档兼容矩阵、迁移失败和无动态方法的兼容路径都有具体测试。
- 完整workspace编译、测试、strict Clippy、格式和diff检查通过后才能宣布完成。
