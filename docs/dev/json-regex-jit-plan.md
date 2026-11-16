# 内嵌 JSON (`derive Json`)、FFI 正则与分层热点 JIT 设计方案

本文档定义 Nessa 下一阶段的三大核心能力设计与实施蓝图，供工程实现代理按 [AGENTS.md](../../AGENTS.md) 的阶段验证与并行开发规范直接落地：

1. **内嵌高性能 JSON 与 `derive Json for MyType`**：在运行时内嵌原生 RFC 8259 Tape 解析器与缓冲序列化内核，并在编译器打通组合式受检 `derive Json`，支持静态类型直解/直写与 `.nsbc` 独立归档执行。
2. **通用安全 FFI 子系统与高性能 `regex` 库**：建立遵守 Immix 移动 GC 与定界延续边界的零拷贝只读借用 FFI / 内嵌 WASM 通道，基于批量 Span Buffer 协议在语言内核之外提供线性时间 $O(N)$ 正则库。
3. **分层热点 JIT（含循环回边 OSR）**：面向字节码解释器的紧急热点兜底优化，采用“外部化 `StackContext` 帧 + 寄存器缓存 + 精确 Safepoint 同步/去优化（Deopt）”架构，在不破坏移动式 GC 与多提示符定界延续（Continuation）的前提下，通过函数入口编译与循环回边栈上替换（OSR）消除热点性能悬崖。

---

## 0. 现状基线与全局不变量

实现前必须核对并遵守以下仓库现状与硬性约束：

- **现有 JSON 与模式匹配基线**：
  - [library/std/json.ns](../../library/std/json.ns) 目前是纯 Nessa 逐字节递归下降实现，解析为动态 `Map`/`List`，存在解释器逐字节分派开销大、字符串切片频繁分配及受限于 `Map` 8192 桶上限（见 [data-type-layout.md](runtime-representation/data-type-layout.md)）的问题。
  - [library/std/pattern.ns](../../library/std/pattern.ns) 与 [pattern.rs](../../engine/initialization/src/builtin_fns/pattern.rs) 是 Lua 5.4 风格轻量模式匹配，并非支持交替 `|`、分组量词与自动机复杂度保证的完备 Regex，保留其兼容语义，不与新增 `regex` 包混同。
- **现有 `derive` 管线基线**：
  - `Eq`、`PartialEq`、`Ord`、`PartialOrd`、`Display`、`Hash` 已在 `engine/resolution/` 与 `engine/nir/` 实现组合式受检函数降级（参见 [derivation.md](../type-system/trait-implementation-extension/derivation.md)、`display_derivation.rs`、`hash_derivation.rs`），生成正规受检函数并写入 `.nsbc` 归档。`derive Json` 必须沿用该架构，禁止使用仅在源码模式生效的反射钩子或绕过类型检查的 sentinel。
- **GC 与根扫描红线**：
  - `VmRoots` 与 `StackContext::visit_slots_mut` 按 `TaggedValue` 精确扫描并原地更新移动后的堆对象地址。**严禁**将裸主机指针、Rust 结构地址或未标记任意整数直接存入托管 `TaggedValue` 槽位；主机侧状态统一存入 `VmState::resources`（见 [resources.rs](../../engine/interpreter/src/resources.rs)）并通过受检句柄引用，或使用显式注册为 No-Scan Leaf 的字节缓冲对象。
- **Continuation 栈段红线**：
  - 定界延续（`ResetClosure`、`EffectCallDyn`、`ResumeContinuation`、`Continuation.clone`）通过分离、重接与克隆 `StackContext` 链工作（见 [continuation-stack-abi.md](continuation-stack-abi.md)）。JIT 与 FFI 不得在宿主 C 栈上隐式持有可跨越效应挂起点的托管帧状态。
- **构建缓存预算约束**：
  - 根 [Cargo.toml](../../Cargo.toml) 与 `scripts/cargo.sh` 设有共享 `target/` 与默认 6 GiB 自动清理阈值。引入新依赖（尤其是 JIT 或正则原生库）必须体积可控、编译迅速，禁止引入 LLVM 等巨型编译期依赖。

---

## 1. 内嵌高性能 JSON 与 `derive Json` 设计

### 1.1 语言层接口与 Trait 契约

1. **内建 Trait 注册**：
   - 在 `engine/type_pool/src/lib.rs` 的 `WellKnownTypes` 中新增 `pub json: TypeIndex`，并在 `register_well_known_traits` 注册 `Json`。
   - 在 [library/std/builtin.ns](../../library/std/builtin.ns) 导出 `pub typealias Json = .Json'builtin`，并在 [library/std/prelude.ns](../../library/std/prelude.ns) 预导入 `Json`，使用户可直接编写 `derive Json for MyType`。

