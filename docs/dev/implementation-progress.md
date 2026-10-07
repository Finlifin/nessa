# Nessa 实现进展与完成条件

目标是实现现行语言与引擎设计，并采用 delimiter 边界栈切换的 continuation ABI。
代码质量、结构一致性与可验证的语义是完成条件。现阶段整个项目尚未完成。
本记录以代码与验证命令为依据，不能用测试全绿推断未覆盖的设计已实现。

## 已落实的 continuation 基础

- `runtime::TaskStacks` 持有根栈、delimiter 栈、暂停链和独立分支；当前执行字段
  属于活动栈段，捕获/恢复修改链接，段内帧不移动。
- `RESET/SHIFT/RESUME` 已实现解释器执行；`CLONE_CONTINUATION` 显式建立独立分支，
  `DROP_CONTINUATION` 丢弃暂停链。全部段随完成、取消或 task 销毁释放。
- GC 根枚举包含 task 的全部活动和暂停上下文。已返回的段回收元数据槽，
  内部 continuation 句柄不重复分配，旧句柄不会别名到后续栈段。
- NSBC v3 定义该控制 ABI；archive reader 拒绝 v1/v2 和未知版本。
- 验证包括上下文/帧地址与栈槽稳定性、最近 prompt、嵌套 prompt、普通调用帧恢复、
  多分支结果、丢弃与取消释放、跨 task 句柄拒绝、栈池跨线程分配和版本拒绝。

实现协议与限制见 [continuation-stack-abi.md](continuation-stack-abi.md)。
该基础最初由直接字节码测试验证；源码 handler 与语言对象的后续进展见下文。
原生 SP 切换仍未实现。

## 本轮验证记录（2026-10-06）

- `cargo check --workspace --all-targets --offline` 通过。
- `cargo test --workspace --offline` 通过：424 个测试通过，0 个失败，1 个已有
  doctest 被忽略。语言 continuation 的源码路径尚未被这些测试证明。
- 修改过的 Rust 文件通过局部 `rustfmt --check`；`git diff --check` 通过。
- `cargo fmt --all -- --check` 未通过：11 个其他现有文件仍有格式差异。
- `cargo clippy --workspace --all-targets --offline -- -D warnings` 未通过：
  当前首先报告原有栈池信号处理表达式/函数指针转换、诊断 API 的 Safety 文档和
  type_pool 嵌套条件等问题。质量基线仍须清理，没有将这些检查声明为通过。

## 后续落实：源码签名与编译失败路径

- effect 参数新增独立的 `ParamCatch` AST 节点；解析器只在效应声明中接受它。
  类型池新增 `Continuation` intrinsic，效应 metadata 记录 continuation 参数位置，
  不把该参数计入调用者参数；普通参数和返回类型使用实际 TypeIndex。
- `effect_tests` 验证 typed/implicit catch、普通和 async 效应签名，以及重复 catch、
  错误 catch 类型、不完整声明和函数参数中使用 catch 的拒绝行为。
- driver 收集词法、语法和解析/类型阶段的诊断，发生错误时停止后续阶段，返回
  空产物和错误状态。build 在编译失败时、AST dump 在词法或语法失败时不会创建
  或覆盖目标文件；合法 AST 仍可用于检查语义尚未解析的源码。
- 修复 SourceMap 按固定文件名复用首次源码的问题，连续编译使用独立的源码身份，
  回归测试验证每次诊断能准确定位到各自标识符。
- 诊断读取改用带借用守卫的安全 API，移除从 RefCell 临时借用中 transmute 切片的代码。
- 清理 lambda 捕获分析中的恒假空分支并说明嵌套捕获的实际行为；浮点往返测试
  使用标准 PI 常量。普通 workspace Clippy 可以完成，严格 `-D warnings` 仍因
  原有告警失败；没有添加 lint 豁免或弱化测试断言。
- workspace 编译与测试通过；最新共 431 个测试通过、0 个失败、1 个已有 doctest
  忽略。CLI 手动验证 check/build/run 拒绝未定义名称并保留已有产物，正常源码
  仍能运行并打印 `ok`。格式和严格 lint 的历史缺口仍在。

这一步完成源码声明和签名的基础；源码执行路径在下述阶段继续接通。

## 源码 handler 与 continuation 执行（2026-10-07）

- 名称解析为 handler 建立独立参数作用域，检查效应目标、重复 arm 与参数数量；
  绑定类型来自已解析的效应签名。普通效应参数不包含运行时提供的 catch 参数。
- NIR 为 handler 建立可捕获周围值的闭包，codegen 生成动态分派和 delimiter
  闭包调用；无 catch 的 handler 在当前上下文调用，可捕获的消除块建立独立栈段。
  同一消除块可同时为多个可捕获效应建立 prompt。
- 语言层使用带类型头的 Continuation 堆对象。调用保留多次恢复语义，显式
  `clone()` 建立独立模板；延迟恢复携带原来的可见 handler 环境，恢复后可以
  再次捕获。环境快照仅保留每个效应最近的绑定，避免嵌套时重复累积。
- 新捕获的 catch 参数在整个 body 中恰好引用一次，且唯一引用为直接尾调用时，
  编译器生成 `RESUME_CONTINUATION_ONCE`，直接重接原链，无需 fork。普通
  Continuation 参数、别名、逃逸、clone、嵌套闭包及多次调用不使用此优化。
- 修复 `return/resume value if condition` 忽略 guard 的问题：先求 guard，
  条件为假时不求 value；无条件返回后不再覆盖已生成的 terminator。
- 修复实例方法被当成全局名称解析、方法调用返回类型未传播的问题，支持
  `Continuation.clone()` 的类型与参数检查。超过当前 effect/handler 编码容量
  的源码在 resolution 阶段明确诊断，不进入会断言失败的 codegen 路径。
- 新指令字段的边界编码/解码、最近 handler、in-place 返回、闭包捕获、
  多 prompt、多次/延迟恢复、再次捕获、clone、放弃恢复、单次证明正反案例、
  guard 正反路径与错误输入均有测试。具体总量与质量检查见最新验证记录。

尚未完成完整的效应集检查、continuation 输入与 answer 类型推导、静态 evidence
passing、handler 参数解构、async handler、活跃区间与寄存器优化、语言对象最后
引用消失时的模板回收和原生 SP/FP 切换。多次调用路径仍复制分支状态；不能将
单次使用优化或现有测试等同于这些设计目标已经全部完成。

## 后续调查与即时修复（2026-10-07）

按用户授权，并行只读调查寄存器分配、continuation GC 生命周期与质量基线。
调查确认的具体缺陷已形成后续实施依据：

- 修复常量操作数共用 r16 的问题：原 `1 + 2` 实际返回 4，现二元操作分别
  使用 r16/r17；回归断言加减乘除、取模及比较的具体结果。
- in-place handler 的结果现在按效应返回类型推导并检查，拒绝将 bool 作为
  `effect ask() -> i64` 的返回值；可捕获 handler 的 answer 类型另行处理。
- Continuation payload 原裸句柄 8/16 等会被 GC 当作指针。现在保存 tagged UInt
  并读取时校验类型；边界句柄往返与无效 payload 有回归测试。
- GC 对 Str/Continuation 不扫描 scalar payload，Closure 跳过原始 FuncId/count，
  只扫描捕获值；直接扫描测试检查实际被访问的槽位。其余对象布局与真实 STW
  的全流程仍需要更完整验证，不能由这些局部测试推断 GC 全部正确。
- 调查时的寄存器缺陷有实测复现：30 个 local 后返回 x0 得到 26；15 个
  local 跨调用后 x12 从 12 变成 112。应增加 context/frame 自有 TaggedValue
  slots 和 slot 指令，先保守 spill 再实现 CFG liveness；不能继续 modulo 分配。
- 语言模板回收需要区分 ABI 显式根与语言弱所有权，并通过 GC 可达性固定点
  展开可达模板。仅加 finalizer 无法收集被无条件 root 扫描保留的孤立模板环。
  新根协议、移动地址更新、mutator 安全点与取消清理需要一起设计和验证。
- stack_pool 信号 handler 内持有 Mutex，存在异步信号死锁；全局只记录首个
  pool 也与多个 Engine 不一致。必须单独做安全机制调整，不直接照搬 lint
  建议扩大 static mut 引用。

集成测试共享 `tests/common` 执行辅助函数，避免复制初始化与结果断言管线。

## Frame slots 与调用正确性（2026-10-07）

- 删除永久取模寄存器分配，NIR local 使用函数自有 TaggedValue 槽。普通调用
  移动槽的所有权到 CallFrame，返回时移回；RootStackContext 与保存帧的槽都
  进入 GC 根枚举。任务结束释放这些槽的分配。
- 捕获与恢复保持当前/保存帧槽的 payload 地址，fork 建立独立副本；地址检查
  和分支修改回归验证原始模板未被分支修改。每个 delimiter 仍持有独立栈段。
- 新增 `ALLOCATE_SLOTS`、`LOAD_SLOT`、`STORE_SLOT`，槽索引为 17 位，越界
  读写返回 VM 错误。宽索引往返与 70000 号槽的实际读写已验证。
- 40 个 local、跨调用保留值、25 个参数、31 captures 加一个参数、递归、
  循环赋值和多次 continuation 分支均有具体结果回归。此前 x0/x12 错误已消除。
- 间接闭包调用检查对象类型与参数数量，在安装捕获参数前保存 caller；handler
  和普通 closure 共享此调用路径。当前 VM 参数窗口仍为 32 寄存器，尚须完成
  设计中的 8 寄存器与额外栈参数 ABI。保守槽布局也不是最终的 liveness 分配器。
- Driver 使用 checked codegen，诊断参数/capture/slot 与部分指令字段的编码
  容量，并拒绝越界 NIR local。超过 12-bit TypeIndex 的合法源码现在诊断失败，
  不再触发编码断言；更宽指令与完整编码容量验证仍需继续。
- 调查发现对象固定分配八个字段的越界风险，已改为按 Struct/Tuple 字段数量
  分配并初始化；字段访问检查 payload 长度。12 字段对象和越界读写有回归。
  源码属性按名字映射声明字段仍需修复，不能将该测试视为构造语义全部完成。
- 修复 Yield 仅改状态却丢失 ready_queue 入队的问题，Running task 现在入队一次。
  真正 GC 请求、STW、安全点和稳定的多 Engine/mutator 根登记仍未完整闭环；
  后续 GC 测试必须检查实际完成收集，不能只依赖扫描器局部测试。

本轮并行调查进一步核对了宽参数、closure 捕获与对象路径，并明确下一阶段 GC
的根登记、mutator 注册与主动收集流程；没有把只读调查当作已实现功能。

### GC 登记基础（上一阶段记录）

- 用独立 TLS token 和 mutator 注册表替代“最后一个 mutator”全局指针。
  ActivePlan 按身份查找、枚举全部实例；销毁只注销所属实例，MMTk flush 后
  由 Heap 持有的 Box 回收绑定内存。收集 snapshot 期间冻结登记/注销，避免
  Heap 销毁释放 snapshot 中的指针。
- VM 状态存于稳定 Box，独立 RAII 根登记替代全局 VM 指针。移动 VM 不改
  根地址，闲置 Engine 仍被登记，销毁先移除根源。scanner 遍历所有根源，
  root registry 锁保证枚举期间 context 不被释放。
- 回归覆盖多个 Heap 身份/指针、独立销毁、跨线程移动后的销毁、冻结期间
  保留 mutator，以及根源随 owner 移动和独立注销。没有执行真实收集；这些
  测试不能代替多 mutator STW、对象移动和 continuation 回收验证。
- 完整停顿协议仍未完成：旧单个 `all_stopped` 不足以证明全部 mutator
  停顿，宿主公开字段访问尚无 operation guard，初始化和入根也需受同一个
  Running 操作保护。下一步补状态/epoch/shadow roots，再验证主动收集。
- 三个 subagent 的后续只读调查已完成，复现出的字段语义、编码/CFG 边界
  与完整 GC 约束记录于 [后续实现调查](upcoming-work-audit.md)。

### 多 mutator 停顿与真实收集

- 以逐 mutator 的运行深度/停顿状态/线程 owner 和收集 epoch 替代旧单个
  all_stopped。每个指令边界 poll；过期请求直接返回，allocation 可先阻塞再
  等下一轮。GC 后继续执行，抢占才 yield。登记/销毁的 lifecycle reservation
  覆盖 bind、flush/on_destroy，避免退出登记表后又和收集争用 allocator。
- 根域独立为稳定 UnsafeCell，仅含 globals/constants/scheduler/临时根；scanner
  不再构造含 Heap 的整块 VmState 引用。公开容器访问已封闭，返回拥有的快照。
  字符串常量与 builtin 返回原子发布；交给 builtin 的堆参数保留到 context 退出。
  同线程跨 VM 执行/分配/收集返回 CrossVmOperation，避免等待父 mutator 死锁。
- 接通同步主动收集。独立进程的 32 MB Immix 测试验证闲置/移动 Engine 的常量、
  70 KB 字符串、暂停 continuation 的帧/slots、closure 函数/count 均为8的布局，
  以及另一线程的不分配循环和连续收集。捕获段恢复后结果为64，栈池归零。
  追加 builtin 覆盖最后参数寄存器后的收集，以及退休屏障和跨 VM 错误回归。
- 真收集揭示并修复 worker 空 TLS、Immix 不支持 in-header mark、mark/LOS side
  metadata 布局及无效 heap_size option。header 现在保存完整 u16 payload count；
  字符串检查类型和容量，超大对象与 BigInt 常量显式报错，不返回伪造 Unit。
- delimiter 入口 body 的闭包环境现在保存在 StackContext.entry_closure_env，
  ResetClosure 安装该环境，GC 枚举此根。LoadCapture 只读取当前 callee 的直接
  frame 环境或当前栈段入口环境，不回退到外层任意 closure；共享校验检查
  Closure 类型头、payload/count 与捕获索引，handler/invoke/reset 沿用同一布局。
  新增4个 VM 单测和第7个真实 GC 回归：body 覆盖保存 capture 的 r0 后收集，
  LoadCapture 仍恢复长度8的字符串。此前合法 artifact 的结果由错误 Unit 修为
  42，并独立验证；这项修复不是原生 SP/FP 切换或最后引用模板回收的实现。
- 当前只证明所测 Immix/pinning 路径，不能据此宣称移动 slot 写回、所有 plan、
  HostRoot、通用 shadow API、最后引用 continuation 回收或完整内存系统完成。

### 按用户优先级推进类型系统、builtin/std 与持久化

- 用户将类型系统、builtin/std 提到主线优先级，其后为 NSBC 持久化；原项目目标
  和 continuation 的 delimiter 独立栈 ABI 要求保持不变。
- 新 numeric 模块按整数位宽/符号范围计算提升，修复 u64→i8/i64 和 signed→unsigned
  被误接受的问题；pointer-width 整数按目标 Rust 架构宽度参与判断。覆盖全整数
  类型配对的范围/对称性，以及源码函数返回的接受/拒绝回归。算术转换、
  完整 Any 边界、复合类型描述与数值语义尚需继续，并非类型系统全部完成。
- 调查确认 std 未实际加载；native registration 按设计保留动态边界，typed API
  应由真正编译/导入的 std wrappers 提供。后续已接通真实 std 与 NSBC 自包含
  产物，见本文相应记录；raw section 往返仍不能代替完整加载执行。具体缺口记录于
  [后续实现调查](upcoming-work-audit.md)。

### 声明与整数字面量的上下文检查

- 声明先解析 annotation，再按 expected type 推导 initializer；`let x: i8 = 42`
  等声明能正确接受。整数字面量统一解析 magnitude，按目标类型检查范围，
  包含负数最小值、u128/i128 边界、binary/octal/hex 和类型别名。
- 无返回标注的普通固定参数函数从定义读取参数类型；定义顺序不再绕过参数
  类型、数量和字面量范围检查。optional/named/variadic 完整绑定仍需实现。
- 为旧手工 AST 测试补真实的全局 interned 字面量文本，并断言 NIR 返回 42；
  之前没有 literal text 的 fixture 不再被当作有效整数字面量。
- 端到端测试覆盖小整数声明返回值；宽整数和非十进制边界的接受测试仅证明
  这一阶段的静态范围检查；后续数值执行进展见下节。

### 精确数值表示与布尔逻辑

- 整数字面量共用 radix parser，表达式和 pattern lowering 删除回零路径；
  signed minimum 直接折叠。NIR/NSBC 常量保留 i128/u128，包括小值 42。
  小 unsigned 也保留 unsigned 编码，不再经过 signed LoadImm。
- VM numeric 模块将超出 57-bit 范围的 i64/u64 装入单字 payload，i128/u128
  始终装入两字 payload；f64 只有所有位能无损压缩时才使用立即数，否则装箱。
  GC 跳过数值 scalar payload，不把对齐的数值位误认作引用。
- VM 算术/位运算/比较、native 转换/数学/打印和 owned scalar snapshots 使用
  统一 Number 解码。64-bit 算术溢出可提升到 128-bit；超过当前 128-bit 实现
  返回 NumericOverflow，零除返回 DivisionByZero，不返回 Unit 或截断结果。
  固定宽类型完整语义和任意精度整数仍需继续；显式转换的后续进展见下节。
- derived Eq/Ord 读取实际数值而非 heap address；读取结构体字段前核对对象
  类型和 payload 长度。数值/布尔/字符/字符串字段可比较，其他字段的递归
  trait 分派与完整 derives 仍未完成，不能把这些回归当作完整 trait 支持。
- 布尔 and/or 在 NIR 构建短路 CFG；not 使用 bool 比较。类型检查拒绝静态
  非布尔操作数；测试覆盖嵌套、跳过异常 RHS、必要 RHS 必须执行。
