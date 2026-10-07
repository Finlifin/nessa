# 后续实现调查（2026-10-07）

这份记录来自三个 subagent 的只读源码审查和局部复现，用于安排后续实现。
调查结论不是功能完成的证据；已修复事项注明状态，其余仍待落实。

## 当前优先级：类型系统、builtin/std，然后 NSBC 持久化

用户要求以上领域优先。先补类型和值语义，使 std 的真实类型、符号、函数和导入
可以被可靠保存；原目标中的 continuation ABI、GC、async、FFI 等仍需完成。

### 类型与 builtin/std 的完整切入点

- 整数提升已改按位宽和符号范围判定，拒绝 u64→i8/i64 等隐式窄化。
  initializer 已先解析 annotation 再按 expected type 推导，整数字面量按位宽、
  符号与基数检查范围，包含 signed minimum、128-bit 边界和类型别名；普通
  固定参数函数即使未标返回类型也检查参数。算术仍需实际类型检查和转换，
  `.as(T)` 已贯通并受检转换；现已实现 57-bit 外的精确整数堆表示、128-bit 常量
  和 f64 无损堆回退，任意精度整数与完整数值API仍需继续；固定宽整数和真实字符值已贯通。
- TypeCheck/TypeCast 已补基本谓词与受检转换，TypeCastSafe值失败返回null；
  Any→具体声明、赋值、固定参数、返回与布尔条件已生成动态 TypeAssert，
  隐式数值拓宽与窄literal/算术表示经coercion落实；间接typed调用入口受检。
  字段构造与携带metadata的Function检查已接通；完整复合类型描述、参数ABI
  与函数variance仍需实现，不能用当前谓词和边界检查覆盖全部类型。
  Type 值、type_of 与动态标量/struct 反射已贯通，null 为 ?NoReturn；
  已知元数据的 closure 可反射完整 Function 签名并精确检查；函数 variance、
  复合类型完整检查与非零稳定 TypeId 仍需贯穿。
- struct字段检查见下节；Tuple已贯通构造/projection/局部及typed参数绑定、
  逐元素Any边界、display、GC和归档。静态Tuple match已接通，动态Any shape及module
  解构仍拒绝；Enum构造/递归模式已接通，newtype、限定类型和trait的完整构造/检查/分派
  也不能仅靠类型元数据存在证明可用。
- driver 已加载 10 个真实 std 源文件（包括 collections.ns、traits.ns、ordering.ns），mod/prelude 使用实际声明与 pub use。
  编译路径关闭 root 类型/native 名称注入，类型与函数来自 std 导入；core/alloc、
  通用包发现仍没有完成；File/Module/Struct/Enum 运行时初始化已接通，见末节。
- 设计 [builtins.md](engine-initialization/builtins.md) 明确 native registration
  不附静态签名，typed API 在 std wrappers 中。保留此边界，完善原生动态检查
  和 typed std 编译/模块导入/prelude/初始化，不能用 native 静态签名替代 std。

字符被降成字符串的问题已修复：现为真实Char immediate，NSBC常量tag9保存Unicode标量。
`type_of(42)` 已返回表示 i64 的 Type 值，std.io 的返回注解已同步。
`Any=true` 传入 i64 固定参数已受检并报 TypeError。
数值提升之外的这些问题尚未因本轮测试通过而完成。

宽整数、非十进制字面量、算术/native/打印与真实 GC 保真路径已补，最大 i64
不再打印为 -1。64-bit 算术可提升到128-bit，超过128-bit仍显式报错；任意精度
整数和固定宽语言类型语义并未完成。已知声明的 optional/default/named 和单 List 变参已生成完整运行时实参；
未知函数值没有声明默认值/变参元数据，仍须完整位置传参。一般方法参数绑定、
双变参与函数 variance 仍缺，不能用已知声明测试代替全部调用语义。

布尔 and/or/not 已实现静态检查、短路与 Any 操作数的运行时布尔检查。
derived Eq/Ord 已修 boxed 数值比较并核对对象布局，复合字段的递归 trait 分派、
完整 derive/hash 与其他 traits 仍需实现。

## GC 完整收集协议

已实现每个 mutator 的运行深度、停顿状态、线程 owner 和 collection epoch，
collector 等待全部运行实例停顿。登记/销毁受 lifecycle reservation 保护，
MMTk bind、flush/on_destroy 在屏障范围内完成；不在 coordinator 锁内回调 MMTk。
普通 poll 在过期请求时直接返回，allocation 可以等待尚未开始的下一轮收集。