2. **统一受检错误集 `JsonError`**（扩展 [library/std/json.ns](../../library/std/json.ns)）：
   保留现有词法/语法错误变体，新增静态类型解码诊断变体（包含精确 JSON Path 如 `$.users[2].age`）：
   ```nessa
   pub enum JsonError {
       -- 词法与语法层（保留兼容）
       pub UnexpectedEof(offset: i64),
       pub UnexpectedToken(offset: i64, message: String),
       pub InvalidNumber(offset: i64, message: String),
       pub InvalidString(offset: i64, message: String),
       pub InvalidEscape(offset: i64, message: String),
       pub TrailingCharacters(offset: i64),
       pub UnsupportedValue(message: String),
       pub DepthLimitExceeded,
       -- 强类型编解码层（新增）
       pub TypeMismatch(path: String, expected: String, actual: String),
       pub MissingField(path: String, field: String),
       pub UnknownField(path: String, field: String),
       pub InvalidVariant(path: String, type_name: String, variant: String),
       pub IntegerOutOfRange(path: String, value: String, target_type: String),
       pub CycleDetected,
   }
   ```

3. **用户可见方法与函数签名**：
   当类型 `T` 实现或派生 `Json` 后，同时具备实例序列化方法与类型关联反序列化函数：
   - `val.to_json() -> !JsonError String`
   - `val.to_json_pretty(.indent: i64 = 2) -> !JsonError String`
   - `T.from_json(input: String, .allow_unknown_fields: bool = true) -> !JsonError T`
   - `std.json` 模块同步提供泛型/统一包装：
     - `json.encode(val: Json, .indent: i64 = 0) -> !JsonError String`
     - `json.decode(target: Type, input: String, .allow_unknown_fields: bool = true) -> !JsonError Any`（配合 `.as(T)` 或直接调用 `T.from_json(input)` 获得静态类型 `T`）
     - 既有动态接口 `json.parse(input: String) -> !JsonError Any` 与 `json.stringify(val: Any, .indent: i64 = 0) -> !JsonError String` 保持签名不变，内部切换至原生内核。

### 1.2 原生内核：Tape 解析器与流式 Writer

避免在解析阶段构造中间 `Map` / `List` 堆对象，在 `engine/interpreter/src/json_kernel.rs` 实现两套无 GC 指针的主机侧数据结构，并托管于 `VmState::resources`：

#### A. 原生 `JsonTape`（单遍扫描、零托管对象分配）
- **输入扫描**：直接借用 Nessa `String` 底层的只读 UTF-8 切片 `&[u8]`，使用 SWAR / 16 字节块加速跳过空白字符与无转义字符串段，严格执行 RFC 8259 校验（控制字符 `< 0x20` 拒绝、UTF-16 高低代理对解码、前导零拒绝、128 层嵌套上限检查）。
- **紧凑节点表示**（每个节点固定 16 字节或双 `u64`）：
  ```rust
  #[repr(C)]
  pub struct TapeNode {
      pub kind: TapeKind,       // Null, False, True, Int64, Uint64, Float64, StrBorrow, StrDecoded, Array, Object
      pub flags: u8,
      pub count_or_len: u32,    // Array 元素数 / Object 键值对数 / 字符串字节长度
      pub payload: u64,         // 数值位模式 / 原输入字节偏移 / arena 偏移 / 容器结束跳转索引 next_sibling
  }
  ```
- **字符串零拷贝策略**：
  - 无转义字符串（`StrBorrow`）仅记录在输入切片中的 `(offset, len)`；当 `derive Json` 匹配结构体字段名时，直接拿主机切片 `&input_copy[offset..offset+len]` 与目标字段名对比，**全程零 Nessa `String` 分配**！只有最终落入目标 `struct` 的 `String` 字段才调用 `heap` 分配一次托管 `String`。
  - 含转义字符串（`StrDecoded`）在单遍扫描时解码追加到 `JsonTape` 自带的 `decoded_arena: Vec<u8>`。
- **对象键查找**：字段数 $\le 16$ 时直接按紧凑偏移线性比对长度与字节；字段数 $> 16$ 时在 Tape 旁按需惰性建立开放寻址小索引表 `Vec<(u32, u32)>`，消除大对象 $O(K^2)$ 查找。

#### B. 原生 `JsonWriter`（单缓冲写出、环检测与深度预算）
- 维护单一 `Vec<u8>` 缓冲区、缩进状态、128 层深度计数与活跃聚合对象地址集合（用于环检测，命中返回 `CycleDetected`）。
- 标量格式化使用栈上快速整数/浮点转十进制（拒绝 `NaN`/`±Inf` 并返回 `UnsupportedValue`），字符串转义通过批量安全字节区间 `extend_from_slice` 写出，直到 `finish` 时**一次性分配单个 Nessa `String`**。
- **Continuation 与异常清理**：`JsonTape` 与 `JsonWriter` 存入 `VmState::resources` 的作用域受限表。编译器生成的 `to_json` / `from_json` 在出口（含错误提前返回路径）必定调用 `close` builtin；同时在任务销毁或未捕获 VM 错误展开时自动回收残留句柄。若自定义 `impl Json` 在序列化中途触发定界延续挂起，遵循与 [display_frames.rs](../../engine/interpreter/src/display_frames.rs) 一致的状态所有权规则或拒绝跨活跃原生游标挂起。