- NSBC v3 增加 Int128/UInt128 tags 与受检 CONSTANTS decoder，验证边界往返、
  UTF-8、长度、unknown tag 和尾随数据。完整产物的 metadata 与独立加载
  后续已接通，见 NSBC 自包含执行记录；任意精度整数仍未实现。
- 源码端到端断言 i64/u64 极值、i128/u128、radix、溢出提升、算术/比较/native
  返回值与 f64 bits。真实 GC 增加宽整数、subnormal、NaN payload 保真回归。

### 显式转换与运行时类型操作

- `.as(T)` 已贯穿 parser/resolution/NIR/codegen/VM；目标必须是类型表达式。
  解析源值与目标类型分离，`300.as(i8)` 留给受检转换，不能先把300推成合法i8。
- TypeCheck 不再固定返回 true；根据立即数种类或堆 TypeIndex 判断，支持类型
  别名、Optional/null、effect/error限定类型和直接 trait impl。数值 widening
  沿用当前 subtype 规则，非法 TypeIndex/别名循环报告 InvalidType。
- TypeCast 的整数转换覆盖所有固定宽度及平台整数，浮点到整数先截向零再查
  范围；f32显式舍入，有限值超出f32范围拒绝。失败报告 TypeError/NumericOverflow。
  TypeCastSafe 的值转换失败返回null，元数据错误和分配失败继续报告错误。
- 显式窄整数/f32转换暂时装箱以保留具体目标 TypeIndex；统一 Number decoder
  检查这些单字payload的值域/精度。真实GC测试覆盖这些转换产生的对象。
- 编译期 coercion 贯穿声明、赋值、固定参数、函数/lambda 返回及布尔条件；
  Any 边界生成 TypeAssert，数值拓宽生成 TypeCast，禁止隐式浮点截断。窄类型
  literal/算术结果经转换保留身份。间接调用在入口受检，先保存全部参数作为 GC 根。
- 未标返回类型的函数具有前向保守 Any 签名，body 完成后合并显式返回与末值；
  数值共同返回类型落实到各产值点，异类返回保守为 Any。非末位分支与循环
  丢弃普通表达式值但保留显式 return。
- 完整 Function/复合类型描述、字段构造检查、optional/named/variadic 参数 ABI、
  完整函数反射与稳定 TypeId 尚未完成，不能把当前边界覆盖当作完整 gradual typing。

## Type 值与反射（2026-10-07）

- 类型名值上下文具有 Type 类型，与类型标注表示的身份分开；NIR ConstType
  经 NSBC Constant::Type 和受检 VM 工厂装载，immediate subtag 8 不进入 GC。
- `type_of` 与 `x'type` 共用动态类型查询；Type 值的动态类型为 Type，透明
  alias 等同目标，窄数值/宽数值及 struct 读取实际身份。null 为 `?NoReturn`。
- Type 可赋值、传参、返回、闭包捕获、比较和显示，Type 参数的 Any 边界受检；
  用户用整数伪造 Type 或调用 Type 值会诊断。字符串比较使用内容。
- 结构类型按 canonical shape 驻留，名义类型不按名字合并；effect 声明保持
  独立操作身份，避免同签名 handler 串接。无效 descriptor/alias cycle 明确报错。
- NSBC v3 常量 tag 7 保存 u32 pool-local index，读取/写入拒绝 INVALID 和截断；
  越界由 VM 装载检查。完整产物后续保存 TypePool 并受检恢复这些索引，
  已实现方法名称重定位；跨包 TypeIndex 重定位与稳定 TypeId 仍需落实。
  已知 closure 签名反射见下节，单独 Type 常量往返不代表完整持久化。

## 真实 std、导入与函数值（2026-10-07）

- driver 编译实际 std/mod.ns 与六个模块源文件，逐文件保存 SourceMap 身份与 span；
  只将可信 std 节点授予 builtin view 权限，用户模块命名 std 不会取得权限。
- 用户编译关闭 root 类型/native 名称注入，通过真实 std.prelude 的 pub use 取得
  API。模块直接声明预收集，导入/重导出共享原 SymbolId，导入可见性独立保存；
  qualified 类型/函数、parent/package 前缀、别名、选择与 glob 已接通。
- 隐式 prelude 允许本地/显式绑定遮蔽；冲突或不存在成员诊断。入口来自用户
  FileScope 的 main，模块同名函数或导入 main 不替换入口。
- FnType/Arrow 注解使用源码参数与返回类型；非法注解不回退推导，元组类型不
  丢弃非法成员。结构/前向 alias 在函数签名前完成解析，循环/非法目标诊断；
  透明 alias 参与 subtype/gradual 检查，trait alias 可用于 derive。
- native 函数值使用源码签名生成 adapter closure，参数在 codegen 入口受检，
  native 返回按声明签名检查。普通函数可传递为值、闭包捕获及间接调用。
- 同步/async effect 类型保留各自身份；结构签名仅从驻留索引集合复用，
  不复用具名 effect 操作的 nominal 索引，避免反射签名影响 handler 身份。
- Function metadata 贯穿 NIR、CompiledFunction 和运行时 FunctionCode；已知
  closure 反射为 Function 签名，动态函数边界精确匹配签名并核对 capture 布局。
  手工无 metadata closure 仍反射为 intrinsic Closure，不伪造具体签名。
- 新 std 端到端测试及 resolution/VM 单测检查正常与错误路径。std.pow 暂用
  Any 参数/返回保留整数运算；泛型数值签名、函数 variance/适配仍未实现。

模块顶层值初始化与 globals/bootstrap 已接通，具体边界见下文初始化记录。
通用包发现、core/alloc、完整标准库和包缓存仍有缺口。
Function metadata、TypePool、全局 schema 与入口后续已写入完整 NSBC artifact，
并通过独立加载验证；容器边界和持久化的具体范围见下面记录。跨包链接、稳定
TypeId 与完整标准库仍不能由这一路径推断为完成。

## NSBC 受检归档容器（2026-10-07）

- 文件头按设计使用 u16 target_arch、u16 target_os 和 u32 flags，实际编码统一
  为60字节。沿用 VERSION 3；此前56字节 writer 是不符合该布局的实现缺陷。
- writer 写实际 section offsets 与8字节 padding；reader 按 header/table/section
  offsets 取数据，支持重定位 table 和乱序 section。
- 在分配 section payload 前验证整个输入，拒绝越界、算术溢出、重叠、重复、
  未知 kind、非法 alignment、压缩及未知 flags。当前最大归档64MiB、最多1024
  entries，具名限制公开；不是任意大产物支持。
- 回归覆盖真实字段字节、偏移/填充、乱序读取和损坏输入；归档 crates 的严格
  Clippy、测试和 driver 消费者检查通过。

容器与常量路径之后已补 checksum、执行 target 检查、完整 metadata 和独立
加载器，见下节。低层归档 API 仍不携带完整产物，不能以其 section 往返代替
可执行持久化验收。精确 stackmap 尚未完成。

## NSBC 自包含产物与独立执行（2026-10-07）

- `CompiledArtifact` 包含 CodegenOutput、完整 TypePool、显式 entry，以及
  builtin ABI revision 和 ID/名称清单。完整 `write_artifact` / `read_artifact`
  保存并恢复 globals；低层 `write_archive` 没有类型池/入口，仍拒绝非空 globals。
- TypePool snapshot/restore 保留原 TypeIndex 和结构驻留来源，涵盖 types、
  methods、trait_impls、vtables、well_known 与 null_type，恢复后重建缓存。
  名义 effect 不按同形状合并；ZERO TypeId 不合并类型，非零冲突受检拒绝。
  合法名义递归接受，alias/trait/透明结构环、坏布局/引用与内建形状拒绝。
- 类型池所有 TypeKind 与函数/类型/字段/variant/method 名称使用受检 UTF-8
  持久化，加载重新 intern。CODE 采用 section-relative offset 和 bit1 closure
  flag，METADATA 保存函数签名、global 类型/可变性、入口与版本化扫描模式。
- 方法名重定位覆盖近/远调用来源，产物使用可重定位的 far 形式；变换受检调整
  常量引用、跳转与 safepoint PC，不把普通 UInt 重写成 StrId。派生方法的
  DERIVE_FUNC_ID 保持合法 sentinel。STACK_MAPS 保存 safepoint PC，运行仍
  扫描 TaggedValue 根，没有把它称作精确 liveness/type bitmap stackmap。
- writer 计算 header 后全部数据的 SHA-256，reader 验证非零 checksum；完整
  artifact 拒绝零 checksum，并校验 target 与当前平台。低层容器保留旧零
  checksum 文件的检查兼容，不因此允许缺 metadata 的旧归档执行。
- 加载前验证类型池、函数 ID/签名、closure 捕获布局、参数/寄存器/槽容量、
  常量/Type/global 索引、指令编码、跳转/调用目标、方法/vtable 与入口；共用
  安装器核对 runtime builtin ABI/ID/名称，不持久化函数指针或补重复 native 签名。
- 源码和归档复用 `driver::install_artifact`，依次装载类型池、global schema、
  函数和常量，再执行保存的 bootstrap。CLI build 写完整产物，`run .nsbc`
  按二进制加载；不再次解析源码或规划模块初始化。
- 跨进程回归在编译后删除源码，仅凭产物执行，覆盖 alias/Type、捕获 closure、
  128-bit 数值、derived 方法、EFFECT 和长距离跳转。加载进程先驻留5000个
  无关字符串，验证方法等名称与原进程 StrId 无关。损坏数据与不兼容 manifest
  有拒绝回归；driver/CLI 的相关9项测试已通过，其中包含独立进程 child harness。
- 本阶段尚未生成默认实参；后续参数绑定阶段已补已知声明的 optional/default/named，
  仍使用完整固定参数的字节码布局，不改变归档格式。variadic 与函数值默认元数据尚缺。

本轮完成自包含归档执行路径，仍未完成跨包 imports/exports、链接/TypeIndex
重定位、稳定 TypeId、精确 stackmap、DEBUG_INFO、文本字节码及可选 WASM/压缩。
原生 SP/FP continuation 切换与最后引用模板回收仍有缺口。详细边界见
[nsbc-artifact-plan.md](nsbc-artifact-plan.md)。

## 共享全局状态与源码启动（2026-10-07）

- 按原始 SymbolId 分配唯一 GlobalId，导入、重导出与别名共享槽。内存编译产物
  保存全局类型/可变性 schema 和显式 entry；VM 执行前安装 schema，拒绝非法
  类型、槽索引、未初始化读取、const 二次写入和类型不兼容的发布。
- File/Module/Struct/Enum 作用域可拥有共享值与 `__init__`。生成的 initializer
  按该作用域源码顺序执行值声明和顶层语句，再调用无参数、返回 Unit 的 hook。
  bootstrap 按依赖调用已加载作用域的 initializer，最后调用用户根文件 main，
  保留其返回值；初始化错误阻止后续 hook/main 执行。
  关联作用域的全局类型与函数元数据也在前置准备中解析，避免源码声明顺序
  让 schema 或函数反射丢失类型。
- import/reference 确定加载集合，未引用模块或类型的 hook 不执行。初始化依赖
  扫描直接全局读取、已知被调用的函数及闭包；闭包创建不执行其 body。软导入环
  采用稳定顺序，实际值初始化依赖环诊断。同作用域前向读取保留源码顺序，在
  运行时明确报 UninitializedGlobal；不能用 Unit/null 伪装已初始化。
- 依赖分析排除可确定的布尔短路与常量条件不可达分支；并非完整动态依赖证明。
  静态 false 的 while body 不计入执行依赖；初始化调用的普通 helper 递归检查
  启动期非法局部捕获，允许 helper 自身参数和局部值。
  普通函数和 lambda 读取同一全局槽，捕获列表只保留局部变量，不复制全局旧值。
  已初始化 globals 参与 GC 根扫描，真实收集回归覆盖仅由 global 持有的字符串。
- initializer 中的分支、循环、break/continue、guard 和循环标签已有端到端回归；
  非尾位置的分支值不提前返回，显式控制流仍生效。顶层 return、循环外控制语句
  及非布尔 guard 诊断。无注解 identity lambda 的结果保持 Any 动态边界。

验证依据是 `module_initialization_tests`、VM globals 单测和真实 `gc_collection`
回归。Newtype/Impl/Extend 关联作用域初始化尚未完成；动态间接调用的依赖仍依赖
受检读取兜底，不能据此宣称完整模块/类型系统或标准库完成。完整 artifact
已保存 global schema 与入口；低层 `write_archive` 仍明确拒绝 globals。

## 上一阶段验证记录（2026-10-07，NSBC 自包含执行之前）

- `cargo check --workspace --all-targets --offline` 通过。
- `cargo test --workspace --offline` 通过：635 个测试通过，0 个失败，1 个已有
  doctest 忽略；`effect_tests` 28个、`standard_library_tests` 6个聚合端到端测试，
  `gc_collection` 6个真实收集/并发回归。
- 68个修改/新增 Rust 文件按 `skip_children=true` 局部 rustfmt 检查通过，避免
  递归格式化旧文件；
  `git diff --check` 通过。
- 普通 workspace Clippy 完成，剩25条已有位置的告警；typing/NIR、归档
  容器及本轮新增文件没有告警。严格 `-D warnings` 仍先在原有 stack_pool/lexer
  问题处失败，未用 lint 豁免取得通过。
- 全仓 `cargo fmt --all -- --check` 仍失败，剩4个既存文件：lexer/lexer.rs、
  parser/basic.rs、parser/pattern.rs、pkg_manager/lib.rs。
  本轮因修改效应类型而格式化的 parser/expr.rs 已通过。

以上数目属于上一阶段，不作为本轮全仓结果。

## NSBC 阶段验证（2026-10-07，参数绑定之前）

- workspace all-targets 编译检查通过；全仓测试691通过、0失败、1已有doctest忽略。
- CLI 归档9项测试（含子进程辅助入口）、真实GC 7项回归通过；覆盖删除源码后的
  跨进程执行、5000个已有StrId、长条件分支，以及delimiter环境的捕获读取。
- 79个修改/新增Rust文件的局部rustfmt与`git diff --check`通过。
- 本轮受影响crate的严格Clippy通过。普通workspace Clippy通过但剩25条已有定位
  告警；全仓严格Clippy仍先在stack_pool/lexer的已有告警处失败。
- 全仓格式检查仍失败于4个旧文件：lexer/lexer.rs、parser/basic.rs、
  parser/pattern.rs、pkg_manager/lib.rs；未以关闭lint或跳过检查获得通过。

项目目标仍未完成。已验证 frame slots、独立 continuation 链切换、真实 Immix
收集和自包含 NSBC 执行路径。接下来继续类型和值语义、builtin/std 与关联
作用域初始化，以及跨包持久化、原生栈切换、GC 所有权、async/FFI 和现行设计
其余部分，不以现有测试数量代表整个项目完成。

## 完整目标审计范围

| 领域 | 规范依据 | 完成所需证据 / 已确认缺口 |
| --- | --- | --- |
| 词法与语法 | `docs/grammar/` | 按每种语法验证接受、拒绝与错误恢复；现有 lexer/parser 测试只是起点 |
| 类型与渐进式类型 | `docs/type-system/` | 数值cast、Any检查、具名struct构造/默认字段已验证；泛型、限定类型、其余数据类型和完整trait仍需落实 |
| 函数与闭包 | `docs/function-and-lambda/` | 已知声明的 optional/default/named 与单 List 变参已贯穿源码和归档；函数值默认元数据、一般方法参数绑定与双变参仍需实现 |
| 控制流与模式匹配 | `docs/control-flow-and-pattern-matching/` | 布尔短路已有端到端证据；分支、循环、复合匹配绑定与动态检查仍需完整覆盖 |
| 代数效应 | `docs/algebraic-effect-and-multi-prompt-delimited-continuation/` | 源码动态 handler 与 in-place 调用已验证；静态 evidence passing、完整效应检查与 async 尚需落实 |
| Continuation | 同上及 `continuation-stack-abi.md` | 源码捕获、多次/延迟恢复、clone 与单次优化已有证据；完整类型推导、最后引用 GC 生命周期与原生栈切换仍有缺口 |
| 结构化并发与 async | `docs/concurrency-and-async-algebraic-effect/` | task tree、取消、join、worker 调度与异步 I/O；scheduler 当前是简化的单线程实现 |
| 包与模块 | `docs/code-orgnization/` | use、可见性和 File/Module/Struct/Enum 源码初始化已接通；plain Impl关联初始化已接通；通用包发现、缓存和 Newtype/Extend 尚需实现 |
| NIR 与编译产物 | `docs/dev/normalied-intermediate-representation/`、`docs/dev/nessa-bytecode/` | 自包含元数据/归档校验/跨进程执行已接通；精确 stackmap、跨包链接、DEBUG_INFO 与文本格式仍缺 |
| 运行时表示 | `docs/dev/runtime-representation/` | 精确128-bit以内数值、f64与受检动态List已接通；任意精度整数、完整固定宽类型、泛型容器与Map仍有缺口 |
| 内存管理 | `docs/dev/memory-management/` | GC 根、移动/收集、暂停栈、弱引用、外部对象和大对象测试；弱引用 glue 尚标为 stub |
| 引擎初始化与语言库 | `docs/dev/engine-initialization/`、架构文档 | builtin、prelude、core/alloc/std 与包初始化端到端可用；目录或函数存在不能代替能力验证 |
| C/WASM FFI | `docs/ffi/` | 类型/所有权转换、调用、错误、跨栈与生命周期测试；CallWasm 尚为 TODO |
| 故障处理 | `docs/disaster-recovery-and-vm-exception-handling/` | 运行时错误、栈溢出和 FFI 故障的设计行为与平台测试；栈池信号处理尚需审计异步信号安全性 |
| 编译性能与跨平台 | `docs/dev/architecture.md` | 编译与运行基准，以及设计中的架构/操作系统验证矩阵；当前机器上的 Linux 测试不足以证明跨平台目标 |
| 质量与规整度 | 根目录 `AGENTS.md` | rustfmt、workspace 编译/测试和严格 Clippy；当前结果以本轮最终检查为准，不沿用上一阶段基线 |