根域独立存入 `Box<RootCell(UnsafeCell<VmRoots>)>`，scanner 不借用 Heap 或整块
VmState。公开根容器已封闭，宿主获取拥有的数据快照；push_constant 将分配、
初始化和入池合为一个操作，builtin return_string 直接发布。交给 builtin 的
堆参数保留到 context 退出，即使原寄存器被覆盖也不会提前失去根。

同步 collect_garbage、每个指令边界 poll 和真实 Immix 收集已接通。独立测试
可执行程序使用 32 MB 堆，验证闲置 Engine、VM 移动、70 KB 字符串、暂停的
continuation 帧/slots/closure、连续收集和另一线程的不分配循环；恢复结果 64。
追加验证了 builtin 覆盖最后参数寄存器后收集，以及同线程跨 VM 执行明确报错。

后续修复 delimiter 入口 body 的 capture 环境：ResetClosure 将环境保存到当前
StackContext.entry_closure_env，GC 独立枚举此根，避免覆盖 r0 后失去唯一引用。
LoadCapture 只访问当前 callee 的直接 frame 环境或当前段入口环境，不能从外层
调用帧任取环境；读取前核对 Closure 类型头、payload/count 与捕获索引。
handler/invoke/reset 复用受检布局。新增4个 VM 单测和第7个真实收集回归验证
清空捕获寄存器后仍读到长度8的字符串；合法 artifact 的错误 Unit 结果修为42，
并完成独立验证。该根与访问修复不改变原生 SP/FP 和最后引用模板回收的缺口。

实际收集发现并修复了 worker 空 TLS、Immix 不支持的 in-header mark bits，以及
错误的 heap_size 配置项。使用有效 gc_trigger，mark/LOS side metadata 分离。
对象头保留完整 u16 payload count；过大字符串明确报错，不截断后越界写入。

仍未完成：root pinning 之外的完整移动/stack maps、所有 plan 的验证、通用宿主
HostRoot 和 shadow root API、任意复杂临时对象跨分配证明、外部对象及语言
continuation 最后引用回收。目前暂停段仍作为强根；跨 VM 嵌套执行暂明确拒绝。
这些缺口仍属于整个项目目标，不能把当前收集测试当作完整内存系统验收。

## 结构体字段语义

早期调查发现的字段乱序、缺失/重复/未知/类型检查、前向声明与默认值缺口已修复。
当前按声明字段建立构造计划，显式表达式保留源码求值顺序，省略默认值随后按
字段顺序求值；private/shorthand/alias 与声明作用域已有源码及归档回归。
剩余携带值 enum、newtype、完整 trait/Extend 构造与分派须分别落实。

## 字节码与 CFG 边界

已复现 `try_codegen` 对累计 constant index 的多处 panic：closure/method
metadata 上限 4096，far call 上限 16384，capturing handler 上限 131072，
wide literal load 上限 524288。需要根据实际发射指令验证累计索引，单查 NIR
局部字段或总常量数量不足以保护所有指令。

此外需检查 effect TypeIndex 17 bits、delimiter handler count 5 bits、
builtin ID 14 bits、block ID/target/entry 与 block 表一致。当前 builtin 16384
静默截断为 0；非法 entry 被忽略；非法 block/target 导致越界 panic。

调查时 `JmpFar` 编码误用17-bit mask，且超范围条件分支会丢失条件。后续已
统一 signed 22-bit 编解码并保留条件跳转语义；本轮自包含产物的长距离分支
跨进程回归也已通过。累计常量容量等生产端边界仍需按实际发射路径继续审查，
不能用 artifact 加载前验证代替 codegen 自身的错误传播。

后续 liveness 分析须支持非 SSA 的重复赋值：构造 CFG、use/def、live-in/out
固定点与每条语句 live-after。调用、effect、delimiter、resume 都需保护活跃值；
精确 stack maps 还缺每个 local 的类型元数据。

## NSBC 自包含持久化：已接通与剩余范围

`CompiledArtifact`、`write_artifact` / `read_artifact`、共用 driver 安装器和
CLI `.nsbc` 执行已接通。完整产物保存函数/常量、TypePool、global schema、显式
bootstrap entry 和 builtin ABI/ID/名称清单。低层 `write_archive` 缺少类型池与
入口，仍拒绝非空 globals，不能作为完整产物写出 API。