#### C. Builtin ABI 分配
在 [engine/runtime/src/builtin.rs](../../engine/runtime/src/builtin.rs) 新增独立 ID 段（例如 `300..=329`）并递增 `BUILTIN_ABI_VERSION`：
- Tape 读取族：`JSON_TAPE_PARSE`、`JSON_TAPE_CLOSE`、`JSON_TAPE_NODE_INFO`、`JSON_TAPE_OBJ_GET`、`JSON_TAPE_OBJ_CHECK_UNKNOWN`、`JSON_TAPE_ARR_AT`、`JSON_TAPE_READ_INT`、`JSON_TAPE_READ_FLOAT`、`JSON_TAPE_READ_STR`、`JSON_TAPE_DYNAMIC_MATERIALIZE`。
- Writer 写入族：`JSON_WRITER_NEW`、`JSON_WRITER_CLOSE`、`JSON_WRITER_FINISH`、`JSON_WRITER_OBJ_BEGIN`、`JSON_WRITER_OBJ_KEY`、`JSON_WRITER_OBJ_END`、`JSON_WRITER_ARR_BEGIN`、`JSON_WRITER_ARR_END`、`JSON_WRITER_SCALAR`。

### 1.3 `derive Json` 编译期降级规则

在 `engine/resolution/src/json_derivation.rs` 与 `engine/nir/src/json_derivation.rs` 实现，分两阶段执行以天然支持前向声明与递归数据结构：

1. **阶段一（签名预登记）**：
   - 扫描所有 `DeriveDef` 中包含 `Json` 的目标类型 `T`，预先登记内部组合游标函数 `__json_write_<T>(val: T, writer: i64) -> !JsonError Unit`、`__json_read_<T>(tape: i64, node: i64, path: String, allow_unknown: bool) -> !JsonError T`，以及公开包装方法 `to_json`、`to_json_pretty`、`from_json`。
2. **阶段二（前置条件核查）**：
   - 允许派生的目标类型：`struct`、`enum`、`newtype`、tuple `typealias`。
   - 递归核查所有组成字段/变体载荷类型必须具备**全局** `Json` 支持：
     - 内建支持：全部有符号/无符号固定宽整数（`i8..i128`、`u8..u128`、`isize`、`usize`）、`f32`、`f64`、`bool`、`char`（单字符 JSON 字符串，多字符解码报 `TypeMismatch`）、`String`、`Unit`（映射 `null`）。
     - 复合支持：`?U`（当 `U: Json`）、定长 Tuple `(U1, U2, ...)`（当各元素 `: Json`）、已实现/派生全局 `Json` 的用户类型。
     - **编译期拒绝**：`Any`（除非显式使用 `std.json.RawValue` / 动态 `Any` 包装器）、闭包函数、`Continuation`、`Future`、未带静态元素类型的裸 `List`/`Map`（动态容器走 `json.parse` / `json.stringify`，若后续引入静态泛型容器再按元素约束放开），以及仅在局部作用域 `extend` 的非全局实现。
3. **数据布局到 JSON 的规范映射（严格双射）**：
   - **`struct`**：编码为 JSON Object，键名即源码字段名，按声明顺序写出。解码时：
     - 存在且非 `null`：按字段类型递归调用 `__json_read_<FieldTy>`。
     - 字段缺失：若字段声明含默认值表达式，按声明顺序求默认值；否则若字段类型为 `?U`，置为 `null`；否则返回 `MissingField(path, field_name)`。
     - 字段值为 `null`：若字段类型为 `?U` 或 `Unit`，置为 `null`/`unit`；否则返回 `TypeMismatch`。
     - 当 `allow_unknown == false` 时，调用 `JSON_TAPE_OBJ_CHECK_UNKNOWN` 校验是否存在未声明键，命中返回 `UnknownField`。
   - **`enum`**：
     - 无载荷变体 `E.A` $\leftrightarrow$ JSON 字符串 `"A"`。
     - 单载荷变体 `E.B(x)` $\leftrightarrow$ 单键 JSON Object `{"B": <x>}`。
     - 多载荷变体 `E.C(x, y)` $\leftrightarrow$ 单键 JSON Object 加定长数组 `{"C": [<x>, <y>]}`。
   - **`newtype`**：透明表示，`Newtype(inner)` 直接编解码为 `<inner>` 的 JSON 表示。
   - **Tuple `typealias`**：定长 JSON Array `[e0, e1, ...]`，数组长度不等于元组元数时返回 `TypeMismatch`。
   - **数值精度与范围**：整数按目标位宽（如 `u8` 要求 `0..=255`，`i64` 要求 `-2^63..=2^63-1`）精确校验，禁止将带小数部分的 `1.5` 静默截断为 `1`；`i128`/`u128` 超出 JSON IEEE-754 双精度安全整数范围时仍按精确十进制字面量解析与写出。
4. **NSBC 归档持久化**：
   - 生成的全部编解码函数作为普通受检字节码函数写入 `.nsbc`，并在 `type_pool` / `nsbc_io` 记录全局 `Json` 实现元数据，确保删除 `.ns` 源码后 `nessa run app.nsbc` 行为完全一致。

