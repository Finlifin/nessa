# Trait 参数隐藏证据 ABI 实施计划

本文区分现状与后续方案。trait dispatch schema、真实 Self 路径、接口签名和
裸 trait 参数的调用方隐藏证明与接口槽动态分派已经接通。
本计划不采用 boxing 替代隐藏参数，也不决定 trait 返回、字段、global 或 Any
存储的未决逃逸语义。

共享 FunctionAbi 与 NIR Capture/User 角色已接入 codegen、runtime 和 NSAM4，
裸 trait 用户参数生成 proof、data，capture 保存 data、proof。显式捕获数、
逻辑参数数和物理数量分别校验；旧 NSAM1–3 的 ABI 保留 None，沿用原解释。
入口配对验证、接口槽调用、父视图投影、typed local/CFG 转发、默认表达式声明
作用域、已知间接调用、闭包与多次 continuation 恢复已接通。真实 GC 与删除
源码后的归档执行已验证。显式 Any 绑定继续擦除证明，返回和存储语义仍待决策。
“现有生产者与消费者”表记录调查时的起点，并非所有行仍未修改。

## 规范与参数顺序

`docs/type-system/trait-implementation-extension/dynamic-dispatching.md` 的示例将
`animal_vtable` 放在对应 `animal: Bird` 参数之前。本轮方案遵循这一位置：

```text
物理入口 = closure capture 前缀 + 按声明顺序展开的逻辑参数
普通参数展开：data
trait 参数展开：proof, data
```

例如 `fn f(n: i64, a: Read, b: Write)` 的普通入口是
`n, a_proof, a_data, b_proof, b_data`；有两个 capture 时前面再加 capture 前缀。
源函数类型只包含 `i64, Read, Write`，证明不是用户可见参数。

trait proof 放在对应 data 前；effect handler chain 单独处理。设计中的8个
寄存器加栈传参尚未落地；当前窗口32个 TaggedValue，间接调用还需 callee
寄存器，TraitCall 还需 proof 寄存器。溢出受检拒绝，不声称完成原生栈参数 ABI。

## 现有生产者与消费者

| 位置 | 当前行为 | 必要修改 |
| --- | --- | --- |
| `engine/nir/src/lib.rs` 的 `NirParam`、`NirFunction` | 参数平铺，只有 `is_evidence: bool`；source `function_type` 不含 captures | 显式参数角色和逻辑到物理位置映射，区分 capture、trait proof、用户 data；effect 证据单独处理 |
| `engine/nir/src/lowering.rs::lower_function_def` | 普通参数按 AST 顺序生成 | 对 trait 参数分配 proof local 与 data local，并记录对应关系 |
| `engine/nir/src/expr.rs::lower_lambda_body` | captures 在用户参数前；所有 capture 仍标为非 evidence | 捕获 trait 参数时同时捕获 data/proof，明确 capture layout，不把 proof 误认普通数据 |
| `engine/nir/src/expr.rs::lower_call`、`arguments.rs`、`defaults.rs` | 已处理源码求值顺序、具名参数和默认表达式 | 数据绑定完成后插入对应 proof；复用已有证明，或在调用表达式实际词法 scope 取得新证明 |
| `engine/nir/src/trait_dispatch.rs` | 对已知 concrete 类型按 lexical scope 选 source FuncId | trait 参数方法使用 proof 和接口槽；保留 concrete 类型原有静态分派路径 |
| `engine/codegen/src/lib.rs::compile_function` | 将 `params[i]` 从 `r[i]` 保存到 slot，再逐参数 TypeAssert | 按物理 ABI 保存全部入口槽，随后检查 data/proof 配对；不能对 trait data 再按 callee scope 选 impl |
| `engine/codegen/src/lib.rs::emit_expr` | Call 顺序加载参数；CallIndirect 把 callee 放在 args 后一寄存器 | logical/physical count 分离；新增 proof 取得、检查及 trait slot 调用 lowering |
| `engine/codegen/src/validation.rs` | 校验总参数 32、间接参数 31、capture count 等 | 校验显式 layout、proof 位置和 register 容量，缺证据不能输出产物 |
| `engine/nsbc/src/lib.rs` 的 `CompiledFunction` | 仅总 `param_count`、`is_closure`、source `function_type` | 保存独立 FunctionAbi 描述，供 loader、VM、validator 共用 |
| `engine/nsbc/src/validation.rs::captures` | 用 `param_count - Function.params.len()` 推断 captures | 删除该推导；依据明确 capture layout 与物理参数数量检查 |
| `engine/runtime/src/lib.rs::FunctionCode` | 同样只保存总参数数量与 source signature | 安装相同 FunctionAbi，受检计算调用入口 |
| `engine/interpreter/src/lib.rs` 的 Call/CallFar、`push_call_frame` | 普通调用不使用指令 arg_count；保存 frame slots 和寄存器 | 执行时检查目标物理 ABI、proof 与数据类型，再进入 frame；不只依赖 artifact 静态检查 |
| `engine/interpreter/src/effects.rs::invoke_closure` | capture payload 全部加载后 append args，按总 count 检查 | 按 capture layout 和逻辑参数展开，不把 hidden proof 数量当 captures |
| `engine/interpreter/src/lib.rs::exec_call_method` | concrete 类型和名字重新选方法，插 self 到 r0 | trait call 独立消费证明里的 slot/FuncId，不能走这个重新选择路径 |
| `engine/interpreter/src/types.rs` | Trait TypeAssert 使用当前 instruction scope 查询实现 | 参数入口改用 proof 验证；普通取得新证明的 cast/check 仍受实际 scope 限制 |
| `engine/nsbc_io/src/artifact.rs` | NSAM1–3 保存函数名、签名等；CODE 保存旧总 count | 新 metadata/执行 ABI revision 显式保存 FunctionAbi，不重解释旧 count |