TypePool snapshot/restore 按原索引保留 types、structural_types、methods、
trait_impls、vtables、well_known 和 null_type；恢复后重建缓存，不重新 intern
类型，不从 kind 猜结构来源。合法名义递归类型可恢复，alias/trait/透明结构环、
坏布局/引用/内建形状与冲突 identity 明确报错。类型池 codec 和函数/方法名称
保存 UTF-8 并在读取后重新 intern，writer 受检查询 StrId。

CODE 采用 section-relative offsets 和 bit1 closure flag；版本化 METADATA
明确完整产物布局与 tagged-root 扫描模式，STACK_MAPS 保存 safepoint PC。方法
重定位覆盖 CallMethod 的名称和 far metadata，并受检调整常量引用、跳转与
safepoint PC；普通 UInt 常量保持原值，DERIVE_FUNC_ID 不按普通函数 ID 处理。
这仍不是精确 liveness/type bitmap stackmap。

容器64MiB上限、60字节 header、真实 offsets/padding 和范围/重叠/重复检查已
落实。writer 计算 header 之后数据的 SHA-256，reader 验证非零 checksum；
完整 artifact 拒绝零 checksum，检查 target 与当前平台兼容。低层容器可检查
旧零 checksum 归档，该兼容路径不允许绕过完整执行验证。

加载前 `validate_artifact` 检查类型池、函数/签名/closure 布局、global/常量/
寄存器/槽引用、指令编码、调用、跳转、方法/vtable 和入口；共用安装器检查
builtin ABI revision 及 runtime ID/名称，验证失败不运行 entry。源码与归档
复用类型池→global schema→函数→常量的安装顺序；归档不重新解析源码或规划
初始化，按已保存 bootstrap 执行。

独立进程回归已在编译后删除源码，仅凭产物执行，覆盖 alias/Type、捕获 closure、
128-bit 数值、derived 方法、EFFECT 和长距离跳转；加载进程先驻留5000个无关
字符串，验证方法等名称不依赖原 StrId。codec 和 artifact 拒绝测试覆盖损坏
UTF-8/count/tag/索引/尾字节、错误 checksum/target 和不兼容 builtin manifest。

后续仍需跨包 imports/exports、稳定 TypeId、链接/重定位、精确 stackmap、
DEBUG_INFO、文本字节码和可选 WASM/压缩。现有 Type 常量的 pool-local index
在完整产物中可跨进程恢复，但不是跨包或跨编译稳定身份。当前自包含路径的
通过不代表整个 NSBC 包系统完成；详见 [实施状态与计划](nsbc-artifact-plan.md)。

## std 真实加载与源码初始化

已落实逐源文件 span、可信节点 builtin 权限、模块成员预声明、实际声明 pub 可见性、
共享 SymbolId 的导入/重导出、导入绑定自身的可见性和完整 qualified Projection。
已知模块缺少成员时直接诊断，不回退到同名 root builtin；用户不能通过命名 std
取得特权。前缀、别名、嵌套选择和 glob 使用同一条名称解析路径。

prelude 由语言源码 pub use 组成，其隐式导入弱于本地和显式绑定。native const
函数值按源码注解生成 CallBuiltin adapter closure，普通函数也可成为值；已知
closure 的 Function metadata 贯穿 NIR/codegen/VM、反射与精确签名检查。无注解
raw native 函数值诊断失败；独立 resolution 的直接 native 调用仍是动态边界。
入口明确来自用户 FileScope 的 main；模块同名 main 不替换它。

端到端覆盖真实 std 导入、qualified 调用、前向成员、别名、重导出、typed 参数、
函数值传递、遮蔽、冲突、非法成员和伪造权限。resolution 单测验证私有成员的
访问规则。pow 暂用 fn(Any, Any) -> Any，保留 native 精确整数行为，完整静态
泛型数值签名仍需实现。非法函数/元组类型注解现在诊断，不丢弃错误成员。

File/Module/Struct/Enum 的普通 const/let/global 值已使用共享 GlobalId；imports
沿用原始 SymbolId，不分配副本。全局类型和可变性 schema 在 VM 启动前安装，
受检读写拒绝未初始化读取、非法索引、错误类型发布与 const 二次写入。lambda
排除 globals 捕获，仍捕获局部变量；函数和闭包观察同一全局槽的最新值。
关联作用域的全局类型与函数元数据在前置准备中解析，避免声明顺序影响 schema
和反射签名。