---

## 2. 通用安全 FFI 子系统与高性能 `regex` 库设计

### 2.1 运行时底层支撑：紧凑 `Bytes` 与受检资源表

为彻底消除 FFI 跨界拷贝开销并严守 GC 安全边界，先在引擎补齐两项**与正则无关的通用原语**：

1. **GC No-Scan 紧凑字节数组 `Bytes`（`engine/gc`、`engine/runtime`、`library/std/bytes.ns`）**：
   - 在对象头元数据中新增非指针叶对象角色（`ObjectRole::ByteBuffer`）：GC 标记与移动阶段仅处理该对象本身，**绝不扫描其内部字节 payload**。
   - 内存布局：`Header (16B) + len: usize + cap: usize + inline/LOS data`，支持超过 512 KiB 时走 Immix 大对象空间（LOS），解除现有 `List` 单缓冲 65535 元素上限与 8 倍内存膨胀。
   - 提供零拷贝区间切片操作与 `String` UTF-8 受检互转。
2. **类型化主机外部句柄表 `VmResourceTable`（扩展 [resources.rs](../../engine/interpreter/src/resources.rs)）**：
   - 统一管理 FFI 打开的动态库、编译好的外部自动机或其他 C/Rust 对象：`handle_id: i64 -> TypedResource { type_tag: u32, payload: Box<dyn Any>, destructor }`。
   - Nessa 层使用 `newtype` 包装 `i64` 句柄；底层每次访问核对 `type_tag` 与存活状态，伪造或已释放句柄返回可恢复错误而非 UB。

### 2.2 C FFI 与内嵌 WASM 双通道设计

补齐 `docs/ffi/` 的工程实现，新增 `engine/ffi` crate，支持两种零特权扩展载体：

1. **安全借用型 C ABI 通道（Pinned Read-Only Borrow Protocol）**：
   - **包清单声明**：仅允许在 `package.toml` 显式声明 `[ffi]` 绑定的受信任包加载原生符号，阻止普通脚本任意探测宿主地址空间。
   - **零拷贝参数传递**：在同步 C FFI 调用期间，当前 mutator 处于非安全点（No-Safepoint）临界区，Immix 收集器不会移动当前堆对象。FFI 编组器直接提取 Nessa `String` 或 `Bytes` 的 `(ptr: *const u8, len: usize)` 作为 `NessaSlice` 传入 C 函数，并提供预分配的可写 `Bytes` 缓冲区 `(mut_ptr: *mut u8, cap: usize)` 接收批量输出。
   - **安全铁律**：
     1. 被借用的只读指针生命周期严格限制在该次同步 C 函数返回前，禁止 C 侧缓存裸指针。
     2. C 函数执行期间禁止重入当前 `Vm` 触发堆分配或 GC。
     3. 若存在活跃原生 FFI 栈帧，任何跨越该帧边界的定界延续捕获（`EffectCallDyn` 命中外层 `catch`）必须立即拒绝并返回 `VmError::EffectAcrossNativeFrame`（对齐 [continuation-stack-abi.md](continuation-stack-abi.md) 约束）。
2. **NSBC 归档内嵌 WASM 通道（对齐 [wasm-in-archive.md](nessa-bytecode/wasm-in-archive.md)）**：
   - 在 `nsbc` / `nsbc_io` 落地 WASM Section 读写与校验，集成轻量 WASM 执行器（开启 `simd128` 特性），通过持久化的线性内存页缓冲与 `Bytes` 批量交换数据。当目标平台无本地预编译动态库时，包可直接分发内嵌 `.wasm` 的单文件 `.nsbc` 归档。

### 2.3 基于 FFI 的 `regex` 标准扩展包

在语言核心与编译器**零改动、零正则硬编码**的前提下，构建 `library/regex/` 包及配套原生 FFI 后端 `crates/nessa_regex_ffi`（基于 `regex-automata`，仅含 `dfa` + `nfa-pikevm` + `perf-literal` 核心模块）：

1. **算法与复杂度保证**：
   - 采用 **Literal Prefilter（`memchr` / Teddy SIMD 前缀过滤）+ Bounded Lazy DFA + PikeVM 捕获组回退**。
   - 严格保证任意模式与输入上的 **$O(N)$ 线性匹配时间复杂度**，彻底免疫灾难性回溯（ReDoS）；因此**禁止支持**破坏正则性的反向引用（`\1`）与任意环视断言，语法对齐 Rust `regex` / RE2（支持分组、交替 `|`、贪婪/非贪婪量词、`(?flags)`、命名捕获 `(?P<name>...)`、UTF-8 与 Unicode 类别）。
   - 编译期可配置 DFA 状态缓存字节上限与编译大小上限，超限返回 `RegexError.CompiledTooLarge`。