以上是审计范围，不把尚未检查的领域宣称为已完成。具体语义以各主题规范为准，
弃用目录不作为新功能依据。`docs/TODO-jit/` 的工作需与当前设计定位一并核实。

## 接续顺序

1. 完成类型和值语义：expected type、数值运算/转换、Any 动态检查、Type 值、
   数据类型/trait/限定类型；同时补可靠 native 行为。
2. 完善已接通的 std privileged 编译、模块/use、typed wrappers、prelude 与初始化，
   补关联作用域、core/alloc、通用包发现和完整语言库。
3. 在已接通的 NSBC 自包含产物与独立执行基础上，继续跨包链接、稳定 TypeId、
   精确 stackmap、DEBUG_INFO 和文本格式，扩展恶意输入与跨平台验证。
4. 完成 continuation 的全部类型约束、最后引用 GC 所有权与原生 SP/FP 切换，
   参数 ABI/liveness，及完整 GC/外部对象生命周期。
5. 继续现行设计的 async、结构化并发、FFI、故障处理和其余能力，清理质量基线，
   完成性能与跨平台验证，再按规范逐条审计整个项目。

每一步仍须满足 AGENTS.md 的局部验证要求，不以此顺序延后本次引入的问题。


## 参数绑定阶段与质量基线（2026-10-07，结构体构造之前）

- 解析器统一 `name = value` 与 `.name = value` 调用实参，名称作为参数键解析，
  不执行同名变量赋值。已知声明和直接 lambda 生成 declaration-order 参数计划；
  显式实参先按 source-order 求值，可选参数须命名，遗漏默认值再按参数顺序求值。
- 默认表达式使用声明作用域，可以引用前序参数。自身/后续引用、错误默认类型、
  字面量越界、未知名字、重复绑定及缺必填参数均有拒绝测试，未调用函数也受检。
- NIR 调用前保存 callee/receiver，再逐个保存实参，避免后续赋值改写已求值结果；
  算术、比较、字符串拼接的左值也保存。callee 不再无条件覆盖可选参数。
  前序参数符号只在默认值 lowering 期间替换并恢复，递归调用保留 caller 参数。
- 默认表达式跨边界 return/resume 明确诊断，避免退出调用者；嵌套函数自身返回合法。
- 默认实参递归展开诊断；深度最多256，重复边计入总展开限制262144。普通函数
  body 递归仍合法；这些限制不代表完成所有恶意输入资源分析。
- initializer 依赖扫描加入实际选中的默认表达式，named argument 键不作为局部
  捕获。跨进程 CLI 回归包含启动期默认值的模块依赖，删除源码后仍可执行。
- 字节码维持完整固定参数布局，归档格式不变。只含 FnType 的函数值没有默认值
  元数据，须完整位置传参；一般方法、自定义 apply/构造参数及 variadic 仍有缺口。
- 清理25条原有 lint 和4个旧格式文件问题，没有 lint suppression。
  Rust `PackageType::from_str` convenience API 更名 `parse`（仍返回 Option），并
  实现标准 `FromStr`（返回 Result）；仓库没有旧 API 调用方。

本阶段验证：workspace all-targets check、全仓 strict Clippy、rustfmt 和差异检查
通过；workspace 测试704通过、0失败、1已有doctest忽略。新参数单元测试7项、
聚合端到端3项及CLI归档新增1项均通过。结构体构造校验、关联作用域、List/Map、
完整 std、跨包链接和完整项目目标仍未完成，接续调查见 upcoming-work-audit.md。


## 结构体字段与 plain impl 关联作用域（2026-10-07）

- StructConstructionPlan 在声明字段顺序映射具名及 shorthand 实参；支持前向
  声明、canonical typealias 和按需默认值。先准备字段类型，显式值按源码顺序
  保存，默认值随后按声明顺序求值，装载布局与 FieldInfo 一致。
- 未知/重复/缺必填字段、字面量越界、错误类型与动态非 struct 构造明确诊断。
  默认表达式按声明作用域解析；裸实例字段引用暂不支持，字段/参数默认共用
  defaults 模块的控制流、递归及规模限制。
- AST/parser 新增 PrivateDef 并支持 private/pub 字段包装；源级字段与
  成员访问受检。同canonical类型的不同plain impl可以访问private成员；外部拒绝。
- plain impl 保持自身 lexical scope，共用目标类型的关联命名空间；多impl、
  前向别名、select import、nested module和关联函数值有执行回归。
  静态 Type(args) 绑定受检 new 函数，动态 Type值调用仍诊断。
- self按真实参数符号和canonical接收者类型降级，Self表示关联类型值；源码及
  独立归档覆盖Self构造、私有self字段读取。一般trait/extend的Self语义尚未扩展。
- NIR暴露源函数SymbolId到生成FuncId映射，driver原子搬迁TypePool method、
  trait impl和vtable身份，保留derive sentinel；归档只保存实际函数身份。
  未生成的方法body明确诊断，不再输出含未知方法函数的产物。
- plain impl的const/global/hook纳入共享槽和initializer；静态构造保留原类型
  声明符号，alias使用同时加载声明模块和底层类型作用域，防止遗漏alias模块hook。
- VM字段load/store检查声明字段、分配边界和实际类型；可能分配的断言之后重新
  获取布局。真实Immix回归清空其他引用后，仅global→struct field保持字符串存活。
- CLI跨进程新增3项struct/impl归档回归，删除源码后验证字段布局、默认依赖、
  alias模块hook、self私有字段及关联初始化。

验证：workspace测试732通过、0失败、1已有doctest忽略；全仓all-targets check、
strict Clippy、rustfmt和差异检查通过。类型元数据和NSBC布局未改变。本轮不代表
整个语言已完成：collections/variadic、Newtype/Extend、通用包链接、完整traits、
原生continuation SP/FP切换及其余设计目标仍需落实。集合后续接口和兼容方案见
[collections-implementation-plan.md](collections-implementation-plan.md)。


## 动态 List、单变参与归档兼容（2026-10-07）

- `List` 是 Any 元素的动态容器。`[]` / `[a, b]` 按源码顺序求元素并保存值；
  列表身份由固定 wrapper 保持，别名观察同一次索引写入或扩容。支持静态和 Any
  接收者的 `xs(index)` / `xs(index) = value`，以及 std 的 `List()`、`new`、
  `len`、`get`、`set`、`apply`、`update`、`push`、`pop`。空列表 pop 返回 null；
  非整数索引报 TypeError，负数或越界索引报明确边界错误，不伪造结果。
- runtime catalog 只公开 List。TypePool 在原 intrinsic/trait/null 前缀之后追加
  nominal Struct 角色，不使用名字或固定 TypeIndex 判断集合：List TypeId 为
  `(0x4e455353434f4c4c, 0x0000000100000001)`，Buffer 为同 namespace 的
  `0x0000000100000002`。List payload 是 tagged U64 len/capacity 与 Any buffer
  引用；Buffer 是逐 TaggedValue 扫描的 opaque GC 对象。保留 ID、布局、重复、
  成对存在及未知版本验证，普通 NewObject/字段访问不能伪造或改写内部布局。
- List 单 buffer 容量最多65535个元素；grow 使用受检算术，保持 wrapper 身份，
  在分配期间保留 receiver、待写元素及新 buffer 的根，发布前初始化全部槽。
  pop 清空移出的槽。真实 Immix 回归验证只有 List buffer 保持字符串存活、扩容
  后实际完成收集，以及 detached continuation 保存 List 时的根枚举。
  这些证据不代替完整移动 GC、通用 HostRoot 或所有 collector plan 的验证。
  List临时根使用Drop guard，正常返回、错误及unwind均恢复原根范围；pop移出的
  heap值转入BuiltinCtx临时根，真实收集验证从pop到return发布、context退出后
  的对象仍可达，不依赖已清空的List槽。
- driver 实际嵌入8个 std 源文件：mod、builtin、prelude、io、math、process、
  string、collections。List 方法的静态签名来自 collections.ns；native ID100
  `__list_init` 现在真实返回空 List，ID101–105 分别提供 len/get/set/push/pop。
  native 仍动态检查 arity/身份/索引，静态注解不移入 native registration。
- 已知函数声明及直接 lambda 的单 `...args: List` 收集多余位置参数，字节码
  仍只接收一个 List 参数槽；可保留前面的固定位置参数及后面的可选命名参数。
  元素按调用源码顺序保存，默认值可引用已经打包的 List。双变参、非 List
  annotation、变参之后的固定参数和按名字传入变参均诊断。仅有 FnType 的
  间接函数值不携带变参声明元数据，仍须显式供给完整运行时参数。
- NewList 的12-bit立即量表示初始长度，元素初始化 Unit；LoadIndex/StoreIndex
  的第三操作数是受检索引寄存器。超过4095元素的字面量从空 List 经 push
  构建，不截断长度；源码变参打包不把所有元素塞进32寄存器调用窗口。
  完整 NSBC 保存 List/Buffer descriptors 与原索引，跨进程无需源码执行上述行为。
- `print`、`println`、`to_string` 显示嵌套 List，字符串元素带引号和转义；
  活跃递归路径上的环显示 `<cycle>`，共享但不成环的子列表正常重复显示。
  递归深度限制128层，单次 List 展开输出限制1 MiB，超限返回 DisplayDepthExceeded
  或 DisplaySizeExceeded，不无限递归/展开；private Buffer 不作为用户显示值。

| builtin ABI / 产物 | 安装行为 |
| --- | --- |
| revision 2，合法 ID/名称；List imports 有完整角色 | 接受，仍执行完整字节码与类型池验证 |
| revision 1，manifest 仅引用 ID 小于100的原 builtin | 接受；旧池可完全没有集合角色，保留原索引 |
| revision 1，引用旧占位 ID100 或新 ID101及以后 | 拒绝，避免改变旧 `__list_init` 的返回契约 |
| revision 2，List imports 或集合 opcode 缺角色 | 拒绝；不向已恢复旧池插入新类型 |
| 未知 revision、伪造 ID/名称、损坏或重复 role | 拒绝，entry 不运行 |

TPOL revision 仍为1，使用既有 Struct/TypeId 编码；builtin ABI 提升为2。
Map/NewMap、双变参、`List[T]` 静态元素参数化、Iterator/IntoIterator、Set、
匿名 Object 混合语义、一般方法的完整参数绑定仍未完成。List 容量与显示限制
是当前实现边界，不能据此宣称整个集合或类型系统完成。

本轮新增验证包括源码列表/副作用顺序/错误索引、单变参、跨进程归档、角色损坏
及 ABI 兼容拒绝、嵌套/环/显示边界和真实收集。

最终全仓验证：769个测试通过、0失败、1个已有doctest忽略；全仓fmt、
all-targets check、strict Clippy（-D warnings）及diff检查通过。源码List聚合
6项覆盖值/身份、索引拒绝、求值顺序、默认值及单变参；CLI archive_execution
16项中新增3项集合回归，包含4100元素字面量、40个变参、动态索引与嵌套/环
显示。真实gc_collection 11项中新增3项List根/扩容/暂停continuation及pop返回根回归；
重复参数诊断回归已修复：ParamSelf显式使用intern("self")，不把AST默认StrId0
误当参数名，也不依赖进程驻留顺序。这些检查通过不代表上述未实现目标已经完成。


## Tuple 构造、绑定与渐进边界（2026-10-07）

- Tuple字面量按源码顺序保存元素，支持单元素/嵌套Tuple，空构造保持Unit；
  数字projection按静态形状检查索引，错误类型、超大索引与越界明确诊断。
- Tuple及struct字段赋值按receiver、右值顺序保存结果，再执行受检StoreField；
  共享别名观察同一次更新。同descriptor的Tuple参数断言保留原对象身份，
  需要改变元素表示的转换才创建新Tuple，不为每次调用复制整个对象。
- 局部let/var与typed函数/lambda参数支持嵌套Tuple绑定、通配符和精确arity。
  Module/全局单标识符typed Tuple值正常初始化与共享；其解构明确拒绝。
  无静态shape的Any数字projection/解构拒绝；Tuple match/matches当时尚未实现，
  已在后续Enum阶段按静态known shape接通。
- expected Tuple逐元素传递类型并建立coercion；整个Any→Tuple边界递归检查
  实际对象、payload长度、元素数量和类型。需要转换时建立目标Tuple副本，
  保留源对象及其反射类型，避免通过修改同一对象破坏其它别名的类型约束。
- display独立模块统一List/Tuple遍历与深度/输出限制；Tuple使用括号，单元素
  带尾逗号，字符串带引号，支持混合嵌套及`<cycle>`，继续受128层/1 MiB限制。
- Tuple payload为逐槽TaggedValue，转换中的源及新元素使用临时根。
  真实GC测试验证Tuple-only字符串根与paused continuation段里的Tuple；
  跨进程归档回归覆盖类型别名、全局Tuple、nested绑定、显示和逐元素Any边界。
- 复用TypeKind::Tuple与既有结构驻留/原索引snapshot恢复；NewObject和受检
  Field指令已可承载Tuple，没有新增NSBC布局tag或builtin ABI。已有12-bit
  类型/字段编码和构造4096字段上限不变。

源码聚合回归覆盖构造/projection/nestedletvar、typed函数/lambda、source-order
快照、字段写入、逐元素与整对象Any边界及拒绝输入；独立归档回归已通过。
本阶段最终全仓测试784通过、0失败、1已有doctest忽略；all-targets check、
strict Clippy、rustfmt与diff检查通过。包括Tuple源码聚合4项、CLI归档17项
（新增1项Tuple）、真实GC13项（新增2项Tuple），不能据此推断整个类型系统完成。

本阶段之后的Enum任务已在下一节落实：修复无载荷enum运行时身份碰撞，使不同
enum相同ordinal不能被当成同一个值，并落实携带值enum的构造、类型/布局、
检查/显示、模式分派、GC及归档。Tuple动态shape和module解构、Newtype、完整traits/Iterator/Map、
跨包链接及continuation/GC剩余目标仍未完成。

## Enum 名义身份、载荷与递归模式（2026-10-07）

- 无载荷variant改为独立IMM_ENUM9，data编码type32/tag25，与legacy Symbol4
  完全分离；同ordinal的不同Enum不再碰撞，反射返回所属Enum，alias保留身份。
- payload variant支持固定具名typed字段、位置/具名实参、前向/递归类型及
  alias。显式实参按源码顺序求值并保存，再按声明字段排列；数量、未知/重复/
  缺必填字段、类型与可见性错误均诊断。typed字段中的Any使用受检边界，
  分配前检查/转换并保留所有元素根。
- payload布局为Enum header+tagged enum tag+max_variant_fields个TaggedValue，
  非当前variant槽初始化Unit。无载荷值无需堆分配；header类型/tag/type/variant/
  payload长度受检，EnumField只读取当前variant实际字段并检查类型。
- match/matches建立每arm作用域，检查所有arm及bool guard；模式支持递归
  Enum与静态known-shape Tuple、字面量、绑定及通配符。EnumIs检查身份/tag后
  才提取字段；子模式或guard不匹配继续下一arm。Any可以用明确Enum模式动态
  分派；Any没有静态Tuple形状时仍拒绝Tuple模式。未匹配时发MatchFail并返回
  NoMatchingCase，不再无条件执行末arm或返回伪造Unit。
- `matches`使用独立临时作用域，绑定只能用于模式guard，不能在失败、部分
  失败或短路后从外部读取；尚未实现成功分支的flow-sensitive绑定。match每个
  arm都按结果类型检查/转换，数值分支统一提升，避免静态i64实际返回bool。
- Enum本体关联方法登记到TypePool并搬迁实际FuncId，self方法对无载荷立即值
  和携值对象均受检调用；源码验证enum/alias初始化hook，独立归档也覆盖递归方法。
- display模块统一Enum/List/Tuple遍历，Enum显示`E.none`/`E.some(value)`，
  字符串带引号/转义，递归环为`<cycle>`；共同128层/1 MiB上限明确报错。
  真实GC回归覆盖Enum-only字符串与暂停continuation段中的Enum载荷。
- NSBC Constant::Enum追加tag8（type_index:u32LE、variant:u32LE），检查真实
  Enum及variant。payload descriptor仅供构造/谓词metadata，不能直接Load为
  无载荷值。新增NewEnum0x67/EnumIs0x68/EnumField0x69与MatchFail0xD4；
  两种descriptor常量参与narrow12bit排序搬迁，MatchFail payload0且无后继。
- NSBC v3、NSAM1/TPOL1及builtin ABI2不变；旧普通常量/指令不重新解释，
  旧reader遇新tag/opcode拒绝。真实跨进程归档覆盖alias、递归载荷、全局值、
  反射/身份、guard、递归Enum/Tuple模式及显示，不依赖源码或原StrId。

同Enum/tag的无载荷值相等，不同Enum或tag比较false；同Enum/tag的payload
比较目前明确UnsupportedEnumEquality，尚未完成Eq/derive字段分派，不能用
指针相同冒充字段相等。Or/AsBind等复杂模式、List/struct模式、constructor函数值、
默认/optional/variadic variant字段、泛型Enum、完整穷尽性/trait规则仍未实现。
本阶段全仓测试814通过、0失败、1已有doctest忽略；all-targets check、strict
Clippy、rustfmt及diff检查通过。Enum源码聚合7项、CLI归档18项（新增1项Enum）、
真实GC15项（新增2项Enum）均通过；这不代表完整语言设计或全部模式已实现。
Map/Iterator/双变参/Newtype、跨包链接与continuation/GC剩余目标仍未完成。