## 显式布局与证明身份

建议共享 `FunctionAbi` 保存 logical 参数数量、capture layout，以及每个逻辑参数
对应的 proof view 和物理位置。capture layout 的每项明确是捕获 data 还是其
trait proof。总物理参数数量由该描述计算，并与 CODE 的 count 交叉检查。
不能再通过 `source parameter count` 与 `physical count` 的差值推断 captures。

NIR 参数角色至少区分 `UserData(logical_index)`、`TraitProof(logical_index, view)`、
`CaptureData(capture_index)` 和 `CaptureProof(capture_index, view)`。用户变量仍
对应原始 TaggedValue local；另一个 local 保存证明。`let y = x`、参数转发和
lambda free-variable capture 必须复制这对关联，不能只复制 data。

证明可采用 VM 内部、专用 immediate tag 的受检 handle，指向 VM 生命周期内
不可变的 proof registry；不是 raw UInt，也不是 receiver boxing。registry
保存 root vtable 身份、concrete type、trait view 和冻结的 slot mapping。
handle 不持有对象地址；同一对象可以同时具有不同 scoped implementation
证明。已使用独立 immediate subtag 10、五条新 opcode 及 NSAM4 显式布局；
没有重解释旧 UInt/Symbol 或旧入口。按 (root table, canonical view) 缓存
handle，但每次取得仍先核查 scope、权限与签名；pool 重装令旧 handle 失效。

取得新 proof 时依据真实 caller scope 选择唯一 exact implementation，并使用
`TypePool::checked_vtable_descriptors`、签名和方法权限检查。取得之后的调用
不再按 callee 或当前 scope 重选实现。入口验证 handle/view 存在、数据反射的
canonical concrete type 与 proof 相符，以及 proof 属于当前安装的 type pool。
缺签名、LegacyUnknown、错误 handle 或歧义都必须明确报错。

Trait→parent 的转发应投影原 proof 的固定槽映射。Child 自有方法覆盖父槽时也
要保留该选择；重新取得独立 Parent table 会丢失覆盖语义。多父 trait 同名槽
按现 schema 的 name shadowing 规则检查兼容性，不能凭相同 FuncId 猜 slot。
若父 view 的签名不能与所选槽吻合，应明确拒绝或先补 checked parent-view
schema，而非偷偷换成当前 scope 的 Parent implementation。

## Trait 槽调用

增加独立的 `TraitCall` NIR/bytecode 操作，携带 proof、receiver、trait view、
接口槽以及已求值参数。slot 来自声明 schema，不能只存方法名字。VM 先验证
proof/view/slot，再取得固定 descriptor 和 target FunctionAbi，检查真实函数
签名并展开目标入口。具体 impl 的 self 接收原始 data；目标若自身具有 trait
参数，则按照其 ABI 转发匹配的 proof。

不要直接将带隐式 proof 的 register 数量塞进当前 CallMethod 的逻辑 arg_count。
CallIndirect 的 closure 和普通 apply fallback 也必须区分逻辑参数与物理 proof
布局；callee 为 Any 时，VM 应依据目标 ABI 在当前 caller context 取得所需证据，
或明确拒绝尚未支持的动态布局。不能将缺 proof 当默认 UInt/Unit。

新操作宜使用显式 typed metadata 描述 view/slot/layout，按已有窄/宽 metadata
模式编码。字段数量须先核实指令容量，不能静默截断。新增 opcode 与 metadata
需更新验证器、constant relocation、PC scope coverage 和所有归档消费者。

Trait 默认方法已按每个实现生成独立具体 adapter。TraitSelf 描述具体签名
中源 Self 参数的 proof/data 入口，TraitCall 转发原冻结证明及其父投影，
不在 provider scope 重新选实现。显式 trait 参数仍使用 Trait；详细布局与
Self provenance 要求见 default-trait-method-plan.md。

## 闭包、continuation 与 GC

`engine/runtime/src/lib.rs::ClosureEnv` 当前前两 word 是 raw FuncId、capture count，
后面是 TaggedValue captures；`engine/gc/src/lib.rs::scan_object` 跳过前两 word，
扫描其余 payload。可以继续把 data 与专用 immediate proof handle 分别保存为
capture 槽，避免改变 receiver 的对象 header。capture count 表示实际 payload
槽数，分类由 FunctionAbi 给出。