2. **跨界批量 Span Buffer ABI（零小对象分配）**：
   彻底规避“每匹配一次跨 FFI 构造字符串/列表”的性能陷阱，原生侧仅导出 5 个粗粒度 C ABI 函数：
   ```c
   // 编译正则，返回主机资源表句柄；失败将错误消息写入 err_buf
   int64_t nessa_regex_compile(const uint8_t* pat, size_t pat_len, uint32_t flags,
                               uint8_t* err_buf, size_t* err_len);
   void    nessa_regex_free(int64_t handle);
   // 快速布尔判定（纯 Lazy DFA，不跑 PikeVM 捕获槽，最快路径）
   int32_t nessa_regex_is_match(int64_t handle, const uint8_t* text, size_t text_len, size_t start);
   // 单次捕获：一次写回 [g0_start, g0_end, g1_start, g1_end, ...]（未命中组填 usize::MAX）
   int32_t nessa_regex_captures(int64_t handle, const uint8_t* text, size_t text_len, size_t start,
                                size_t* out_slots, size_t slots_len);
   // 批量全局查找：一次 FFI 调用最多填充 max_matches 对 [start, end]，返回实际写入对数（如一次 64 个）
   ssize_t nessa_regex_find_batch(int64_t handle, const uint8_t* text, size_t text_len, size_t start,
                                  size_t* out_pairs, size_t max_matches);
   ```
   - `find_all` 迭代器每次跨 FFI 预取 64 个匹配区间存入紧凑缓冲，后续 63 次迭代完全在 Nessa 内存内读取偏移；`Match.as_str()` 按需对原 `String` 切片；`replace_all` 在单次缓冲区内完成全部区间拼接。

3. **Nessa 侧公开 API（`library/regex/mod.ns`）**：
   - `pub enum RegexError { InvalidSyntax(message: String), CompiledTooLarge(limit: usize) }`
   - `Regex.compile(pattern: String, .case_insensitive: bool = false, .multi_line: bool = false, .dot_matches_new_line: bool = false) -> !RegexError Regex`
   - `re.is_match(text: String, .start: usize = 0) -> bool`
   - `re.find(text: String, .start: usize = 0) -> ?SpanMatch`
   - `re.captures(text: String, .start: usize = 0) -> ?Captures`（支持按索引 `caps.get(1)` 与按名字 `caps.name("year")`）
   - `re.find_all(text: String) -> MatchIter`（实现 `std.traits.Iterator`）
   - `re.split(text: String, .limit: usize = 0) -> List`
   - `re.replace(text: String, replacement: String) -> String` / `re.replace_all(text: String, replacement: String) -> String`（支持 `$0`、`$1`、`${name}` 展开与 `$$` 转义）

---

## 3. 分层热点 JIT 编译器设计（`engine/jit`）

### 3.1 设计目标与核心架构取舍

解释器在遇到密集循环、数值计算、哈希/编码或状态机热点时，取指解码、间接跳转分派与每次算术的 `TaggedValue` 重复拆装箱会造成 20～100 倍吞吐落差。同时，Nessa 具有两大强运行时约束：
1. **Immix 移动式 GC**：任何分配或 `Safepoint` 都可能移动堆对象并原地改写 `StackContext` 槽位。
2. **Multi-prompt Delimited Continuation**：随时可在 `EffectCallDyn` 分离 `StackContext` 链，并由 `Continuation.clone` 任意复制暂停栈段。

**核心架构决策：采用“外部化 `StackContext` 帧 ABI + 循环回边 OSR + 双层轻量后端”**。
- **不在宿主 C 栈上保存 Nessa 调用帧**：JIT 生成的机器码仅借用宿主物理寄存器作为当前 `StackContext.regs[0..32]` 与热 `local_slots` 的高速缓存；在所有潜在 GC 安全点、外部调用与效应指令前，将活跃虚拟寄存器写回 `StackContext` 并写入精确字节码 `pc`。
- **架构收益**：
  - **GC 零改造**：Immix 收集器无需解析原生机器栈或维护复杂 DWARF StackMap，照常扫描 `StackContext::visit_slots_mut`，JIT 在 helper 返回后从 `ctx` 重载已转发的新地址。
  - **Continuation 零改造**：遇到效应捕获或恢复指令时，JIT 直接将状态同步回 `StackContext` 并返回 `JitExit::InterpretAt(pc)`，退回解释器外层调度循环完成栈段拆接或克隆；后续恢复时可从任意 `pc` 继续解释或经 OSR 重新进入 JIT。
  - **无重型依赖**：新增独立 crate `engine/jit`，自研轻量双架构（`x86_64` SysV/Win64 与 `aarch64` AAPCS64）机器码发射器（仅数千行安全封装的缓冲汇编器 + POSIX `mmap`/`mprotect` W^X 管理），单函数编译耗时 $< 100\,\mu\text{s}$，零外部巨型编译器依赖。不支持的目标架构编译期自动回退为无操作 stub，解释器全功能可用。

### 3.2 原生调用约定与退出协议（Trampoline ABI）