### Eq/PartialEq 派生函数与持久化

这一阶段补上静态 Eq/PartialEq 的完整字段比较路径：struct、enum 和 tuple
alias 派生生成普通 NIR 函数。先登记所有派生，再递归验证字段先决条件，支持
前向声明、递归 enum、Tuple/Optional 组合。Enum 先确认双方同一 variant，
再按声明顺序短路比较；struct/enum 字段按具体 trait 调用用户实现或生成函数，
PartialEq 可回退 Eq，不通过同名方法顺序猜测字段实现。

生成函数沿用正常 VM 调用帧与 GC 根，不在 Rust 中重入解释器。旧 native 派生
Eq/Cmp/Display 校验准确实参数量；Hash 不再返回占位 0，源码 Hash 派生明确
诊断。未知堆值 Eq 不再用地址相同冒充值相等。Any 动态 trait 分派、循环值
比较预算、完整 Ord/Hash/Display 派生仍未完成。

Driver 先搬迁源码函数身份，再原子发布生成方法的真实 FuncId 至 method、
trait impl 与可确定对应关系的 vtable。歧义或缺失身份报诊断，不输出产物。
现有 TPOL/NSBC 格式即可保存普通函数身份，无需改变格式版本；多 sentinel
且没有 method-name 映射的复杂 vtable 明确拒绝歧义，保留旧未生成 sentinel。
删除源码后的跨进程回归证明 enum 派生比较仍调用字段自定义 Eq，计数验证
短路与 variant 分派；真实 GC 回归覆盖普通 Eq 帧中左右嵌套字段及效应暂停。

同时修复 Optional 的通用类型关系：null 和内层值可赋给对应 Optional，
Optional 数值提升经现有 nullable cast 保持实际内层表示；反向解包和数值收窄
仍诊断。Tuple alias impl/self 在真实 std builtin alias 路径完成前向绑定，
两个不同 trait 的同名 eq 不再被当作普通关联定义重复；显式歧义访问报错。

本阶段最终全仓测试838通过、0失败、1已有doctest忽略；workspace all-targets
check、strict Clippy、rustfmt 与 diff检查通过。新增派生端到端7组、Optional
lifting2组，CLI归档19组与真实GC16组全部通过。验证日志保存在
`/tmp/nessa-derived-workspace-final.log`、`/tmp/nessa-derived-check-final.log`和
`/tmp/nessa-derived-clippy-final.log`。这些完成项不代表完整语言设计已实现；
Map/Iterator、泛型与剩余trait、跨包链接及continuation/GC目标继续推进。


### 动态 Map、标准库与独立归档

字符串键、Any值Map现在贯穿稳定类型角色、Map()/Map.new()构造、原生与std
方法、静态/Any索引读写、GC、共享显示器与NSBC独立加载。源码通过真实std
声明获取Map，不向用户root注入builtin；Map透明alias保留同一类型身份。
新增native IDs110–115为init/len/get/set/remove/contains，std同时提供apply/update。

Map使用固定3 tagged words的wrapper与4 tagged words/bucket的GC buffer，
按字符串UTF-8内容计算确定57位hash，处理开放寻址碰撞与tombstone；删除清空
key/value根。get/remove缺失返回null，contains区分缺失与存储null。增长重新
插入活跃桶、完成后才发布buffer，失败保留旧wrapper和内容。查询只检查布局
和实际探测桶，不对每次操作全表扫描；最大8192桶满表仍可更新已有键，新键
明确ObjectTooLarge，删除后可重新插入。运行时仍依赖已验证的pinned Immix，
未因重读布局而宣称支持移动GC。

Map/MapBuffer追加稳定TypeId低半部version1 id3/id4；保留intrinsic/trait/null
和List/Buffer身份。TPOL1 exact-index恢复允许legacy、List-only、Map-only和四
角色完整池，各自要求成对且布局准确。普通NewObject/字段访问明确拒绝Map
内部角色；NewMap0x62受检启用（imm12预分配容量，len0），LoadIndex/StoreIndex
支持受检Map角色。源Map索引以MAP_GET/SET lowering，不擅自新增Map字面量或
把匿名Object解释为Map；当前Object源表达式明确诊断未支持。

builtin ABI3新增Map导入，ABI2仅兼容ID<110的已有契约，ABI1仅兼容ID<100。
每个ID/name核对，集合native导入必须有对应pairedroles；NSBC3/NSAM1/TPOL1
revision不变。跨进程CLI归档在删除源码后验证Map全局共享、Unicode键、null/
missing、remove、索引写入和nestedList/Tuple，实际运行不依赖源码或StrId。

最终全仓测试863通过、0失败、1已有doctest忽略；all-targets check、strict
Clippy、rustfmt与diff检查通过。新增Map源码7组、native边界2组、VM Map5组、
真实GC3组及metadata/CLI5组；Map真实GC验证仅由Map图保留的key/value、删除
结果跨native返回槽保留、暂停continuation中的Map。CLI归档共20组、真实GC共19组。
日志：`/tmp/nessa-map-workspace-final.log`、`/tmp/nessa-map-check-final.log`、
`/tmp/nessa-map-clippy-final.log`。

泛型List/Map、任意Hash/Eq键分派、Iterator、匿名Object、双变参、跨包链接及
continuation/GC剩余设计仍未完成；String的++运算符尚未接入，当前字符串拼接
使用实际std.string.str_concat。完整项目目标继续保持，不以本轮集合切片替代。

### 拼接运算符与受检源方法

`++`不再盲发无类型的concat MethodCall：静态receiver按实际词法作用域查找
`concat(self, other)`，检查可见性、固定单操作数、operand类型/coercion和真实
结果类型，再绑定具体源函数普通Call。左操作数求值后先快照，再求值右侧；
参数传递、返回值与effect/GC保持正常VM帧路径。缺方法、非self、optional/
variadic或多operand签名明确诊断；Any继续动态受检，不把数字自动转字符串。
未注解方法返回值支持按需前向推导，两种声明次序的metadata都实际为i64；
递归未注解推导通过guard保留Any，不声称完成递归类型约束求解。

真实std.string提供String.concat/len；`++`支持Unicode、空串和链式拼接，
len沿现有str_len的UTF-8字节数契约。std.collections的List.concat用源语言
构造新wrapper并依次push，两侧输入不变，嵌套元素保持共享引用；self-concat
不会修改输入或无限遍历。没有新增native IDs、指令或ABI/归档revision。

NIR初始化Planner现在消费concat_calls/instance_methods，追踪方法所属scope、
关联常量、hook和函数全局依赖，避免global initializer中的运算符漏掉依赖。
普通动态method slot在写寄存器前核对函数存在、self+实参精确数量以及32寄存器
边界；缺/多参数、无效函数和注入self溢出明确报错。真实GC回归覆盖自定义
concat→String方法→native、暂停operand以及暂停结果，恢复后仍保留完整字符串。
String concat结果超过u16对象计数上限时报ObjectTooLarge，输入合法不等于结果
也能分配。

最终全仓879测试通过、0失败、1已有doctest忽略；all-targets check、strict
Clippy、rustfmt及diff检查通过。源码concat8组、CLI归档新增3组（共23组）、
method VM边界1组、真实GC新增1组（共20组）、前向类型metadata2组和native
容量1组均通过。删除源码后的CLI验证String/List拼接、浅共享、自定义非receiver
结果、副作用与effect恢复。日志：`/tmp/nessa-concat-workspace-final.log`、
`/tmp/nessa-concat-check-final.log`和`/tmp/nessa-concat-clippy-final.log`。

动态方法的private/extend访问权限仍缺caller scope/token元数据：现有visible_scope
描述extend，而plain private方法权限没有完整持久化，不能把静态可见性检查当作
动态权限完成。通用apply/update、完整Ord/PartialOrd、泛型、Iterator、匿名Object、
跨包链接和continuation/GC剩余目标仍在继续；完整设计目标未因此缩减或达成。

### 通用 apply/update 与普通参数绑定

静态已知实例的调用和调用赋值现在分别记录实际 apply/update 源函数身份，
按真实词法作用域检查方法权限、self、参数与结果类型，经普通 Call 执行。
声明参数计划排除隐式 self，复用具名、默认值和单 List 变参绑定；更新右值
对应最后固定位置参数或变参尾元素。默认值可以引用 self 与前置参数；共享
绑定器同时修复了 Tuple 解构参数的默认值错误读取整个 Tuple 的问题。
无载荷 Enum 的 projection 保留实际值，有 apply 时按实例调用，避免误用构造路径。

NIR 先保存 receiver，再按源码顺序保存显式实参和更新 RHS，之后才重排参数、
打包变参并求默认值。方法返回类型用于 apply；update 返回值丢弃，赋值为 Unit。
更新不要求 getter，也不执行 getter。初始化 planner 消费两个新方法身份表，
追踪全局依赖与所属 scope/hook。Any 仍以完整位置参数动态分派；CallIndirect
修复非堆立即值的 apply 查找，保留函数、闭包与 continuation 的原调用路径。

源码回归包含 named/default/variadic、Tuple default、setter-only、多索引、
结果类型、快照与副作用顺序、初始化及效应恢复，并覆盖错误类型/签名/权限。
真实 GC 验证 apply/update 普通帧和暂停 continuation 中的 receiver、key、RHS
及结果根。新增4项删除源码后跨进程 CLI 归档回归，包含 Enum、默认/具名/
变参、初始化与 Any/effect 路径；沿用当前 NSBC3/NSAM1/TPOL1 和 builtin ABI3。

最终全仓893测试通过、0失败、1已有doctest忽略；all-targets check、strict
Clippy、rustfmt及diff检查通过。CLI归档27项、真实GC21项均通过。日志：
`/tmp/nessa-application-workspace-final.log`、`/tmp/nessa-application-check-final.log`
和`/tmp/nessa-application-clippy-final.log`。

动态方法的 private/package/extend 权限仍缺持久化的访问等级、作用域关系和
调用点上下文；已整理[完整实施计划](dynamic-method-access-plan.md)，尚未实施。
不能用静态检查或运行时参数校验冒充动态权限已完成。完整 traits、泛型、
Iterator、匿名 Object、跨包链接及 continuation/GC 剩余目标仍未完成。

### 方法权限元数据与 TPOL2 基础

方法槽显式保存 Public、Package(package ID)、Private(declaration scope) 或
LegacyUnknown，保留独立 extend scope。源方法按实际 symbol visibility 登记，
不再剥离修饰符后丢失权限；Struct/Enum 本体、普通 impl、trait impl/default
与生成派生入口均进入实际生产路径。缺函数 symbol 明确诊断，不生成伪造 ID。

TypePool 保存受检 ScopeContext 图，包含父 scope、归档局部 package 身份及
canonical associated type。普通模块继承用户包，显式加载 package root 建立
独立身份；synthetic ROOT 与实际用户 FileScope 归一，不改变 @ 路径查找根。
静态 package 授权与元数据共用身份规则，不用 std 名字或 StrId 授权。图验证
引用、无环和每包单根；访问 scope/package/type 引用验证失败不发布新图。
独立 method_accessible 查询叠加访问等级与 extend 范围，保留同包同类型的
跨 impl private 规则；UnknownAccess 与坏 caller/metadata 区分普通权限拒绝。

TPOL writer 升为2，reader兼容1/2，保留 exact-index；新格式记录访问等级与
scope 图。旧 TPOL1 缺权限，标 LegacyUnknown 并保留原 extend ID，不默认 public；
重新写入新版仍保持 Unknown。NSBC3/NSAM1 与 builtin ABI3 不变。损坏访问tag、
图环、parent、package、private/extend scope及associated type均受检拒绝。

新增11项回归：TypePool4组、resolution3组、codec3组、driver完整编译/归档/
执行1组。driver证明不同impl的private语义、源方法权限及scope身份往返，恢复
后实际返回42。最终全仓904测试通过、0失败、1已有doctest忽略；all-targets
check、strict Clippy、rustfmt与diff检查通过。日志：
`/tmp/nessa-method-access-workspace-final.log`、`/tmp/nessa-method-access-check-final.log`
和`/tmp/nessa-method-access-clippy-final.log`。

这里只完成可验证的权限元数据基础，VM尚未消费调用点scope和授权查询，动态
权限整体仍未完成。已复现 extend self 前端缺少所属类型绑定的诊断；显式target
参数的metadata测试不能证明self扩展可调用。下一步须共同补齐扩展的词法候选
筛选、调用点scope持久化与VM分派授权，详见[实施要求](dynamic-method-access-plan.md)。
完整语言与continuation设计目标继续保持，尚未达到完成条件。

### 词法扩展与动态调用点授权

ExtendDef/ExtendTraitDef scope 预准备并绑定 canonical type，self/Self、别名及
导入目标沿普通方法检查。方法候选先过滤扩展声明 scope及后代和访问等级，
再判断歧义；不相交模块/块可独立定义同名扩展，可见重叠不按登记顺序选取。
局部函数收集继续进入函数体，局部扩展成员有实际 FuncId，修复缺 func_map
panic。无效 associated type 在重复检查中先受检，不把错误源码送入 get panic。

NIR ScopedCall 保存原表达式的词法 scope，默认值保持声明侧 scope；codegen按
实际指令 PC 发布完整 MethodCallScope 表，覆盖窄/Far方法调用及CallIndirect。
无 scope 的新动态调用明确拒绝。VM从当前task的函数与PC取授权上下文，
过滤候选后校验函数与参数并进入普通帧。private/default package/extend外部
访问拒绝，歧义明确报错；closure及continuation恢复不借用caller或handler
权限。授权后集合intrinsic仍保留受检整数索引ABI，避免std usize包装提前误拒
原合法signed index。无效context表原子拒绝，换装类型池清除旧授权。

NSAM writer对Some完整表（含空）写revision2，对legacy None仍写revision1；
reader1/2分别恢复None/Some，不默补public或完整空表。validator校验所有动态
PC完整覆盖、函数/PC/scope引用和重复记录；有词法图的dynamic code不能降级
为缺上下文。旧TPOL1方法LegacyUnknown不能授权、需从source重编译，ordinary/
closure及明确intrinsic协议按原ABI处理。TPOL2、NSBC3与builtin ABI3沿用；
窄方法名/Far和常量搬迁保持PC，往返与损坏输入均已验证。

同时修复lambda free-variable分析遗漏SelfLower，方法内lambda捕获self后
实际返回42。跨scope initializer经extension concat helper读取后定义模块global
已证明依赖先初始化；同scope前向读取仍报UninitializedGlobal，保持原源码
初始化顺序，不为测试重排副作用。

新增20项回归：extension源码7组、NIR1组、codegen2组、VM3组、NSBC1组、
codec3组及CLI3组。真实source涵盖self/Self、private/alias、局部块、defaults、
apply/update/concat、闭包、多次恢复及初始化；删除source后的独立CLI覆盖默认值
声明权限、闭包逃逸、continuation恢复、同包方法及访问拒绝。最终全仓924测试
通过、0失败、1已有doctest忽略；CLI归档30组、真实GC21组通过；all-targets
check、strict Clippy、rustfmt和diff检查通过。日志：
`/tmp/nessa-dynamic-method-workspace-final.log`、`/tmp/nessa-dynamic-method-check-final.log`
和`/tmp/nessa-dynamic-method-clippy-final.log`。

具名扩展方法捕获外层function local尚无环境ABI，当前明确诊断；其内部lambda
捕获self和普通lambda捕获仍可用。同类型同trait的多个词法扩展仍缺record级
可见域，重复(type,trait)仍拒绝。扩展关联值/hook、完整traits/泛型/Iterator、
跨包链接及continuation/GC剩余目标继续推进，完整项目尚未完成。

### 词法 trait 身份、类型边界与 NSAM3/TPOL3

TraitImplRecord 与 VTable 增加显式 visible_scope，身份按 canonical
(implementor, trait, scope) 区分；不同模块/块可为同类型实现同 trait，同 scope
重复拒绝。受检查询按调用位置过滤，可见重叠报歧义；旧无 scope 的查询只返回
全局实现，不泄漏词法扩展。父 trait 和继承 vtable 按同一规则解析，缺少方法
或函数身份明确诊断，不生成 func_id=0 占位。全局派生不能捕获局部字段比较
证明，比较调用同时检查所选方法的访问权限。

trait 注册完成后再验证类型注解、参数/返回与 Optional 等义务，避免早期类型
阶段因尚无 record 而漏检。运行时 .as(Type)、Any 边界和参数 prologue 使用
声明/表达式自身的 scope；默认值、闭包、效应暂停及多次 continuation 恢复
保持原词法上下文。当前没有通用 .is(Type) 源码语法；TypeCheck 指令通过底层
回归和已有 pattern lowering 验证，不能把 opcode 等同于新的语言表达式。

NIR 将 checked expression 与 StoreGlobal/StoreField 的原始 scope 显式传到
codegen，按实际发出的 PC 记录。共享 ScopeCoverage 定义 dynamic calls、四种
显式类型指令及隐式声明类型检查的 StoreGlobal/Wide、LoadField/StoreField、
NewEnum/EnumField 的覆盖契约。VM 每条指令安装并恢复查询上下文，不让后续
host 查询或恢复后的 frame 借用前一指令的权限。全局 Any 读取可传输普通值，
不会赋予调用扩展或重新转换为该 trait 的证明；字段/枚举读取仍检查声明布局。