各已加载作用域的 initializer 按源码顺序执行顶层声明和语句，再调用本作用域
无参数、返回 Unit 的 `__init__`。bootstrap 按依赖执行 initializer，再调用根
main 并保留返回值。未引用模块/类型不执行 hook，初始化失败阻止 main。
直接读取与已知 helper/闭包调用形成值依赖；单纯创建 closure 不执行其 body。
实际跨作用域值依赖环诊断，软 import 环按稳定顺序执行；同作用域前向读取
按源码顺序触发 UninitializedGlobal。常量布尔条件和短路不可达读取不制造环，
动态间接依赖仍由运行时受检读取兜底，不能声称已完成一般依赖分析。
静态 false 的 while body 不计入执行依赖；初始化调用的普通 helper 递归检查
非法外围函数局部捕获，helper 自身参数和局部值可正常使用。

新增回归覆盖 initializer 中的 if/while、带 guard 的 break/continue、循环标签、
顶层非法 return 与错误 hook 签名。非尾分支值不提前结束初始化，显式控制流
保留；无注解 identity lambda 的动态返回边界已修复。GC 集成回归仅保留 global
根，清空寄存器后实际收集并验证字符串长度，覆盖共享状态的存活路径。

仍需实现 Newtype/Impl/Extend 关联作用域、通用包源码发现和缓存、core/alloc、
完整标准库及跨包产物链接。pkg_manager 反向依赖 driver，复用发现器前需调整
依赖方向。NSBC 阶段workspace测试691通过、0失败、1已有忽略，all-targets检查通过；
79个改动Rust文件局部格式通过。当时受影响crate严格Clippy通过，全仓剩25条
已有告警和4个旧格式文件问题；后续参数绑定阶段已清理这些基线问题。原生 SP/FP 切换和 continuation 最后
引用回收仍未完成；测试通过不代表整个项目目标已经落实。

## 参数绑定之后的调查（2026-10-07）

已知声明调用使用 source-order 显式求值、parameter-order 绑定和按需默认值；
callee、receiver、实参及二元运算左值在后续表达式前保存，避免可变局部值被覆盖。
默认表达式按声明作用域解析，允许之前参数，拒绝自身/后续参数和展开循环。
字节码仍收到完整固定参数列表，未知函数值不附加声明默认值。

以下保留参数绑定完成时的调查；struct/plain impl/List/单变参已在后续阶段落实，
不能将这些历史缺口继续当作当前结论：

- struct 构造按源码字段顺序直接装载，缺少字段重排、未知/重复/必填校验及默认值。
  先建立 declaration-order 构造计划和 expected-type 检查，保留实参源序。
- impl scope 未关联目标类型，导致关联方法查找失败；应贯穿预声明、类型准备、
  projection/type(args)、可见性与归档测试。
- ListOf、NewList/NewMap、`__list_init` 尚有 Unit 占位路径，完整产物校验已拒绝
  未支持的容器 opcode。List 必须先打通类型、受检操作、GC tracing、std、归档，
  再做迭代与单变参；Map 需落实 Hash/Eq，之后再做双变参。
- 设计中的 List/Map 原始外部缓冲不能直接使用当前通用 TaggedValue payload 扫描。
  需明确对象布局、元素根、扩容期间临时根、写屏障及容量溢出检查。
- Newtype 名称解析仍缺；相同运行时表示与名义动态类型身份之间的实现约定须单独
  落实，不能以擦除身份或无依据继承 trait 的方式补占位。

这些是调查结果和下一步计划；实际完成情况以新增端到端证据为准。


## 结构体与关联作用域阶段结论（2026-10-07）

上一节的 struct 构造计划和 plain impl 关联命名空间已经实现并验证，不能继续
把这两项列为完全空白。前向类型/alias、shorthand、字段校验与默认值、private
同类型访问、真实self字段、关联值初始化和method FuncId搬迁都有源码/归档证据。
Alias使用同时保留声明模块和canonical type的load边，__init__不会被别名抹掉。
字段/参数默认使用独立defaults模块，共享控制流和展开循环/规模检查。