```rust
#[repr(C)]
pub struct JitExit {
    pub kind: JitExitKind, // Return = 0, Call = 1, InterpretAt = 2, Trap = 3
    pub aux: u32,          // Call 时的指令 pc / InterpretAt 时的精确恢复 pc / Trap 错误码
    pub payload: u64,      // Return 时的 TaggedValue bits / Call 时的目标 FuncId
}

pub type JitEntryFn = unsafe extern "C" fn(
    vm: *mut VmState,
    roots: *mut VmRoots,
    ctx: *mut StackContext,
    entry_offset: usize,   // 0 表示函数首入口；非 0 表示 OSR 循环头原生偏移
) -> JitExit;
```

- **物理寄存器规划**（以 `x86_64` System V 与 `aarch64` 为例）：
  - 固定上下文指针寄存器：`CTX_REG`（保存 `*mut StackContext`，直接以固定偏移寻址 `regs[0..32]` 与 `local_slots_ptr`）、`VM_REG`（保存 `*mut VmState`）。
  - 热虚拟寄存器驻留池：划拨 4～6 个 callee-saved 通用寄存器映射当前基本块/循环最热的 Nessa 虚拟寄存器（如 `r0..r5`），划拨 4～6 个 SIMD/FP 寄存器（`xmm0..xmm5` / `v0..v5`）供 Tier-2 存放拆箱 `f64`。
- **退出路径语义**：
  1. `JitExitKind::Return`：直接把返回值交给调用方帧（若调用方也是同段 JIT 帧且命中直接调用快路径，可在允许深度预算内直接串联，否则返回解释器主循环弹帧）。
  2. `JitExitKind::InterpretAt`：遇到 `EffectCallDyn`、`PushHandlerClosure`、`PushCapturingHandler`、`ResetClosure`、`ResumeContinuation`、`ResumeContinuationOnce`、`AwaitFuture` 或去优化（Deopt Guard 失败）。此时 `ctx.pc = aux`，所有寄存器与槽位已是该指令执行前的精确状态，解释器当场执行该条指令，无任何状态丢失。
  3. `JitExitKind::Trap`：算术除零、越界、`TypeAssert` 失败、`MatchFail` 等，将精确故障 `pc` 写回 `ctx` 后交由解释器统一构造带源码行号的 `VmError`。

### 3.3 热点探测与循环回边 OSR（救急核心机制）

如果仅在函数调用入口计数，**只调用 1 次但内部执行 $10^8$ 次迭代的热循环**将永远卡在解释器中无法被优化。因此必须同时实施入口计数与**循环回边栈上替换（On-Stack Replacement, OSR）**：

1. **热度计数器与状态机**：
   每个 `FunctionCode` 附带 `JitCell`：
   - 状态：`Cold` $\rightarrow$ `CompiledTier1` $\rightarrow$ `CompiledTier2`（若包含异常超限结构或累计 Deopt 超过 8 次，则标记为 `Disabled` 永久走解释器，防止编译抖动）。
   - 计分权重：每次函数调用 `hotness += 16`；每次解释器执行偏移量 $\le 0$ 的后向跳转（`Jmp`、`JmpIf`、`JmpIfNot`、`JmpIfNull`、`JmpIfNotNull`、`JmpFar`）时 `hotness += 1`。
2. **触发阈值**：
   - **Tier-1 触发阈值**：`hotness >= 4096`（即 256 次函数调用，或单次调用内 4096 次循环迭代，解释器耗时尚不足 0.5 ms 时即触发）。
   - **Tier-2 触发阈值**：Tier-1 代码在循环回边与入口继续轻量递减采样计数器，累计达到 `65,536` 时升级编译 Tier-2。
3. **OSR 切入流程**：
   - 编译器在扫描 NSBC 字节码时识别所有后向跳转的目标 `target_pc`（自然循环头），在生成的机器码中为每个 `target_pc` 建立一段 **OSR Prologue**：从 `ctx.regs` / `ctx.local_slots` 加载该循环头活跃的虚拟寄存器到物理寄存器（Tier-2 同时执行类型守卫与拆箱），然后无条件跳入原生循环体 `pc_to_native[target_pc]`。
   - 当解释器在后向跳转处触发 `hotness >= 4096` 时，立即编译该函数，查表拿到 `target_pc` 对应的 `osr_native_offset`，**在当次迭代原地调用 `jit_entry(vm, roots, ctx, osr_native_offset)`**，热循环瞬间切入原生机器码运行！

### 3.4 两级编译优化管线

#### Tier-1：单遍模板与内联快路径 JIT（Baseline Fast JIT）
面向全指令集覆盖（特殊效应指令自动发射 `InterpretAt` 出口），编译时间控制在微秒级：
- **消除分派开销**：NSBC 跳转指令直接编译为原生相对分支 `jmp` / `je` / `jne`（双遍回填分支偏移）。
- **TaggedValue 算术与比较内联快路径**：
  - `Add` / `Sub` / `Mul` / `Bit*` / `Shl` / `Shr` / `Cmp*`：内联 3～5 条机器指令检查两个寄存器是否均为 immediate SmallInt（57-bit 有符号整数 tag）；命中时直接在物理寄存器完成原生整数运算与溢出标志位检查（`jo slow_path`）；未命中（`f64`、堆上 128-bit 大整数或动态错误类型）跳入紧凑的冷路径块调用 `jit_rt_arith_slow` helper。