writer 新源码使用 TPOL3 和 NSAM3，reader 保留 1/2。TPOL3 持久化 record/table
各自的 scope，读取和重复保存保持 exact-index 及选择结果；旧 record 只在
方法 scope 一致时推导，旧 vtable 必须唯一对应 record。旧 NSAM2 仅承诺动态
调用的 scope，不能自动标为完整类型上下文；无 context 的查询若涉及 scoped
trait 明确 MissingTraitContext。损坏覆盖表、scope 引用、重复 record/table 和
不安全旧格式迁移均拒绝。外层 NSBC3 与 builtin ABI3 保持。

新增端到端覆盖 disjoint/overlap/duplicate、别名、父 trait 前向声明、typed
参数/返回/Optional、.as 与 Any 注解、global/struct/enum 存取、default/closure/
effect 恢复及外部拒绝；CLI 删除源码后执行 NSBC，验证同 trait 两模块各自
选取及上述运行时类型上下文。完整 trait evidence/boxing、具名扩展环境 ABI、
泛型、扩展 hook/关联值初始化和独立包链接仍未完成，整体项目目标继续进行。

最后补上 ForLoop 的已知 IntoIterator/Iterator 义务：在 lowering 前检查可见
歧义、方法缺失、访问权限、self-only 参数和 has_next 的 bool 返回，错误源码
产生诊断而非触发 NIR invariant。普通动态 iterator 协议保留原受检分派路径。
缺失 comparison/derive/iterator/trait requirement 的词法 scope 也明确诊断，
不以 ROOT 或当前 ambient scope 补造证明。Ord/PartialOrd 的完整派生及结果协议
尚未完成，不因独立 scoped slot/vtable 能登记就宣称完成。

本轮最终验证：全 workspace **956 passed / 0 failed / 1 existing ignored
doctest**；all-targets check、strict workspace Clippy、fmt 与 git diff --check
全部通过。最后日志位于 `/tmp/nessa-scoped-workspace-final.log`、
`/tmp/nessa-scoped-check-final.log`、`/tmp/nessa-scoped-clippy-final.log` 和
`/tmp/nessa-scoped-fmt-final.log`。未提交或重置既有工作区变更；整体目标仍 active。

### 持久化 trait 方法槽：TPOL4 与受检 vtable 描述

实际复现了跨模块 trait 返回/字段传输从实现40重选为2、局部实现传给模块外
trait 参数函数 TypeError，以及不同trait同名方法 AmbiguousMethod。另确认
required 方法签名未比对：声明i64而实现String仍check通过。这些错误没有在
本轮被伪称修完；隐藏 vtable ABI 和接口签名仍是必须补齐的后续工作，计划见
`docs/dev/trait-evidence-abi-plan.md`。

本轮为该 ABI 建立真实元数据基础：TypePool 持久化 TraitDispatchSchema，槽
保留 canonical 声明trait owner/name；source与固定bootstrap接口生成统一
父优先schema，table按schema构造，不再由每个impl的methods任意定义接口。
checked_vtable_descriptors 校验 exact(type,trait,scope)、原声明scope的父实现、
名字/槽顺序/数量与实际函数ID，并区分声明owner与child覆盖的implementation。
缺schema的legacy元数据仍可读取，但新查询明确MissingSchema，不能猜出证明。

新增校验同时揭示并修复：父trait表达式为typealias时原collector丢失继承；
父派生方法生成后Child table仍留sentinel。现在canonical识别父trait，派生
发布原子更新真正继承该global实现的所有schema-aware槽，不覆盖scoped自定义
实现。过深forward继承链在schema生成前诊断，避免递归栈溢出。PartialOrd派生
旧cmp槽与partial_cmp接口不一致现明确拒绝，不宣称完整可选结果协议已实现。

TPOL writer升4，尾部追加schema，reader兼容1/2/3/4；旧pool保持空schema，
重复保存不伪造。接口names采用UTF-8、计数/字节数/引用/重复/损坏槽都受检。
NSBC loader除了target存在，还拒绝schema槽被其它已知函数替换。source-delete
CLI验证alias/继承/schema顺序与重保存。NSAM3/NSBC3/builtin ABI3保持；本轮
没有新增proof opcode、隐藏参数、carrier或改变trait返回语义。

最终全workspace **975 passed / 0 failed / 1 existing ignored doctest**；
all-targets check、strict workspace Clippy、fmt、git diff --check通过。日志为
`/tmp/nessa-trait-schema-workspace-final.log`、`/tmp/nessa-trait-schema-check-final.log`、
`/tmp/nessa-trait-schema-clippy-final.log` 和 `/tmp/nessa-trait-schema-fmt-final.log`。
整体目标保持active，未提交或重置工作区。

### Trait 接口签名契约与 TPOL5

required 方法现在检查接口声明与实际实现的参数类型、参数数量、参数类别和
返回类型。此前声明返回 i64、实现返回 String 仍能通过 check 的程序明确拒绝。
默认方法体与 child 覆盖也受检查。真正的声明 Self 按 implementor 替换；显式
trait 类型、指向 trait 的别名和其它类型的 Self 别名保留原含义。嵌套函数、
tuple、Optional 及 error/effect 限定结构记录对应路径，nominal effect 身份
不会因结构同形被丢失。检查逻辑分别归入 TypePool 和 resolution 独立模块。

TPOL writer 升至5，保存声明函数类型、Self 路径和参数类别，reader 兼容1–5。
路径深度、标签、计数和类型引用受检。NSBC 加载与写出均校验 vtable 目标函数
的逻辑签名，拒绝签名被替换的归档；删除源码后的 CLI 执行覆盖别名、嵌套 Self
及重复保存。旧格式和缺声明签名的 bootstrap 槽保持 None，不推测补造契约。
NSAM3、NSBC3 和 builtin ABI3 保持不变。

新声明签名同时暴露 NIR 将 trait 原型当作直接调用函数的问题：typed trait
receiver 继续走虚方法调用，具体实例方法保留直接调用。已有 CLI 与 scoped
trait 回归恢复通过。这仍不是完整 evidence 分派；同名接口碰撞、调用方隐藏
vtable 参数、继承 Self 返回类型特化和默认实现适配仍待实现。trait 返回与存储
证明的统一语义尚待用户明确选择，整体项目目标保持 active。

最终全 workspace **993 passed / 0 failed / 1 existing ignored doctest**；
all-targets check、strict workspace Clippy、fmt 和 git diff --check 通过。
日志为 `/tmp/nessa-trait-signature-workspace-final.log`、
`/tmp/nessa-trait-signature-check-final.log`、
`/tmp/nessa-trait-signature-clippy-final.log` 和
`/tmp/nessa-trait-signature-fmt-final.log`。未提交或重置既有工作区变更。

### 继承 trait 调用的静态 Self 特化

父 trait 关系现在在别名准备后、函数体类型检查前建立，最终 trait 收集复用
同一份关系。此前部分继承方法没有静态调用类型而退回 Any，运行成功不能证明
类型正确。现在 Child 参数调用父声明 clone()->Self 的表达式精确返回 Child；
别名、diamond、嵌套 Optional/tuple/function 的 Self 同样特化，显式 Base 保留。
错误 Self 参数、参数数量和不存在的方法在编译阶段拒绝。

TypePool 新增受检签名实例化 API；先完整检查形状，再注册结构类型，错误输入
不产生部分类型。子接口同名重声明用独立 symbolic Self binder 比对父契约，
既不把显式 Child 冒充 Self，也不因 qualifier 集合排序改变路径索引而误拒。
不兼容声明即使未使用、没有 impl 也报错。真正 Self 的进一步 Grandchild
调用保持特化。源码回归直接断言 AST 精确类型，另有继承 clone/value 的真实
执行及删除源码后的独立 CLI 归档执行。

本轮没有改变 TPOL5、NSAM3、NSBC3 或 builtin ABI3。隐藏证明参数尚未实现；
调用参数/closure captures 的显式物理布局、trait slot 执行、默认方法适配
和 GC/continuation 保存是后续完整纵切。具体调查见
`docs/dev/trait-parameter-abi-plan.md`。bootstrap 固定契约、native sentinel
验证、Ord 与 Ordering 差异及关联返回类型缺口已记录在
`docs/dev/bootstrap-trait-signatures-plan.md`；它们仍是待办，不是完成声明。

最终全 workspace **1002 passed / 0 failed / 1 existing ignored doctest**；
all-targets check、strict workspace Clippy、fmt 和 git diff --check 通过。
日志为 `/tmp/nessa-trait-self-workspace-final.log`、
`/tmp/nessa-trait-self-check-final.log`、`/tmp/nessa-trait-self-clippy-final.log` 和
`/tmp/nessa-trait-self-fmt-final.log`。整体目标仍 active，未提交或重置工作区。

### 显式函数入口布局与 NSAM4

新增共享 FunctionAbi，区分 capture 环境槽、用户逻辑参数及物理入口数量。
NIR 的 Capture/User 角色由真实捕获生产处填写，所有源码函数、native adapter
和 handler/body closure 输出 Some(Value 布局)，不再从总参数数量减源码签名
推导 captures。codegen 拒绝 capture 非闭包或不在入口前缀的畸形 NIR。
Trait 参数预留 proof、data 的物理顺序，但本轮尚不生成隐藏 proof 或新指令。

归档 validator 检查显式布局与 CODE 总 count、源码函数签名、closure 身份及
trait view 的一致性。VM 在实际调用时也检查：直接/远调用、root 入口、方法、
闭包创建和 invoke、LoadCapture、反射、handler 安装及 Reset/ResetClosure。
真实环境的 capture 数、用户传参数和总入口槽分开校验；直接 host 安装代码
不能以旧寄存器内容绕过 Some 的调用数量检查。handler/reset 超窗口数量也
在切片寄存器前拒绝。Trait/TraitProof 入口仍明确 Unsupported，不伪装可执行。

NSAM writer 有任意 Some 时写4，显式保存 scope coverage、scope table presence
和精确覆盖函数 ID 的 ABI table；reader 兼容1–4。旧1–3仍为 ABI None，全部
None 的重保存保留原1/2/3 scope 契约，混合产物逐函数保留 None/Some。标签、
presence、重复/未知 ID、类型、数量与分配预算受检；每函数 capture/parameter
数量在分配前限制为32。TPOL5、外层 NSBC3 与 builtin ABI3 保持不变。

新增回归验证真实 closure 参数/捕获角色、畸形 prefix/layout、直接与远调用
以及 root 参数数量、错误 closure 环境、handler/delimiter 拒绝不改变帧、
归档损坏/重保存和删除源码后的 escaping closure 执行42。真实 GC 的 delimiter
环境与 detached continuation 持有8个 captures 的测试已采用显式 ABI；GC
仍扫描原 TaggedValue 根，捕获/恢复继续拆接独立栈段，不复制调用帧。

最终全 workspace **1016 passed / 0 failed / 1 existing ignored doctest**；
all-targets check、strict workspace Clippy、fmt 和 git diff --check 通过。
日志为 `/tmp/nessa-function-abi-workspace-final.log`、
`/tmp/nessa-function-abi-check-final.log`、`/tmp/nessa-function-abi-clippy-final.log` 和
`/tmp/nessa-function-abi-fmt-final.log`。下一项仍是实际 proof 取得、入口配对检查
与 trait slot 调用；默认 adapter、bootstrap 契约及 trait 返回/存储语义也未完成。
整体目标保持 active，未提交或重置既有工作区变更。


### 裸 trait 参数证明、接口槽与可执行 NSBC

裸 trait 用户参数展开为 proof、data，capture 按 data、proof 保存。NIR 显式
跟踪关联，typed local、CFG 分支、赋值、同视图转换、默认表达式声明侧 scope
与已知间接调用保留调用方选择；显式 Any 参数/绑定继续擦除证明，不提前决定
trait 返回与存储的未决语义。入口先保存全部槽，再验证配对；VM 也独立检查
真实 incoming 参数与 captures，不能通过省略 TraitAssert 绕过。

新增 TraitProof、TraitAssert、TraitProject、TraitCall、CallIndirectProof。
TraitCall 使用接口槽，冻结方法选择；父视图通过真实 parents 图投影原 table，
保留 child 覆盖，不按接收方 scope 重新选择。extra Self 参数检查实际 concrete
身份，显式 trait 参数独立带 proof。专用 immediate subtag10 的57-bit handle
引用 VM 内部不可变 registry；全进程单调 ID 防止跨 VM 碰撞，pool 重装令旧
handle 失效。按 root table/view 缓存，不混用不同 scoped implementation，
每次取得仍检查当前 scope、权限和真实签名。registry 不保存 receiver 地址。

NSAM4/TPOL5/外层NSBC3/builtin ABI3 保持；新指令及布局贯通 reader、validator、
relocation 和 VM，handle 不归档为常量。旧1–3的逻辑入口解释保持，未知新
指令由旧 reader 拒绝。validator 检查 schema/view、capture 配对、物理数量、
寄存器及槽可能范围；精确 proof/view/slot/data 的验证由 VM 完成。

源码与删除源码后的归档回归覆盖跨 module 40/2 经外部 consume 得42、Left/
Right 同名槽、child 覆盖的 parent 转发、escaping closure 和 effect 多次恢复。
真实 GC 验证 receiver 的 heap String 仅由 closure/独立 continuation 持有时
仍可达。损坏 view/slot/layout、伪造或跨 VM handle、错误 data/capture 配对
均拒绝；重复 acquire/project 不积累 registry 项。continuation 捕获/恢复仍
拆接 delimiter 独立栈段，复制成本仅归属多分支 clone。

最终全 workspace **1039 passed / 0 failed / 1 existing ignored doctest**；
all-targets check、strict workspace Clippy、fmt 与 git diff --check 通过。
日志为 `/tmp/nessa-traitproof-workspace-final.log`、
`/tmp/nessa-traitproof-check-final.log`、`/tmp/nessa-traitproof-clippy-final.log` 和
`/tmp/nessa-traitproof-fmt-final.log`。默认方法具体 adapter、缺签名 bootstrap
接口、comparison/for 的 trait 参数 proof 路由、nested 参数、返回及存储仍未完成。
旧 EffectCallDyn 的裸 trait 输入明确 Unsupported；零输入 effect 捕获 proof
可执行，后续输入协议不能偷偷扩写旧 scope coverage。整体目标仍 active，未
提交或重置既有工作区变更。


### Bootstrap 固定接口、trait 参数比较与原生派生契约

新 source producer 为 Eq/PartialEq.eq(Self,Self)->bool、Display.to_string(Self)
->String 和 Iterator.has_next(Self)->bool 保存固定声明、Self 路径与参数类别。
schema 与静态方法投影共用同一声明来源，不从具体实现反推接口。固定 schema
即使无 implementor 也存在；未调用的错误 arity、receiver、参数模式或结果类型
在 resolution 拒绝。继承 builtin 接口的方法将真实 Self 特化为 child view；
源码声明及访问检查优先，不能因权限错误退回父 builtin。方法值与无 AST 的
具名绑定没有假装实现。Iterator.next/IntoIterator 返回仍保留未完成契约。

Eq/PartialEq trait 参数的比较改用冻结 proof 与接口槽，other Self 独立带证明，
不同 concrete 数据配对拒绝；concrete 比较保持原静态函数选择。普通 virtual
调用复用该 helper，并修正继承 Self 参数错误使用声明 owner 的问题。for 的
消费者已整理为共用路径，但裸 trait iterable 或 into_iter 返回 trait 会因
关联返回 proof 契约缺失在编译期明确诊断；concrete for 原有行为通过回归。

共享 type_pool::native_derived 受检原生派生契约，raw method table、trait impl、
vtable 与 VM 真实选中 slot 使用同一检查。新 Some 契约仅支持可信 Eq/PartialEq
和 Display：核 bootstrap 保留 prefix/TypeId namespace、实际实现 trait、槽、
Self 路径、参数模式、结果及普通 struct 字段布局；拒绝 collection/非 struct。
恶意同名 trait、well_known 指向同 namespace 的假 trait、接口交换、伪签名与
坏 layout 均拒绝；旧 None 不补造签名或改解释。原生 sentinel 的 proof adapter
仍未实现，不能把 native 普通调用守门当作 proof 执行完成。

CLI 新正例经 Eq、PartialEq、Display 和 Child(Eq) trait 参数直接调用，删除
源码后仍输出 true/true/checked/true；五种坏契约不生成归档。codec 精确保存
固定声明、路径和参数类别，旧 None 重写不补签名。默认 trait 方法具体 adapter
完成了实现路径调查，要求记录在 docs/dev/default-trait-method-plan.md，尚无
执行完成声明；Ord/Ordering、Hash、关联迭代、返回与存储仍是完整目标的待办。

最终全 workspace **1052 passed / 0 failed / 1 existing ignored doctest**；
all-targets check、strict workspace Clippy、fmt、git diff --check 通过。
日志为 `/tmp/nessa-bootstrap-workspace-final.log`、
`/tmp/nessa-bootstrap-check-final.log`、`/tmp/nessa-bootstrap-clippy-final.log` 和
`/tmp/nessa-bootstrap-fmt-final.log`。continuation 捕获/恢复继续拆接 delimiter
独立栈段，多分支复制仍归 clone。整体目标保持 active，未提交或重置工作区。


### 默认 trait 方法的具体 adapter 与 NSAM5

DefaultMethodPlan 按 implementor/trait/visible scope/method 为原默认声明生成
独立 fresh SymbolId 与具体 Function 签名，NIR 再分配独立 FuncId；metadata
使用已有函数 ID remap 发布，不让同一声明的多个实现相互覆盖。源码 Self
路径决定参数/返回类型特化，explicit trait 和 alias 不会因 TypeIndex 相等
而变成 concrete Self。对应 default/内嵌 lambda 的 Self 类型值映射保持精确。