`engine/runtime/src/lib.rs::CallFrame::evidence` 是 effect handler chain，不能
直接当 trait proof 参数表。proof 若作为 TaggedValue 保存在 register/local
slots，可复用现 frame 的保存和恢复；对象 data 仍由原有引用槽保持可达。
证明 registry 只持有静态 metadata ID，因此不需要将 proof handle 当 GC 指针。

`engine/runtime/src/stacks.rs` 的 StackContext/TaskStacks 持有 active 和 captured
栈段；`engine/interpreter/src/lib.rs::scan_vm_roots` 扫描全部 contexts 的寄存器、
locals、saved registers、closure environments 和 handlers。新增槽必须落在这些
已扫描区域，或补相应 root producer。delimiter 捕获和恢复保持链接拆接，不复制
调用帧；多分支 clone 会复制 TaggedValue proof handle，仍引用同一冻结证明。
不能把证明挂在一个不参与 context 保存的全局“当前 trait”变量上。

## 原纵向实施顺序与当前缺口

以下为调查时的实施顺序；显式布局、裸参数分派、父视图、闭包、归档及 GC
纵切已完成。第3步的默认 adapter、第2步的嵌套布局与第5步的返回/存储仍待完成。

1. 先落共享 FunctionAbi 与参数角色，迁移普通函数、lambda、native adapter、
   handler body 和所有 fixtures；没有 proof 的调用行为保持一致。加入 layout
   损坏和 logical/physical/capture 数量检查。
2. 打通裸 trait 参数的 proof 取得、proof/data 入口检查和独立 slot call，覆盖
   caller 局部 extend 经外部 `consume(x: Read)` 调用。普通具体值不改 header，
   type_of 仍返回具体类型。此时 nested trait/Optional trait 参数若无完整展开
   描述，明确诊断其缺口。
3. 支持 trait 参数转发、parent view、多个 proof 参数及已知 function value/closure
   ABI；lambda 捕获参数时捕获 data/proof。补默认方法 adapter 与 Self 参数检查，
   不允许在转发过程中重新取得更近 scope 的证据。
4. 迁移新 executable archive revision，跨进程删除源码执行上述路径，并补恶意
   slot/handle/layout/signature 与未声明 proof 参数拒绝。source 与 archive 共用
   installer 和验证器。
5. 验证真实 GC、effect 捕获和 continuation 多次恢复。返回与字段/global/Any
   存储另按用户明确的逃逸语义实施，不能用参数 ABI 的成功冒称其已完成。

## 归档与兼容性

当前 TPOL5 保存接口签名，NSAM4/5 显式保存 FunctionAbi、scope coverage 及 scope
表 presence，并核对 CODE 总 count。外层 NSBC3 和 builtin ABI3 保持；旧 reader
拒绝未知 metadata revision。无TraitSelf的Some仍写4；TraitSelf必须写5，
4拒绝参数tag2，读取兼容1–5。读取旧1–3时 ABI 保留 None，不补造 proof 或以 count 差值推测
新 layout；全部 None 的重复保存仍保留原1/2/3 scope 契约。0x6A–0x6C、
0x92–0x93 已实现；旧 reader 拒绝未知指令。proof handle 不成为持久化常量。

runtime proof handle 是进程内身份，不写入 archive 常量。归档保存的是取得
proof 所需的 checked schema/table 和指令，不持久化用户进程的 handle 值。
新 typed metadata 的函数、类型与字符串引用必须受检搬迁；不能重写所有 UInt
常量来模拟 metadata relocation。table 与 FunctionAbi 安装之后，在 proof
存活期间不得悄悄替换 type pool 或改变其表身份。

## 验收程序与边界

最小成功案例是两处不相交的 module extend 分别返回 40、2，通过同一个外部
trait 参数函数得到 42；callee 自己看不到这两个 extend。还需证明同名 Left/
Right 方法不混用、Child→Parent 与 diamond 不重选、明确 Self 与显式 trait
参数不同、static 无 receiver 槽不会假插 self，以及多 trait 参数 register
容量的正常和拒绝边界。

closure 捕获与间接调用要实际读 receiver 字段并在 GC 后运行；effect handler
内经过 trait 调用，再多次恢复同一个 continuation，结果与作用域选择须稳定。
完整归档回归必须删除 source 后独立执行。错误案例覆盖缺 proof、错误 concrete
类型、错 view/slot、LegacyUnknown、无签名、错误 capture/proof 数量、超容量及
损坏 metadata。当前裸参数与基本默认方法 adapter 纵切已完成；复杂 Self provenance、缺签名 bootstrap
接口、nested trait/Optional/Tuple 展开、trait 返回与存储仍未完成。Eq/PartialEq comparison
已接入 proof 槽；for 的裸 trait 迭代因关联返回 proof 契约不完整而在编译期
明确诊断，concrete 迭代保持原路径。旧 EffectCallDyn 的裸
trait 输入明确 Unsupported，需要独立新协议，不能扩写旧 scope coverage。