- **单态内联缓存（Monomorphic Inline Cache, IC）**：
  - 每个 `LoadField` / `StoreField` / `EnumIs` / `EnumField` / `CallMethod` 站点在元数据区拥有独立 IC 槽（记录缓存的 `TypeIndex` 与已验证偏移/目标 `FuncId`）。
  - 命中 `obj.header.type_index == cached_type` 时，无需查 `TypePool`，一条变址内存加载指令直接读写 `*(obj_ptr + 16 + field_idx * 8)`。
- **精确 Safepoint 轮询**：
  - 在所有后向跳转回边内联递减 `vm.slice_safepoints` 并检查 `heap.safepoint_requested` 原子标志；仅当标志置位（慢路径）时刷回全部缓存寄存器并调用 `jit_rt_safepoint_poll`（支持时间片抢占调度与并发/同步 Immix GC 停顿）。

#### Tier-2：基于静态类型与反馈的循环特化 JIT（Loop-Specializing JIT）
对持续高温的函数，结合 `TypePool` 静态类型信息、局部变量活跃区间分析（Liveness，支持非 SSA 重复赋值的不动点 `live_in`/`live_out`）与 Tier-1 IC 收集的具体类型反馈：
- **循环寄存器拆箱（Loop Register Unboxing）**：
  - 识别循环体内不变类型的数值寄存器与局部 slot（`i64` / `f64` / `bool`）。
  - 在循环 Preheader（及 OSR Prologue）一次性执行 Tag 守卫（Guard）并拆箱到物理通用寄存器或浮点寄存器；循环体内使用纯原生 `add`/`imul`/`addsd`/`mulsd`/`ucomisd` 指令，**零 Tag 移位、零堆分配**（`f64` 循环彻底消除每次迭代分配 boxed float 堆对象的开销！）。
  - **Box-on-Exit（延迟装箱）**：仅在循环正常出口、Deopt 侧出口（如算术溢出到 128-bit）或触发 GC Safepoint 慢路径时，才将拆箱值重新编码回 `TaggedValue` 写入 `StackContext`。
- **冗余类型检查消除与不变式外提（LICM）**：
  - 同一基本块/支配路径上对未重定义寄存器的重复 `TypeAssert`、`TypeCheck` 与对象 `TypeIndex` 守卫合并为单次检查。
  - 循环内对同一 `Struct` 实例的重复头类型检查外提到循环头之前（若循环内无未知函数调用；若有调用，因 `Struct` 的 `TypeIndex` 在对象生命周期内不可变，只需在 Safepoint 后重载 `obj_ptr` 地址，无需重验 `TypeIndex`）。

### 3.5 内存保护、代码缓存预算与可观测性

- **严格 W^X（Write XOR Execute）**：
  - 代码页分配时仅开启 `PROT_READ | PROT_WRITE`；指令发射与重定位完成后调用 `mprotect(..., PROT_READ | PROT_EXEC)` 并在 `aarch64` 上执行指令缓存刷新（`__clear_cache` / `ic ivau` + `dsb ish` + `isb`）。绝不允许任何内存页同时具备可写与可执行权限。
- **代码缓存上限**：
  - 每个 `Vm` 默认上限 `16 MiB`（可通过 `VmConfig` 调整）。达到容量上限时停止新增编译并保持已有热点代码运行，未编译函数平滑留在解释器，禁止 OOM 或 panic。
- **CLI 控制与运行时内省**：
  - `nessa run` 支持 `--jit=auto|off|tier1|tier2`（默认 `auto`，环境变量 `NESSA_JIT` 可覆盖，便于 CI 进行解释器 vs JIT 差分等价验证）。
  - [library/std/runtime.ns](../../library/std/runtime.ns) 新增只读诊断查询：`jit_enabled() -> bool`、`jit_compiled_functions() -> usize`、`jit_code_bytes() -> usize`、`jit_deopt_count() -> usize`。

---

## 4. 模块边界、并行拆解与文件所有权

下游实现代理可使用 `graph-engineering-workflow` 按以下 4 条正交工作流并行推进。先合并共享底层约定（`builtin.rs` ID 段预留与 `resources.rs` 扩展点），各工作流严守各自文件边界，避免并发写冲突：