新 ParameterAbi::TraitSelf 表示逻辑签名中的具体 Self，物理接收 proof、data。
入口先存根，再验证冻结证明与 concrete 类型；TraitCall receiver 与 extra Self
转发各自的原证明。Child→Base 投影保留 root table，因此继承默认 body 的
required 方法调用继续使用 Child 覆盖，不在 provider scope 重新取得证明。
普通 concrete 实现的 Value 入口仍剥除隐藏证明，旧裸 Trait 描述规则保持。

NSAM5 与4共享原尾部布局，仅新增参数tag2 TraitSelf+view u32；实际含TraitSelf
才写5，其余Some写4，旧None仍保持1/2/3解释。reader兼容1–5，4tag2、未知tag、
错view及签名/实现缺口受检拒绝。外层NSBC3/TPOL5/builtinABI3和opcodes保持。

默认 body 的显式 Self lambda 签名/参数位置按源路径专化并传播到 nested builder。
纯Value Function 间接调用使用逻辑CallIndirect，根据实际calleeABI与调用方scope
适配TraitSelf；bareTrait Function仍走物理proof路径保留已有选择。回调捕获a的
receiver40，other在b调用位置取得自己的2，实际返回42，没有共享错误的proof。

源码8组及删除源码后的CLI3新增组验证：defaultsum40+2、多concrete与同名traits、
explicitoverride、Child继承覆盖、default→default、extraSelf独立proof、concrete
identity返回P、Self类型值P/Q与显式traitalias保持、nestedSelf lambda、局部Self
副本。真实GC测试receiver的String仅由default closure/detached continuation保持，
actualImmix收集后仍返回42，多次恢复分别42/43，最终active栈段归零。

最终全 workspace **1069 passed / 0 failed / 1 existing ignored doctest**；
all-targets check、strict workspace Clippy、fmt 与 git diff --check 通过。
日志为 `/tmp/nessa-default-workspace-final.log`、`/tmp/nessa-default-check-final.log`、
`/tmp/nessa-default-clippy-final.log` 和 `/tmp/nessa-default-fmt-final.log`。
推断Self lambda/内嵌命名函数的完整provenance、native proof adapter、关联迭代、
Ordering/Hash与trait返回存储仍未完成。continuation捕获/恢复继续拆接delimiter
独立栈段，复制成本归属clone；完整目标保持active，未提交或重置既有工作区。


### 派生 Display 的真实函数证明调用与 builtin ABI4

NativeDerivedPlan 为新 source 的全局 struct Display 派生保存具体 Function
签名。NIR 为其生成普通 Value(self) 参数函数，调用内部 native120并检查
String 结果；复用 derived_functions/derived_methods publication 将 methods、
record、vtable 的 sentinel 替换为真实 FuncId，继承槽亦正确更新。Eq/PartialEq
原有 generated comparisons 同样经完整固定接口 proof 实际执行；没有放宽
legacy sentinel 的 proof 授权，也没有新增 opcode 或 archive metadata 格式。

新增 runtime::ids::DERIVED_DISPLAY=120、名称 __derived_display，builtin ABI
升4。native复用旧 derived_struct_to_string 字段格式（Unicode/Empty/alias
结果一致），不拿通用Any formatter代替。BuiltinCtx核 trustedDisplaycontract、
actualstructpayload/layout、执行函数为已发布全局目标、真实concrete签名和
Some(Value)无capture ABI；rawAny call不能绕过wrapper。argument与returned
String经原临时根/frames保护，分配仅在host格式化结束后发布managedString。
非法metadata名字改为受检try_get，错误payload/caller/contract明确拒绝。

发现source struct prepare_fields仅替换字段列表、遗漏TypeInfo.size；已同步
size=count×8/align=8，与实际TaggedValuepayload保持一致，修复非空Struct
native contract拒绝。字段槽布局和GC扫描未变化。Display重复derive/已有impl
诊断，不覆盖用户目标。派生Ord仍旧协议，不借此次Display改动声称Ordering完成。

loader允许旧ABI3在全部imports<120时安装，原ABI2<110/ABI1<100限制保持；
新120必须manifest精确ID/name。归档缺import、伪名、ABI3声称新native均在
执行前拒绝；旧ABI1 Display sentinel/LegacyUnknown/无schema与imports往返
不补造wrapper/proof。CLI删除source后Display精确文本及Eq/PartialEq true/false
一致。真实GC在Display receiver与返回String前后收集，detachedDisplay proof
多次resume保持结果。pipeline archive测试固定/tmp目录发生并行进程碰撞，已
改每次独立目录并检查创建/清理错误，重跑全source及finalworkspace绿。

最终全 workspace **1080 passed / 0 failed / 1 existing ignored doctest**；
all-targets check、strict workspace Clippy、fmt 和 git diff --check通过。
日志为 `/tmp/nessa-derived-display-workspace-final.log`、
`/tmp/nessa-derived-display-check-final.log`、`/tmp/nessa-derived-display-clippy-final.log`
与 `/tmp/nessa-derived-display-fmt-final.log`。intrinsic/Enum/Tuple Display、嵌套
对象完整显示、Ord/Ordering/Hash、关联迭代、推断Self与trait返回存储仍待完成。
continuation捕获/恢复继续拆接delimiter独立栈段，复制成本归clone。完整项目
目标保持active，未提交或重置既有工作区。


### std基础类型trait、真实Char常量与builtin ABI5

新增`library/std/traits.ns`，通过trusted标准库loader加载19种基础类型的
Eq、PartialEq和Display显式源码impl：12种整数、f32/f64、bool、char、String、
Unit、Type。普通函数具体Self签名、Value ABI、impl记录与vtable由现有流程
生成和归档；并非引擎bootstrap注入或reader对旧归档补造。private typed helper
`__scalar_eq`使用newstable ID121、builtin ABI5，Eq/PartialEq共享VM原
CmpEq精确比较核心，避免函数体`==`递归派发自身。Display复用TO_STRING5。
浮点NaN不等于自身、正负零相等保持既有语义，没有新增Ord/Hash或集合impl。
Any helper不授予Any/Closure/Continuation等类型trait实现。

Char原降级错误为原token String，现为NirValue::ConstChar、Constant::Char、
TaggedValue::from_char完整链路。单Unicode scalar及lexer支持的六种escape
正确解码；NSBC constants新增稳定tag9/u32LE scalar，拒绝surrogate、越界、
截断和unknown tag。外层NSBC3及NSAM/TPOL布局不变；旧tag0–8解释不变，
旧reader遇新tag拒绝。真实Char formatter输出原有裸字符，消除String原token
造成的引号。无新堆布局或GC扫描规则。

新global impl暴露两个比较分派问题：已coercion的operand必须按target actual
类型选择函数；actualnumeric两侧kind不同的普通operator保留原精确kernel，
避免f32Self函数收到f64值、或统一float导致128位整数丢精度。显式词法实现
仍保留原dispatch，bareTrait参数的不同concrete证明仍明确拒绝。

loader旧ABI4仅允许imports<121，旧3<120/2<110/1<100限制保持，ID/name精确
校验不变。source与跨进程archive覆盖标量proof、mixedSelf拒绝、ordinary函数
签名ABI/vtable精确保存、伪ABI4claim121拒绝、CharUnicode/escape及坏payload。
真实GC覆盖Stringproof闭包和detached continuation帧多次恢复，capture/resume
独立delimiter栈段拆接机制保持，复制成本归clone。

最终全workspace **1098 passed / 0 failed / 1 existing ignored doctest**；
all-targets check、strict workspace Clippy、fmt和git diff --check通过。
日志`/tmp/nessa-intrinsic-traits-workspace-final.log`、
`/tmp/nessa-intrinsic-traits-check-final.log`、
`/tmp/nessa-intrinsic-traits-clippy-final.log`及
`/tmp/nessa-intrinsic-traits-fmt-final.log`。Enum/Tuple Display、嵌套对象完整显示、
Ord/Ordering/Hash、关联迭代、推断Self与trait返回存储仍待完成；完整目标保持
active，未提交或重置既有工作区。


### 默认函数推断Self、内嵌函数身份与返回契约

新增self_provenance模块：sourceSelf参数、推断绑定、Tuple/projection、共同
if/return路径及checkedSelf方法返回传播来源；变量写入取交集，显式接口
注解/cast截断。源fn(Self)->Self上下文为省略lambda参数注解提供路径。
不按相等TypeIndex全量替换Read/Any。复杂match/loop来源仍保守，Self构造
尚未重新检查，命名函数捕获保持明确拒绝。

每DefaultMethodPlan保存nestedFunction实际签名和Self参数；NIR每adapter
独立生成命名函数，局部覆盖源Symbol映射，递归/函数值返回不共享抽象模板。
分配/生成按ASTindex排序。type阶段对具体默认method调用instantiate源Self
签名，trait发布后重绑scope正确的adapter。P/Q回调Fn签名反射、直接返回字段
访问及inner()->Type分别为具体P/Q，显式Read保持接口身份。

所有因Self改变returntype的出口追加真实TypeAssert；若FnSelf契约实际返回
捕获explicitRead的闭包，source/删除sourcearchive均TypeError且没有输出。
不是把Functionmetadata换成具体标签掩盖实际值。TraitSelf参数、proof/data
捕获及原逻辑间接调用仍复用NSAM5，不新加archive格式或builtin能力。

新增源码测试包含上下文省略参数、self.identity()来源、P/Q和递归函数、
边界注解/转换/分支/变量写入与不实return拒绝。实际GC覆盖唯一closure或
pausedcontinuation持有P{text:String}后返回真实P与完整字符串，multishot
多恢复正确；保留独立delimiter拆接和clone复制机制。archive精确保存实际
函数ID/Function签名/FunctionAbi，source删去后10项真实函数/值行为一致。

最终workspace **1109 passed / 0 failed / 1 existing ignored doctest**；
check全targets、strictworkspaceClippy、fmt与diffcheck绿。日志为
`/tmp/nessa-default-provenance-workspace-final.log`、
`/tmp/nessa-default-provenance-check-final.log`、
`/tmp/nessa-default-provenance-clippy-final.log`及
`/tmp/nessa-default-provenance-fmt-final.log`。
完整目标仍active；Ord/Ordering迁移已获用户明确选择Ordering/?Ordering与
NaN不可比较返回null，接下来落实。trait返回/存储carrier尚未获明确选择。


### Ordering/?Ordering迁移、真实排序派生与默认方法

用户明确选择Ord.cmp返回Ordering、PartialOrd.partial_cmp返回?Ordering，
NaN不可比较为null。新增ordinary std.ordering nominal enum公开less/equal/
greater，保持intrinsic/bootstrapprefix索引不变。只在trustedstd该源类型存在
时给bootstrap Ord/PartialOrd建立Eq/PartialEq父接口和准确固定签名；仿同名
untrusted类型不激活契约，rawLegacy TypePool无schema/无parent保持原状。

std.traits为16种基础类型（12integer+Bool/Char/String/Unit）写显式Ord与
PartialOrd实现，float额外仅PartialOrd；Type/Any/集合/闭包不注册排序。新增
builtin stable122 __scalar_cmp、ABI6，受检native临时根/arity2，复用精确
Number.partial_cmp_numeric及同类标量比较，返回?i64 sign。private std helper
将sign映射真实Ordering，不把TypeIndex嵌进nativeABI。helper用==-1/==1而非
<0/>0，避免新globalOrd导致递归。NaN返回null，±0equal，128bit不丢精度。

NIR signed operator消费EnumIs真实variant，Optional的null使所有关系false，
先TypeAssert准确接口返回类型，不把Ordering位模式当整数或非法Unit当false。
Struct/Enum/Tuple派生普通函数，字段调用真实sourceOrd/PartialOrd；variant
按声明序，fields lexicographic，Optional null<nonnull，less/greater/null均
短路停止后续字段；PartialOrd可回退Ord但检查其非Optional返回契约。要求
global父equality与字段排序前置，不把局部扩展升级成globalderive证据。

std.ordering提供4普通source默认body。Ord schema保存parent eq及own cmp/
lt/gt/lte/gte槽，每missinghelper实现/derive通过DefaultMethodPlan产生独立
鲜明函数身份与双TraitSelf参数。用户override保留，gte=notlt/lte=notgt组合
经原rootvtable生效；typedOrd及静态P调用保持一致，跨scope不重选provider。
TypePool追加pendingrecord方法API核exactowner/trait/scope，拒duplicate或
已published table修改，之后统一发布完整表，无元数据占位成功。

NSBC3、NSAM/TPOL布局无变化，无Ordering新role。旧ABI5仅imports<122，
旧4<121/3<120/2<110/1<100保持。源码旧integercmpfixture改Ordering.greater
并满足Eq父接口，手构legacyarchive则仍真实返回整数1、原CmpGttrue，reader
不补schema/parents/Ordering。旧5+121执行true，缺新import、假ABI5claim122、
错误name拒绝。新归档精确保留nominal结果、父槽、函数签名及doubleTraitSelf
ABI；删除source后scalar/custom/derived/NaN/defaultoverride/frozenproof行为
一致。另保metadata与ABI、仅把cmp Return改Unit，两条新process测试分别验证
typedoperator及derivedfield的实际ret contract TypeError，无输出。

源码8组覆盖全部16scalar defaults、128bit、UnicodeChar、float精度和NaN、
customcmp副作用/效应/短路、derivedEnum/Tuple/Optional、错concreteSelf拒、
A/B冻结proof及Eq父投影、真实GC和默认lt→cmp→pause多恢复。旧legacy native
structcmp保持原函数。捕获/恢复仍拆接delimiter独立栈段，clone承担复制。

最终全workspace **1130 passed / 0 failed / 1 existing ignored doctest**；
all-targets check、strictworkspaceClippy、fmt及gitdiffcheck绿。日志
`/tmp/nessa-ordering-workspace-final.log`、`/tmp/nessa-ordering-check-final.log`、
`/tmp/nessa-ordering-clippy-final.log`及`/tmp/nessa-ordering-fmt-final.log`。
Hash、Enum/Tuple Display、集合关联迭代、Self构造/复杂值流、trait返回/存储与
稳定跨包身份等完整设计仍待完成；完整目标保持active，未提交或重置既有改动。

### 关联声明前端与明确的编译边界

并行调查确认Hash尚无公共签名/Hasher协议，bootstrap槽仍无签名；具体用户
实现可调用，但typed Hash及派生尚未支持。Hash返回值型或Hasher参数型接口
已询问用户，尚未获得选择；没有据此更改Hashschema、nativeABI或旧归档。

修复文档中的`assoc name: type = value`无法解析的问题，AST使用
`AssocDecl(AssocBinding[name,type,value])`保留关联声明身份，不伪装成ConstDecl。
旧`assoc <definition>`保留原AST。准确节点布局、trait/impl/extend声明、旧形式、
缺名/类型/初值均有parser回归。名称解析检查直接声明位置，拒绝文件、函数或
嵌套函数中的assoc；规范关联绑定由于exact impl/scope绑定和签名替换尚缺，
明确报告不支持并停止产物生成。回归同时检查诊断、空codegen与归档构造失败，
避免初始化阶段忽略assoc却仍生成不完整产物。Iterator.Item及next返回协议
尚未完成，本轮只完成前端及诊断边界。同步Enum文档的排序派生与Char表示状态。

最终workspace **1134 passed / 0 failed / 1 existing ignored doctest**；
all-targets check、strictClippy、fmt与gitdiffcheck通过。日志
`/tmp/nessa-assoc-workspace-final.log`、`/tmp/nessa-assoc-check-final.log`、
`/tmp/nessa-assoc-clippy-final.log`及`/tmp/nessa-assoc-fmt-final.log`。
完整目标保持active，未提交或重置累积改动。

### 静态关联类型、TPOL6与字符模式

用户定义trait的`assoc Item:Type=Any`不再停留在unsupported前端。独立
associated_types模块预声明、准备具体默认值及实现覆盖，签名保留来源路径
`TraitAssociatedPath{trait_owner,name,path}`，与Self路径分离；显式Any不因
和default相等而替换。支持Optional/Tuple/嵌套Function及source alias路径，
绑定保存exact implementor/implementation trait/visible_scope/declaration owner。
同类型不同trait的Item、不同scope的String/i64绑定不混；继承沿真实父provider
取实际绑定，而非重取Any声明默认值。拒绝未知/重复/非Type/循环绑定、歧义父
声明、错误函数签名和依赖Self/其它关联值的抽象默认类型。

TypePool关联绑定只允许在vtable发布前登记，完整snapshot校验缺失/重复/无
exact impl/foreign owner/错误value/非法重叠或越界path；签名检查从普通
implementor API扩展到exact impl/scope descriptor，NSBC table目标逐槽受检。
绑定校验使用exact记录/key索引及声明缓存，避免对每个binding扫描全部records。
动态关联trait view及Optional/嵌套函数里的该view、关联绑定中含trait返回/存储
carrier、关联trait默认方法body均明确拒绝；source停止产物生成，NSBC ABI和
VM acquire/project保持同边界，含关联声明但无关联slot的view也不能绕过。

TPOL6仅在有新paths/bindings时启用，signature参数类别之后追加owner/name/path，
schemas后追加完整绑定表；名字UTF-8保存，读取重新intern。无关联元数据仍写5，
旧1–5读取两项empty不补造，旧revision写新数据拒绝；坏scope/identity/value/
name/path/count/truncation拒绝。外层NSBC3、NSAM、opcode及builtin ABI6不变。
CLI删除source后验证显式/default、alias/Any、不同trait同名、不同scope、前向父
实现、heap返回和多次continuation；篡改binding或目标function signature拒绝。
真实GC覆盖仅closure及暂停continuation保留的String，恢复两次仍得到长度8。