结构体阶段workspace 732测试通过、0失败、1已有忽略，格式、all-targets check、严格
Clippy及差异检查通过。详细行为与边界见 implementation-progress.md 的新阶段。
动态 List 后续已经贯穿类型角色、受检 runtime/std、GC、显示、单变参和独立归档，
见 implementation-progress.md 的 List 阶段记录。当前 std 实际为8个源码文件，
不再有 ListOf/NewList/索引操作及 __list_init 的 Unit 占位成功路径。

下一步集合工作是 Map 的明确键/Hash/Eq 契约、泛型元素约束、Iterator/IntoIterator
和双变参；一般方法默认绑定、完整trait/Extend/Newtype、跨包链接和continuation
原生 SP/FP/生命周期仍不完整。List 单 buffer 最多65535元素，显示深度128层、
展开1 MiB上限；当前角色与 ABI2/旧ABI1兼容矩阵见 implementation-progress.md。
本轮全仓769测试通过、0失败、1已有doctest忽略；fmt、all-targets check、
strict Clippy和diff检查通过。重复参数回归已修复，self参数不再使用AST默认StrId0作为名称。
List临时根用Drop guard支持unwind清理，新增pop返回前后真实收集回归；
当前gc_collection为11项。上述完整项目目标仍待落实。


## Tuple 阶段状态与下一步（2026-10-07）

Tuple已从静态TypeKind扩展为真实源码路径：construction/numeric projection、
nested let/var与typed fn/lambda参数、逐元素expected/Any检查、整对象受检转换、
List/Tuple统一显示、GC与NSBC独立加载均有具体值或拒绝回归。Module/global
使用单标识符保存typed Tuple正常，解构尚不支持。Any没有静态shape时投影和
解构仍明确诊断；静态known-shape Tuple match/matches已在Enum阶段接通，
完整泛型/trait模式系统未因此完成。

现有Tuple descriptor/原索引恢复/字段字节码足以承载该路径，无新布局或ABI。
具体支持及边界见[type-system tuple](../type-system/data-types/tuple.md)。
本阶段全仓验证为784通过，前面的769是List阶段记录。

## Enum 阶段状态与剩余类型工作（2026-10-07）

无载荷Enum身份碰撞已修复，payload Enum construction/type checks/display/GC/
NSBC已贯通。支持recursive/alias/具名实参与source-order保存；match/matches
支持递归Enum和静态Tuple模式、字面量/绑定/通配符/guard，并检查全部arm。
没有匹配项明确返回NoMatchingCase；Any可用指定Enum variant动态分派，不能
从未知Tuple shape推导投影/解构。独立归档不依赖源码或进程StrId。

当前剩余缺口是Enum Eq/derive：同Enum/tag的payload equality明确返回
UnsupportedEnumEquality，不比较地址或伪造false；不同Enum或不同tag则false。
复杂Or/AsBind、List/struct等模式、完整穷尽性分析、constructor函数值、variant
默认/optional/variadic字段及泛型Enum仍未实现。普通数字/Symbol/Type常量不
重新解释为Enum，新tag8/opcode追加但revision及builtin ABI不变。

Map/Iterator/双变参/Newtype/完整trait、跨包与continuation/GC剩余目标仍待落实。
本轮最终全仓验证814通过、0失败、1已有doctest忽略，strict Clippy/check/fmt/diff
均通过；前面769是List阶段历史记录。`matches`绑定已隔离，match所有分支
类型均受检，Enum本体self方法及独立归档递归调用已接通；成功分支绑定分析、
完整字符值/字符模式仍待落实。


## 基础类型trait之后的当前调查

std源码现为19种基础类型提供Eq、PartialEq、Display，普通函数签名、ABI和
vtable可独立归档；builtin ABI5新增标量比较121，全部1098测试通过。先前
“Enum缺Eq”的段落是历史调查：Eq/PartialEq现已支持Struct/Enum/Tuple派生
真实函数、嵌套字段实现调用、递归/Optional和NSBC；不要重复按旧状态开发。

本次只读probe确认默认trait方法仍有类型具体化缺陷：
`derive fn callback(self)->fn()->Self { ||self }`返回闭包反射仍为`fn()->Read`，
没有成为`fn()->P`；内嵌`fn inner(other:Self)->Self`也保留抽象签名，
内嵌`fn inner()->Type { Self }`对P/Q返回抽象Read。简单调用值42的测试
不能证明函数类型正确。下一步必须保留源Self provenance，生成每default
adapter独立命名函数，不能全量替换与Read相同的TypeIndex。