| 工作流 | 职责范围 | 独占/主要修改路径 | 依赖关系 |
| --- | --- | --- | --- |
| **WS-1: 内嵌 JSON 与 `derive Json`** | `JsonTape`/`JsonWriter` 原生内核、`Json` well-known trait、`resolution` + `nir` 组合式派生降级、`std.json` 升级与 `.nsbc` 归档往返 | `engine/type_pool/src/lib.rs`、`engine/resolution/src/json_derivation.rs`、`engine/nir/src/json_derivation.rs`、`engine/interpreter/src/json_kernel.rs`、`library/std/json.ns` | 独立启动 |
| **WS-2: `Bytes` 与通用安全 FFI** | GC No-Scan `ByteBuffer`、`VmResourceTable`、Pinned 只读切片 C FFI 通道、跨原生帧 Continuation 拦截、WASM Section 对接 | `engine/gc/`、`engine/ffi/`（新增）、`engine/interpreter/src/resources.rs`、`library/std/bytes.ns`、`docs/ffi/` | 独立启动 |
| **WS-3: FFI `regex` 扩展包** | `crates/nessa_regex_ffi`（`regex-automata` 封装与批量 Span Buffer C ABI）、`library/regex/` Nessa 包装、迭代器与替换实现 | `crates/nessa_regex_ffi/`（或 `engine/regex_ffi/`）、`library/regex/`、`engine/test/tests/regex_tests.rs` | 依赖 WS-2 的 FFI 借用与句柄接口就绪（可先并行编写原生自动机内核与测试用例） |
| **WS-4: 分层热点 JIT (`engine/jit`)** | `x86_64`/`aarch64` W^X 发射器、Trampoline ABI、循环回边热度计数与 OSR、Tier-1 内联快路径 + IC、Tier-2 循环拆箱、CLI `--jit` 开关 | `engine/jit/`（新增）、`engine/interpreter/src/jit_bridge.rs`、`engine/nessa/src/main.rs`、`library/std/runtime.ns`、`docs/TODO-jit/intro.md` | 独立启动 |

---

## 5. 验证与验收标准

所有交付必须通过 `bash scripts/cargo.sh` 完成验证，不得以占位实现、关闭 lint 或仅检查“不崩溃”宣称完成：

1. **JSON 与 `derive Json` 验收**：
   - **正例**：覆盖 `struct`（含默认值字段、`?T` 缺失与显式 `null`）、`enum`（无载荷、单载荷、多载荷）、`newtype`、Tuple `typealias`、嵌套自递归树结构、全部整数位宽边界（含 `i64::MIN`、`u64::MAX`、`i128`）、UTF-16 代理对与美化缩进往返等价。
   - **拒绝与错误案例**：编译期明确诊断含 `Any`/闭包/`Continuation`/局部 `extend` 字段的非法 `derive Json`；运行期精确断言 `MissingField`、`UnknownField`（当 `allow_unknown_fields = false`）、`TypeMismatch`、`IntegerOutOfRange`（如 `256` 解码为 `u8` 或 `1.5` 解码为 `i64`）、128 层 `DepthLimitExceeded`、对象引用环 `CycleDetected` 与非有限浮点数拒绝。
   - **归档与 GC 回归**：编译为 `.nsbc` 后删除原 `.ns` 源码独立执行通过；在 32 MiB 小堆与强制 Immix GC 压力下大 JSON（数万节点）编解码无悬垂引用、无句柄泄漏。

2. **FFI 与 `regex` 验收**：
   - **功能与语义**：覆盖分组、交替 `|`、贪婪与非贪婪量词 `+?`/`*?`、命名捕获 `(?P<name>...)`、多行/大小写不敏感标志、多字节 UTF-8 字符边界、`find_all` 跨批次（`> 64` 次匹配）迭代、`split`、`replace_all`（含 `$1`/`${name}`/`$$`）。
   - **抗 ReDoS 线性时间验证**：对经典病态回溯输入（如模式 `(a+)+b` 匹配长度 $50{,}000$ 的 `"a...a"`）断言在毫秒级返回 `false`，无指数级卡死。
   - **FFI 安全边界**：验证伪造/已释放句柄优雅报错；验证在 FFI 回调或原生帧存活期尝试跨帧捕获 Continuation 被明确拦截；验证多次创建并丢弃 `Regex` 实例在 GC 后主机内存正常回收。

3. **分层 JIT 验收**：
   - **差分等价测试（Differential Testing）**：将现有 `nessa-test` 集成测试集（算术、大整数溢出提升、结构体/枚举/Trait 分派、受检错误传播、协程、多提示符定界延续多次 `clone` 与恢复）分别在 `--jit=off`、`--jit=tier1`、`--jit=tier2` 下运行，输出与退出状态必须逐字节一致。
   - **OSR 救急专项测试**：构造单次 `main` 调用内包含 $5 \times 10^7$ 次迭代的纯数值计算、矩阵乘法、FNV/CRC 字节循环与对象字段累加循环（函数仅进入 1 次），断言循环中途成功触发 OSR（`runtime.jit_compiled_functions() >= 1`），且相比 `--jit=off` 取得显著实测加速并记录基准数据。
   - **GC 移动与去优化压力测试**：在 JIT 热循环内混合触发小对象分配、Immix 强制移动收集（核验循环中持有的对象引用在 GC 后仍读到正确字段值）、第 $N$ 次迭代突发 SmallInt 溢出至 128-bit 大整数触发 Deopt 回退解释器、以及循环中途触发 `EffectCallDyn` 捕获 Continuation 并多次恢复，确保零内存损坏、零寄存器错位。

4. **全仓交付门禁**：
   ```sh
   bash scripts/cargo.sh fmt --all -- --check
   bash scripts/cargo.sh check --workspace --all-targets
   bash scripts/cargo.sh test --workspace
   bash scripts/cargo.sh clippy --workspace --all-targets -- -D warnings
   git diff --check
   ```