Char literal模式删除过时的resolution拒绝，沿现有真实Char常量/比较路径
实现Unicode、全部六种escape、match/matches及嵌套Enum/Tuple载荷。Any中的
String/整数/null不误匹配，静态类型不兼容诊断，scrutinee只求值一次，guard
及NoMatchingCase有回归；源码与归档往返及独立source-delete CLI均验证具体值。

最终workspace **1162 passed / 0 failed / 1 existing ignored doctest**；
all-targets check、strictworkspaceClippy、fmt及gitdiffcheck通过。日志
`/tmp/nessa-associated-workspace-final.log`、`/tmp/nessa-associated-check-final.log`、
`/tmp/nessa-associated-clippy-root-final.log`及`/tmp/nessa-associated-fmt-final.log`。
静态关联next调用不是完整Iterator/for协议，bootstrap循环尚未迁移；Hash接口
仍待用户选择。关联默认方法与dependent defaults、trait carrier、Enum/Tuple
Display、跨包稳定身份、原生SP/FP与完整async等原目标继续待完成，goal保持active。


### Enum/Tuple字段Display、帧所有状态与NSAM6

Enum和Tuple typealias派生普通fn(Self)->String，字段调用实际全局Display；
Optional和匿名Tuple递归，显式Tuple实现优先，String加引号而用户返回文本不
重复引用。缺少全局字段实现或仅有局部extend时诊断。生成调用参与模块初始化
依赖规划，修复字段方法读取另一个模块未初始化全局值的问题。用户明确选择
暂时保留struct旧派生：Any/List字段兼容，ID120和内部formatter行为保持。

Display状态归调用帧所有，实际Display调用与内联Tuple计入128层预算，普通
辅助函数不增加深度；活跃堆聚合路径检查环并作为GC根。捕获/恢复随栈段保存
状态，clone独立复制状态；真实GC与多次恢复回归验证不同分支的结果。逻辑
输出预算1 MiB在拼接、转义及实际String返回处检查。String仍有u16 payload
字数限制，约524 KB可能先ObjectTooLarge，未改变堆布局。

CompiledFunction.display_owner由resolver/NIR/codegen传递。NSAM6仅在存在
Some owner时写入，在完整ABI表后保存全部函数的owner表；shared validator
检查可信Display/global实现/FuncId、具体聚合接收者、String返回、无capture和
Value入口。旧1–5保持None，不补造；未知/重复ID、计数、presence、错误owner/
ABI/签名、截断及降版尾部均拒绝。builtin ABI7新增123–125转义与内联Tuple
helper；旧ABI6仅imports<123，旧compatibility界限和120语义保持。NSBC3及
TPOL6关联类型扩展独立，普通无新增数据仍按既有revision写入。

全workspace验证：cargo test --workspace为1179 passed、0 failed、1既有ignored；
cargo check --workspace --all-targets及严格Clippy通过。覆盖源码删除后的独立
归档、错误字段实际返回、深度/输出预算、缺import及假ABI、真实GC、多次恢复、
struct旧行为及模块初始化。日志为/tmp/nessa-display-workspace-final.log、
/tmp/nessa-display-check-final.log及/tmp/nessa-display-clippy-final.log。

此阶段完成Enum/Tuple Display，不代表项目完成。Hash接口仍待用户选择；
关联迭代、dependent默认关联类型、trait返回/存储carrier、跨包稳定身份、原生
SP/FP及完整async等原目标继续待完成。


### 关联默认类型依赖Self/其它绑定、符号声明与TPOL7

用户trait的关联默认类型现可使用源Self、同trait或父trait关联绑定，并嵌套
Optional、Tuple、Function及透明alias；前向引用不依赖声明次序。先收集显式
override及精确父实现绑定，再按依赖关系求其余默认值。无实现的默认循环可以
保存，override能打断循环；实际安装未打断循环或超过256层预算时诊断且不产
可执行代码。父值冻结准确可见的父实现，两个scope中String/i64的Item不会混用。

新AssociatedTypeExpr模板明确Concrete/SelfType/Binding及结构构造；symbolic
TypeKind::AssociatedType保存owner/name，声明assoc_types指向自己的符号叶，
不以Any/INVALID伪装默认值。源Self与显式trait名称区别保存，显式Any不替换。
DefaultResolver集中持有求值状态，无新增lint关闭。继承name准备在默认求值前
接通，实际函数签名由现有associated_paths替换并检查。shared schema要求每个
符号叶均有准确标记，遗漏路径不能授权接口。

TPOL7新增kindtag13并在revision6绑定表后追加完整默认模板树。writer有新模板
或符号类型时写7，否则沿用6/5条件；old1–6的templates为空且不fabricate。
恢复校验声明/模板对应、名称/owner/继承关系、所有Concrete类型引用、精确
具体实现绑定、结构深度及累计item预算。损坏声明叶、重复模板、未知owner/name、
不允许的具体trait视图及降版新增数据均拒绝。NSBC3、NSAM6及builtin ABI7未改。

source拒抽象Type值、实际函数签名和对象字段逃逸；NSBC validator同样拒global、
Typeconstant、执行签名及指令操作数中的符号叶。VM检查host手造描述符、反射、
转换及Optionalnull路径，不能只靠归档producer安全。具体返回ABI仍为普通值；
含trait视图的关联类型、动态关联trait carrier和关联默认方法body仍明确拒绝。

本阶段新增验证24项，全workspace为1203 passed、0 failed、1既有ignored；
cargo check --workspace --all-targets、严格Clippy、fmt及git diff --check通过。
9组source测试含300项前向默认链拒绝、alias/Function具体反射、精确scope归档
模板，真实GC验证Fn()->P闭包及continuation多次恢复的(P,?P)与String字段。
CLI共85、codec76，删除源码后专化值/反射类型相同；错误source不生成archive。
日志：/tmp/nessa-dependent-workspace-final.log、/tmp/nessa-dependent-check-final.log、
/tmp/nessa-dependent-clippy-final.log及/tmp/nessa-dependent-fmt-final.log。

本阶段要求与边界见[关联默认类型计划](associated-default-specialization-plan.md)。
项目仍未完成：关联trait默认方法body、Iterator/for关联协议、Hash、trait返回与
存储carrier、跨包稳定身份、原生SP/FP、完整async等继续待实现。


### 关联 trait 默认方法体的具体检查与冻结执行

先登记精确 impl records 和关联绑定，再为所需默认方法生成独立 adapter。
default_body 模块按每个具体实现重检方法体，专化局部类型、Type 值、cast、
Optional/Tuple、闭包和内嵌命名函数；用户覆盖的默认体不实例化。P 合法不会
掩盖 Q 的错误。隐式 lambda 返回也遵守上下文返回类型，Item=String 时 ||42
在编译阶段诊断，无可执行代码或归档；Any 边界保留实际值检查。

每个 adapter 保存当前 body 的节点域和 lowering facts，包括字段索引、
constructor/enum 信息、模式载荷、实参适配、coercion 和各类调用目标；域内
缺失不能回退到共享声明的旧结果。域外依赖读取正常声明信息。临时重检的原
facts、symbol types 和 method return inference 在成功或错误后恢复，长期
数据仅存 body 及嵌套定义，不复制整个模块图到每个 adapter。

内部 TraitSelf 证明使用精确根实现的完整关联绑定，VM 和 validator 共用检查。
签名按实际 descriptor 的 implementation trait/scope 实例化，继承投影保留
原 root table 的覆盖。Item 参数用 Value，source Self 才用 proof/data，即使
Item=Self 也保持不同物理布局。闭包捕获的原 receiver 与调用者传入的 Self
各自持有冻结证明，显式、省略注解和命名函数回归分别验证 scoped 40+2=42。
bare ParameterAbi::Trait 关联视图及用户动态返回/存储 carrier 仍不支持。

初始化扫描区分当前 body adapter 与冻结 root adapter，跟随 required 实际
函数目标和继承覆盖，将目标模块加入加载图。源码及删除源码后的独立归档
验证顶层默认调用间接读取 storage.global 的正确顺序。不同 scoped provider、
用户覆盖及错误 provider 会造成假循环的例子也验证正确实际依赖。同模块前向
全局读取仍遵循源码顺序并报告 UninitializedGlobal；另一个 Self 参数的依赖
目前保守收集同具体类型的兼容目标，尚无精确参数证明数据流，可能多计依赖。

本阶段关联默认方法只支持直接 Self 参数的证明传递。参数内部嵌套 Self 的
Tuple/Optional/Function 等需要额外 carrier，明确诊断并拒绝产物；返回闭包
的直接 Self 参数允许，Item 的结构参数及 Self 返回结构不受此限制。没有
借此改变 struct 旧 Display 行为，也没有迁移 Iterator/for 或决定 Hash 接口。
NSBC3、NSAM6、TPOL7 和 builtin ABI7 未新增格式或标签。

全 workspace 最终验证为1226 passed、0 failed、1既有ignored；check all-targets、
严格 Clippy、fmt 与 git diff --check 全部通过。CLI93及codec76通过，源码
回归含不同字段布局/声明顺序、准确 Function 反射、静态与实际错误、真实 GC
和 continuation 多次恢复。日志为 /tmp/nessa-associated-body-workspace-final.log、
/tmp/nessa-associated-body-check-final.log、/tmp/nessa-associated-body-clippy-final.log
及 /tmp/nessa-associated-body-fmt-final.log。

阶段要求与边界见[默认方法计划](default-trait-method-plan.md)。项目目标继续：
Iterator/for 关联协议、Hash、trait 返回/存储与嵌套参数 carrier、精确初始化
参数证明数据流、跨包稳定身份、原生 SP/FP、完整 async 等仍未完成。

### 初始化 receiver 证明别名与迭代协议决策

修复 `let copy=self;copy.next()` 的初始化假循环：按具体 body 收集 receiver
证明及未被写入的简单别名链，按冻结 root 选择目标，而非加载全部 scoped
provider。receiver 自身及别名赋值（含闭包内写入）均使该推断失效。收集复制
边后线性传播，避免对逆序链重复扫描。直接 Self 调用也遵守 receiver 写入
失效规则。独立 Self 参数仍保守规划，不声称完整参数证明数据流已实现。

源码回归2项、删源归档回归1项新增；全 workspace 为1229 passed、0 failed、
1既有ignored，check all-targets、严格 Clippy、fmt、diff 检查全部通过。独立
verifier 首轮复现 receiver 写入遗漏，root 修复后第二个 verifier 全项重验，
适用 rubric11/11通过。完整证据、限制与运行记录见
[初始化证明验收](initialization-proof-audit.md)。旧归档格式和函数 ABI 未改变。

Iterator 调查确认 Optional 的 null 结束标记与合法 List null 元素冲突。用户
明确选择单次 next 与带标签迭代结果，要求正确 Item 载荷类型，不能以 Any enum
或限制 null 元素代替。当前规范示例、bootstrap schema、for lowering、List
与归档协议仍需迁移；[迭代协议计划](tagged-iterator-plan.md)记录要求和缺口。
这是下一阶段的真实目标，尚未完成。其他原目标继续 active。

### 静态单步迭代、Map快照与组合模式的后续验收

新的 Iterator 必须绑定准确 Item，next 返回 IterationStep(Item)；IntoIterator
绑定具体 Iter 并检查其 Iterator 实现。for 每次只调用一次 next，不匹配的模式
跳过元素。List 的 Item 为 Any，null/Unit 是正常元素；Map 的 Item 为
(String, Any)，keys/values/entries 和迭代使用浅快照与独立游标，顺序未承诺。
旧归档按原协议执行，不在加载时补造新接口。具体 ABI/版本、真实 GC、多次恢复
和删除源码归档证据分别见[single-next验收](single-next-iterator-audit.md)与
[Map验收](map-iteration-audit.md)。这些已完成路径取代上述历史段落的待实现状态。

源Enum的可选默认字段与单List变参已贯穿共享实参计划、NIR、初始化、默认方法
adapter、展开/控制边界、GC和归档；普通match与真正handler的控制上下文区别
已修复。仍不支持first-class constructor、解构字段、双变参或泛型Enum。
[Enum默认验收](enum-default-audit.md)保留独立首轮失败、修复及第二轮12/12通过。

match、matches、for现支持嵌套or/as：备选分支共用SymbolId且名称/准确类型一致；
独立guard按绑定次序解析，失败不泄漏未初始化的绑定。as在成功后保存整个输入，
Any别名保持Any。短路与部分失败后的重绑定、闭包、具体关联默认方法、冻结trait
receiver、真实GC/continuation和独立归档通过端到端验证。声明和参数的完整模式、
not、List/struct模式及穷尽性分析仍待完成。

当前全workspace1313通过、0失败、1项已有doctest忽略；fmt、all-targets check、
严格Clippy及diff检查通过，独立验收者另行复跑结果一致。组合模式首轮rubric12/12
通过，无产品修复轮。完整范围与证据见[组合模式验收](composite-pattern-audit.md)。
公共动态trait carrier、Hash、泛型、跨包身份、原生SP/FP、完整async等仍属于
整个项目的未完成目标；测试全绿不等于这些设计已实现，goal继续active。

### 动态 List 高阶方法与回调边界

map/filter/fold/each/foreach 已接通当前 List/Any 契约；按输入浅快照顺序调用，
null/Unit、空列表与元素共享正确。遍历 index 和标量 accumulator 在调用帧内，
continuation 两次恢复分别15/25、进入次数5、合计42。间接调用检查实际 Fn 参数、
数字表示和调用位置返回类型，Unit 结果即使忽略也检查；原 trait proof 路径保持。
初始化跟踪实际调用的回调、简单无写入别名及捕获参数的闭包，修复软依赖环中
UninitializedGlobal；复杂动态函数指针流仍由运行时守卫处理。

隔离行为/GC/删源归档测试与生产修复并行；独立审查发现连续参数转换的临时根
缺失，修复注册scratch slots并以两次堆i128转换之间强制GC验证。当前全仓1337
通过、0失败、1项已有忽略，全部质量门通过，独立第二轮11/11适用项通过。
C4无外部研究不适用；C1/C2/C7/C8为运行记录核对。范围及证据见
[List回调验收](list-callback-audit.md)。泛型、完整tacit lambda/尾随do、Hash、
公共trait carrier与其它原目标继续待完成，goal保持active。

### 尾随 do 回调完整调用路径

裸callee do lambda、已有Call追加callback及零参数do block已在resolution前
正规化为Call/Lambda；rawparser仍保留PostDo，原wrapper/callee/实参/body身份
和span保留。共享实参计划、类型上下文、eval顺序、控制边界、闭包capture、
初始化实际回调依赖、associated/default adapter均复用，不新增ABI或归档布局。

两个隔离测试作者同时覆盖行为与GC/归档，生产代码唯一owner整合，峰值3个
并发agent。关联Item i64/String、冻结Selfproof、init软环、实收集5/4/4/7次、
multishot15/25/entries5/final42/stack0与删除源码新进程归档通过。全workspace
1362通过、0失败、1项已有忽略，全部质量门通过，独立首轮11/11适用项通过。
C4无外部研究不适用；C1/C2/C7/C8为记录核对。完整证据见
[尾随do验收](post-do-audit.md)。这取代上一阶段的尾随do待实现状态；完整tacit
lambda、泛型、Hash、公共carrier、跨包稳定身份及其它原目标继续待完成。

### not 模式与否定内部作用域

文档的not关键字模式接通match/matches/for，反转整个子模式及其guard，
否定内部绑定仅供内部guard及其捕获使用，不导出到外部成功路径。not优先级
高于as，外部alias保留输入准确类型与trait proof。旧前缀!的错误模式标签
已与逻辑否定分开，仍保持未实现诊断；裸Enum分支的for名称解析缺口已修复。

两个隔离测试作者并行，root唯一合并，11组行为、4组边界、3组GC、4组
删源新进程归档测试通过。关联Item i64/String、初始化实际回调依赖、冻结
Self proof40+2、多次恢复21/21及trace132、充分完成GC和活动栈0均有精确断言。
全workspace1384通过、0失败、1项已有忽略，全部质量门通过。独立验收复跑一致，
首轮11/11适用项通过；C4不适用，C1/C2/C7/C8为运行记录核对。
记录见[not模式验收](not-pattern-audit.md)。其它原始设计目标保持未完成。

### and expr is 约束模式

文档的pattern and expr is pattern接通match/matches/for，以三个AST子节点
区分左模式、计算表达式、右模式。左侧成功后才求右侧表达式一次，按计算结果
的准确类型匹配并保留快照/proof；左右成功绑定按遇到顺序向后可见，提前读取、
重复绑定、or契约不一致和非法声明/参数均拒绝。and左结合，优先级高于or/guard，
低于as/not；原始输入与计算结果的alias各自正确。

两个隔离测试作者与root实现并行，11组行为、4组边界、4组GC、5组删源归档
全部通过。关联Item i64/String、冻结RHS receiverproof40+2、初始化实际回调
软依赖、完成GC、RHS表达式/guard multishot20/22及trace123/1234和活动栈0
均有精确断言。root全workspace1408通过、0失败、1项已有忽略，全部质量门
通过，独立验收复跑一致，首轮11/11适用项通过；C4不适用，C1/C2/C7/C8为
运行记录核对。用户确认左右成功绑定导出规则。详见[约束模式验收](and-is-pattern-audit.md)。整体目标
仍保持未完成，未将专项通过替代全设计审计。

### List 模式与具名 rest

List/Any的List模式接通match/matches/for，准确/最小长度检查、固定字段Any、
rest List、单个具名三点rest任意位置。用户确认浅快照：每次List节点尝试先
检查身份/长度并复制所有槽位再执行字段guard；元素对象共享，rest独立wrapper，
后续arm重新读取原List。or/as/not/andis/guard绑定契约和for跳过失败保持。