Ord设计规定`cmp->Ordering`，现旧实现返回整数，operator也按整数零消费；
PartialOrd返回协议尚未规定。相关迁移决策现已由用户明确批准Ordering/?Ordering，NaN返回null。
Enum/Tuple Display尚未实现，现generic显示不调用自定义字段Display；普通
struct字段仍可能显示对象地址，不能简单用TO_STRING wrapper声称派生完整。
后续需生成逐字段普通函数调用、检查全局前置trait、保根/短路及递归预算。


上述默认函数反射probe现已修复：source Self值流与显式上下文路径用于真实
Function专化，内嵌函数每adapter独立生成；具体调用方类型及实际return检查
同步落实。若FnSelf契约返回显式Read闭包，动态TypeError而非伪造具体签名。
验收与边界见[默认方法计划](default-trait-method-plan.md)及最新implementation
progress。此修复未决定trait返回carrier、Self构造或排序协议。


Ordering/?Ordering迁移现已落实：新源码固定接口、parent Eq/PartialEq、普通
std标量实现及Struct/Enum/Tuple/Optional派生完成；真实四默认方法adapter
与用户override通过Ord槽生效。NSBC旧integercmp仍按原指令解释，无reader
metadata修补。后续主要类型/std工作包括Hash、集合关联迭代、Enum/Tuple
Display、Self构造与复杂值流、trait返回/存储；稳定跨包身份和完整async等
原目标也尚未完成。最新全仓证据见implementation-progress。

用户定义trait的静态关联类型现已接通：Type关联声明、具体default/override、
源Item路径与显式Any区分、精确impl/scope绑定、继承实际父实现及NSBC独立执行。
TPOL6保存新关联字段，无相关数据仍写5，旧reader路径不补协议。动态关联trait
视图、依赖Self/关联值的默认类型和关联trait默认方法仍明确拒绝；bootstrap
Iterator/IntoIterator及for协议未改变，不能将静态Stream.next调用等同于完整
集合迭代。Char literal模式已接通Unicode/转义、Enum/Tuple载荷、Any与归档。


Enum/Tuple Display的上述缺口现已完成：普通逐字段调用、全局前置条件、字段
方法模块初始化、引号规则、帧所有递归状态、真实GC/多次恢复和独立归档均通过
回归。用户选择暂时保留struct旧派生，不新增其字段Display要求。NSAM6保存
受检Display owner，builtin ABI7新增三个helper；旧产物不补造新语义。Hash、
关联迭代、trait返回/存储、跨包稳定身份及完整async等仍需继续实现；当前验证
及String物理大小限制见implementation-progress与派生文档。


Self/其它关联绑定的静态默认类型已完成具体专化：模板保留源引用，override先
求值并可打断循环，继承读取精确父实现绑定。TPOL7保存符号声明叶和默认树，
旧1–6不补造；抽象符号不能进入执行类型及Type值。真实GC、多次恢复、源码删除
后的归档及作用域反射均有回归。此完成不包含关联trait默认方法body或动态
carrier，也没有迁移Iterator/for协议，后续缺口仍按最新implementation-progress。

关联 trait 的默认方法体已接通逐实现检查与独立 adapter：Item 局部类型、Type
值、cast、闭包与命名函数使用精确绑定；字段布局和调用 facts 按方法体隔离。
静态错误停止产物生成，显式 Any 保持实际返回检查。源码 Self 与 Item 即使
具体类型相同仍使用不同 ABI，内部证明保持冻结根实现及继承覆盖。bare 动态
关联视图继续拒绝，不决定 trait 返回/存储 carrier。当前阶段验证见最新
implementation-progress；Iterator/for、Hash 和其余原目标仍需继续完成。

初始化的简单 receiver 别名链现已按原冻结证明扫描，修复未调用 provider 引入
的假循环；receiver 或别名写入时失效。精确独立 Self 参数数据流仍待完成。
用户已选择 Iterator 单次 next 与带标签结果，见 tagged-iterator-plan，不能
继续按 Optional null 结束或禁用 List null 元素来替代该目标。该迭代协议尚未
实现，现有旧 source/NSBC 语义未在本阶段迁移。