两个隔离作者与root实现并行，10组行为、4组边界、5组GC、6组删源归档通过。
关联Item i64/String、冻结Selfguardproof40+2和初始化实际回调软依赖有精确
证据。GC完成数充足、活动栈0；for的heap游标按共享语义20/0及trace11，
fold的frame游标独立20/22及trace111，两种状态区别验证。全workspace1433
通过、0失败、1项已有忽略，全部质量门通过，独立验收恢复后复跑一致，首轮
11/11适用项通过。写入配额耗尽后按用户授权清理1.3GiB临时构建缓存，失败与
恢复日志均保留；C4不适用，C1/C2/C7/C8为运行记录核对。详见
[List模式验收](list-pattern-audit.md)。整体目标仍保持未完成。

### Continuation 不可达暂停栈回收

捕获/恢复继续只修改 delimiter 栈段链接。TaskStacks 区分 raw/pending 强根与
language-owned 模板；GC 通过弱 wrapper 所有者按不动点扫描暂停链，所有引用
闭包完成且不再新增所有者后才释放不可达链。无外部根的 self/cross cycle
在任务结束前回收，可达循环、链式引用、原始句柄及待发布分支保留。移动对象
写回 weak owner 和所有 captured 槽；clone 独立所有权，once 消费并重接原链。

隔离 runtime 与 GC 测试作者并行，root唯一合并，runtime36/36、生命周期9/9，
32次实际完成GC，weak-only wrapper及captured String实际地址变动均有断言。
全workspace1446通过、0失败、1项已有忽略，全部质量门通过；独立验收复跑
一致，首轮11/11适用项通过。C4不适用，C1/C2/C7/C8为运行记录核对。
普通寄存器/local slots仍保守扫描，精确源码生命周期与其他原始目标继续未完成。
详见[暂停栈生命周期验收](continuation-lifetime-audit.md)。

### Effect 恢复输入与 handler 退出检查

稳定 catch/alias/clone/captured closure 根据 effect 返回类型检查输入；写入绑定
保守撤销精化。普通 handler 有独立 return/resume 推断与检查边界，所有提前
退出、guarded 退出和尾值都受约束，嵌套 lambda/handler 不污染外层返回推断。
标准库 Continuation 透明别名的字段 clone 也已修复。

弱所有者附带 effect 返回类型与捕获 scope；语言恢复在 fork/once 消费前检查
并转换，失败保留模板/pool，scope 恢复调用点；clone 继承契约，Any/擦除存储
仍有 runtime guard。捕获/恢复继续链接栈段，原始线性 ABI 及归档格式未变。

隔离 runtime7组、独立行为9组（15次完成GC）、归档3组全部通过；异构 answer、
延迟/再次捕获、typed heap/narrow/optional/Unit 和无效恢复所有权都有具体断言。
全workspace1465通过、0失败、1项已有忽略，全部质量门通过；独立验收复跑
一致，首轮11/11适用项通过。C4不适用，C1/C2/C7/C8为记录核对。精确answer
类型、effect参数协议和其它原目标继续未完成。
详见[Effect契约验收](effect-contract-audit.md)。


### Effect 默认、具名和单 List 变参协议（实施中）

已知 effect 调用共享声明参数计划，显式实参按源序快照，再绑定具名、默认值和
List 变参。catch 从调用者槽排除，仍按完整声明位置注入；任意位置均有验证。
默认值使用声明作用域和此前调用者绑定，禁止引用 catch/self/later 参数，检查
未使用默认值、非法变参布局、控制边界和选择默认值的递归/init依赖。
签名注解与默认表达式分阶段；默认值检查推迟到普通函数返回推断之后。
不改变 opcode、NSBC 格式或 delimiter 栈段捕获/恢复 ABI。

隔离行为13组、GC4组（21次实际收集）、归档9组通过；全 workspace1493通过、
0失败、1项已有忽略，全部质量门和独立复跑通过。但独立验收 C11 仍有 major
缺陷：两层无返回注解函数按不利声明顺序时留下旧 Any，非法未使用默认值仍可
生成归档。两轮验收10/11适用项，既定循环 capped，本专项尚未通过。
后续修一般返回类型依赖推断，不降低默认值类型要求，完整目标继续保持未完成。
详见[Effect参数协议记录](effect-parameter-audit.md)。

### 函数返回类型的无环依赖推断

静态函数与变量初始化式先按依赖准备，词法块在参数与模式绑定类型建立后
准备；函数头和函数体按事实上下文缓存。用户选择块内具名函数前向可见，
局部变量仍按源序可见；具名函数捕获局部值仍明确拒绝，可执行捕获使用lambda。
无注解循环暂保留渐进Any，完整循环方程推断仍待完成。

独立新增行为14/14、删源归档3/3通过，修复此前两层无注解函数默认值漏检。
但root全workspace发现默认trait体内部具名函数的Item签名未按P/Q重放具体化；
独立只读探针还发现未标注factory receiver的512边方法链走递归推断而栈溢出。
两项均交回原实现作者隔离修复，独立测试作者补回归，尚未验收通过。
记录见[函数依赖推断](function-inference-audit.md)，完整目标继续未完成。

修复已合入：词法/具体重放上下文准备内部函数头，factory结果建立后把新方法
依赖加入显式工作栈；块内扩展仍保留。独立行为16/16、归档5/5通过，root全
workspace1525/0/1及全部质量门通过。最终独立复跑一致，首轮11/11适用项通过；
原参数行为13、GC4（21次收集）、归档9及原失败复现均独立通过。旧参数专项
capped历史保留，其无注解链漏检根因由本专项修复。C4不适用，C1/C2/C7/C8
为记录核对；全部原设计目标仍未完成。

### Optional 传播、非空模式与直接 unwrap（实施中）

实际基线中get()?错误地产生Unit，null?后仍执行剩余函数体。核心resolver/NIR
修复已合并：求操作数一次，保留准确载荷，null退出当前callable，推断纳入
隐式null返回；some模式贯穿match/matches/for，直接Optional.unwrap保持载荷
并在null时panic。普通用户unwrap与Any动态方法保留现有分派。

生产局部单元resolver131、parser71、NIR22项及目标Clippy通过，独立行为/GC/
归档检查正在执行。null消除块正常完成的行为等待用户明确选择，共同提案未
合并；绑定unwrap函数值仍明确拒绝。本阶段未独立验收，完整目标继续未完成。
详见[Optional实施记录](optional-flow-audit.md)。

用户随后明确删除整个 `value? { ... }` 消除块形式，认为它容易误解且多余。
旧共同实现只在隔离树验证、未合并；当前保留单一后缀传播语义，生产/测试
回到core范围并添加被删除语法的明确拒绝和条件块回归。旧待决项已经撤销，
不以过去的消除块通过用例声称新的范围已验收。

最终按用户范围合并core12个Rust路径与独立3个测试文件。独立行为14/14、
真实GC3/3（18次完成收集）、删源新进程归档3/3通过；root workspace1552/0/1
及全部质量门通过。删除消除块无新增AST/opcode/NSBC布局，capture/resume仍
链接栈段。独立grader尚待验收，完整项目继续未完成。

独立grader最终复跑1552/0/1、质量门与专项14/3/3一致通过，额外条件/混合
数值/Any/enum默认值探针有具体值、类型或诊断与noartifact。用户明确修订的
范围首轮11/11适用项通过（C4不适用，C1/C2/C7/C8记录核对）；旧消除块范围
从未通过，历史保留。一般双变参、泛型、Hash公共契约、Error表示及其它目标
继续待完成。

### 双 List/Map 变参与扩展调用（验证中）

已合入共享参数计划与 NIR 调用降级：两个调用者槽严格为 List 然后 Map，
callee/receiver 在前，children 和属性值按混合源码顺序各求值一次并快照。
重复属性按用户选择全部执行，最后值覆盖；键是字面字符串。self/catch
保持既有隐式边界，trait 签名使用已有 MapVariadic 标记，不新增字节码格式。

独立测试正在最终源码上检查真实 513 条扩展调用依赖、具体 Self/Item 默认体、
初始化回调选择、continuation 多次恢复、完成 GC 和删源归档。root 尚未完成
全 workspace 与独立 grader 验收，不能据局部验证声称阶段通过。间接 Fn 值
的扩展打包、双变参 enum、匿名 Object、一般方法默认参数及其它完整目标
仍待完成。详见[双变参实施记录](dual-variadic-audit.md)。

双变参最终独立专项15/3/4通过，真实GC21次；root与独立workspace均1579/0/1，
全部质量门通过。独立九个额外CLI探针及baseline/hash/patch核对一致，首轮
11/11适用项通过（C4不适用，C1/C2/C7/C8记录核对）。本专项完成，前述
间接packing元数据、dual enum、匿名Object等及全部剩余设计目标继续未完成。

### 包身份、依赖 DAG 与锁文件（验证中）

已合入完整 TOML 清单保留、版本1 Merkle128 包身份、确定性依赖回溯和严格
锁文件复用。未知元数据参与身份，只有自身 package.version 排除；选中的
子包身份递归参与，具体版本由 lock 固定。源格式与注册顺序不影响结果。
实现拆为 manifest/identity/version/resolver/lock/error 模块，旧清单投影 API
保留；深依赖与搜索使用显式工作栈，清单在搜索分支间共享。

独立测试正在执行最终源码；工作区质量检查和独立验收待完成。用户已明确
最后稳定版本显式提供、缺省包版本，以及临时源码按规范化内容独立身份。
当前包管理基础尚未接入 driver/源码 TypeId/归档来源验证，也没有修复 Error
构造落到 Unit 的缺陷；完整目标仍在进行。详见[包身份实施记录](package-identity-audit.md)。

包身份最终独立24/24通过；513包与4097包实际解析/锁重放成功。root工作区
1610/0/1及全部质量门通过，独立grader待验收。此范围仍只完成包管理基础。

包身份独立grader复跑1610/0/1、专项24/24、全部质量门和完整128位重算
通过。首轮过时包hash TODO导致C10 minor，窄修文档后第二轮11/11适用项
通过（C4不适用，四项运行记录核对），原失败保留。包基础专项完成；
源码TypeId、身份来源归档验证、driver包编排及完整Error执行继续待完成。

### 稳定源码 TypeId 与 TPOL11 输入（已独立验收，下文保留过程）

包身份完成后，正实施源码/标准库上下文、有效稳定版本、有限递归名义图
身份计算和原子发布，以及TPOL11持久化输入重计算。三个隔离writer并行，
源码TypeId最终实现与归档损坏/旧版本/删源测试尚待执行，不能据协议和
已有128位存储声称完成。详见[当前实施记录](stable-type-id-audit.md)。

用户明确授权今后自主cargo clean。配额耗尽后实际清理20.8GiB恢复工具，
新增低debug/noincremental构建profile及flock+6GiB自动clean入口，授权和
统一调用方式已记入AGENTS.md；不修改宿主权限或删除源码/验收产物。

TypeId池与源码接入已合并，受影响单元与663个既有集成测试通过。独立
类型/归档fixture修正后20/7通过，正在补完整std身份与reader抽象执行拒绝
回归；全workspace与独立验收尚未执行。保留首败记录，不把局部通过当作
整个类型系统、Error或项目完成。


稳定 TypeId / TPOL11 最终合并检查：根工作区1668测试通过、0失败、1项既有
忽略，fmt/check/严格 Clippy 通过；源码来源与空 SourceMap 诊断修复已合并。
独立验收待完成，详细历史与最终日志见 stable-type-id-audit.md。完整 Error、
源码 newtype 等仍是后续任务；该阶段不代表整个项目完成。


稳定身份专项独立验收11/11适用项通过（C4不适用，C1/C2/C7/C8为记录核对）。
全新 grader 复跑1668/0/1及全部质量门，通过独立完整128位重算、TPOL11
删源执行、篡改拒绝与finalized抽象Holder限制。导出补丁的无末尾换行标记
已修复并独立应用/readback验证；生产实现不变。完整 Error 下一阶段调查
正在进行，项目总目标继续 active。


### 完整 Error 路径（本阶段已验收）

TypeId/TPOL11 独立验收后，已在当前CLI复现Error构造/传播返回Unit的缺陷，
冻结完整静态/运行时/GC/持久化验收，并启动三个隔离writer：静态规则、
backend、独立行为/GC/删源归档测试。详见[Error实施记录](error-flow-audit.md)。
静态与后端实现已按封存哈希合入，真实128位标签区分成功与错误，静态集合、
构造、传播、消除和模式贯通运行时及NSBC4。普通根可写回移动地址，native
复制引用仅在调用期固定，根句柄可重新加载；修复跨分配载荷与receiver重载。
嵌套handler的单次continuation优化现须证明不发生捕获，capture/resume仍
拆接多栈，独立分支才复制。

根与新上下文验收者的全仓格式、编译、1753项测试和严格Clippy均通过，
0失败、1项已有ignored；独立行为21项、GC10项和删源归档8项通过，真实
分配压力下观察到对象移动。固定阶段标准14/14适用项通过，C1/C2/C7/C8为
运行记录证据。泛型、newtype、包链接、异步与原生栈切换等完整目标继续。

### 包源码与文件系统模块输入（本阶段已验收）

新增受检 `PackageSources` 加载器，保留完整 manifest 与原始文本；按包类型选择
入口、稳定排序模块树、保留嵌套 main/lib，并显式拒绝重名、缺入口、非法名称、
无效 UTF-8、祖先符号链接循环和超限输入。普通文件类型在打开前检查，防止 FIFO
manifest 阻塞。独立修复版 13 个单元、24 个身份、22 个文件系统测试及格式、
严格 Clippy 通过；根全仓 1775/0/1 与格式、编译、严格 Clippy 全部通过，
新上下文独立复跑以上检查及 4 个追加边界测试，首轮 11/11 适用项通过。
C4 不适用，C1/C2/C7/C8 为运行记录核对；完整项目目标仍在进行。

这补齐文件输入生产者，完整跨包编译、CLI 和库链接仍待继续。详见
[包源码实施记录](package-source-audit.md)。

### 多文件源码位置与原始文本（本阶段已验收）

新增统一的原始字节到 SourceMap 坐标映射。lexer 诊断与 parser 完成阶段各在
生产边界转换一次，语义诊断不再重映射；保留原始源码、字面量与宏文本。AST
记录每个节点的原始范围和所属文件，追加文件后的调试输出读取正确来源。
BOM 仍报词法错误，其零宽位置带有文件归属；两个渲染器对无标签主位置也
显示文件和行列，同时保留无来源 dummy 与越界位置的普通文本诊断。

根及独立新上下文全仓 1794/0/1，格式、编译、严格 Clippy 通过；独立源码位置
17 项与附加边界 7 项通过。CLI LF/CRLF 都报 2:5 且输出字节一致，BOM 报 1:1。
第二轮 11/11 适用标准通过（C4 不适用，C1/C2/C7/C8 为记录核对）。
完整包编译、依赖来源与库链接继续；完整项目目标未完成。详见
[源码位置实施记录](source-span-audit.md)。

### 受检 AST 源码单元合并（本阶段已验收）

增加存储索引、子节点范围与环验证，以及先预检、后合并的批量接口；容量统计
包括哨兵与保留槽位，畸形或超限输入返回错误，保留目标原始文本和节点来源。
验证深度不依赖递归；批量追加只扫描目标一次和每个来源一次。std 的九个
模块正文已接入真实批量路径，失败通过普通诊断停止加载。

根和独立验收者全仓 1798/0/1，格式、编译、严格 Clippy 通过；独立边界 7 项、
std/源码位置/稳定身份专项 43 项通过，11/11 适用标准通过。实际 OOM 未强制
注入；验证图在顺序实现后、独立验收前固定。完整项目目标仍在进行，继续
文件系统模块组装和包编译管线。详见[AST 合并实施记录](ast-assembly-audit.md)。

### 文件系统包的真实编译管线（本阶段已验收）

Driver 已分别解析各文件并装配逻辑模块树，再复用单文件编译后端；空声明连接
文件并保留可见性，正文或其它同名绑定冲突报错。exe 在解析后验证根零参数 main，
lib/tmp 仅启动初始化。目录 CLI 支持 run/check/build 和 AST dump；NSBC 可在删源
后执行，失败不会写归档。物理文件位置及原始字面量保留，诊断统一输出 stderr。

初版根及新上下文全仓 1830/0/1 与质量检查通过，独立 21 个行为、10 个 CLI
测试通过，但额外边界发现 private 祖先模块可见性泄漏，首轮 C11 未通过。
现已补名称、导入及关联访问的命名空间边界和目录/内联回归。根和独立新副本
全仓 1832/0/1，格式、编译及严格 Clippy 均通过；原样 7 个边界测试与 38 次
CLI 操作通过，第二轮 11/11 适用标准通过，C4 不适用，C1/C2/C7/C8 为记录核对。
首轮失败与预算封顶记录保留，窄修复图在写入前固定；真实系统 OOM 未强制注入。
完整项目目标未完成；依赖来源、跨包编译与外部库链接继续。详见
[包编译实施记录](package-compiler-audit.md)。

### 跨包源码编译（实现后正在独立验收）

新增调用者持有的源码 catalog 入口，以完整清单选择确切依赖图，并将各包
源码装配后复用真实编译与 NSBC 后端。外部包独立词法边界、直接依赖导入、
独立弱 prelude、原始类型身份与按实际使用初始化已接入。独立测试首轮发现
模块符号重复创建，已修复并原样复跑 11/11；根补了同名真实模块身份碰撞
回归及嵌套导入加载根初始化对照，专项 13/13。全仓质量和新上下文验收仍在
进行，以实际工件为准。依赖获取、目录 CLI 的依赖来源和外部归档链接尚未
完成，完整项目目标保持进行。详见[跨包源码编译记录](cross-package-compiler-audit.md)。
