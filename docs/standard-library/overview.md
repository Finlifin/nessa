# Nessa 标准库总览 (`std`)

Nessa 标准库位于 `library/std/`，在编译时作为独立受信任包 `std` 加载，完整支持源码运行（`nessa run <file.ns>`）与脱离源码的独立字节码归档运行（`nessa build` -> `nessa run <file.nsbc>`）。

标准库围绕 Nessa 的五项核心语言优势设计，摒弃早期照搬 Lua 5.4 的无类型表/字符串约定 API：

1. **丰富的参数形态**：命名可选参数（`.radix: i64 = 10`、`.mode: OpenMode = .read`）、变参展开（`...args: List`）、双变参块应用 DSL（`json.object { ... }`、`Command.spec { ... }`、`ffi.load("...") { ... }`）与后置 `do |x|` 块。
2. **一等类型值驱动的静态精化**：`"42".parse(i64)!` / `conv.parse(u16, "0xff", radix = 16)!`、`json.decode(Config, text)!` 与 `ffi.load("m") { hypot: fn(f64, f64) -> f64 }!` 直接以一等 `Type` 作为参数，并在编译期完成返回类型与受检错误集精化。
3. **高效的 `?` / `!` / `#` 传播**：可选查询使用 `?T`，可恢复失败使用名义错误枚举与受检错误集 `!E T`，高阶回调（`List.map`、`Iterator.filter`、`File.with_open`、`Mutex.update`、`task.par_map` 等）自动传播内联闭包的 `!Error` 与 `#Effect`。
4. **同步 `effect` 与异步 `async effect` 双代数效应**：`std.stream` 基于同步 `effect emit` 提供可组合生成器流并可零摩擦桥接为惰性 `Iterator`；`std.io`、`std.time`、`std.rand` 提供可拦截替换的控制台/时钟/随机源能力效应；`std.task` 基于结构化 `async effect` 提供并发映射与竞速组合子。
5. **名义类型安全**：以 `Path`、`OpenMode`、`FileKind`、`Weekday`、`Command`、`CoroutineStep`、`Ptr`、`CStr`、`CallConv` 等名义类型取代裸字符串标志与无约束整数句柄。

从旧版 Lua 风格 API 升级的完整对照表与示例参见 [标准库迁移指南](../stdlib-migration.md)。

## 模块列表与职责

| 模块 | 预导入 (`prelude`) | 主要类型与功能 |
| --- | --- | --- |
| `std.builtin` | 是 | 基础内建类型与契约（`String`、`Bytes`、`List`、`Map`、`Continuation`、`Future`、`IterationStep`、`Json` 等） |
| `std.ordering` | 是 | `Ordering`（`less` / `equal` / `greater`）全序/偏序比较枚举 |
| `std.traits` | 是 | 核心标量与内建类型的 `Eq`、`PartialEq`、`Ord`、`PartialOrd`、`Display`、`Hash`、`Json` 实现 |
| `std.conv` | `ParseError`, `Utf8Error`, `type_of`, `to_i64`, `to_f64`, `to_string` | 一等类型驱动解析 `conv.parse(T, text, .radix = 10)` / `conv.try_parse(T, text, .radix = 10)` 与受检错误 `ParseError`、`Utf8Error` |
| [`std.iter`](iter.md) | 通过 `Iterator` / `IntoIterator` 自动特化 | 统一惰性迭代器体系：为所有实现 `Iterator` / `IntoIterator` 的类型按需特化 35 个适配器与消费器（`map`、`filter`、`filter_map`、`flat_map`、`flatten`、`take`、`skip`、`enumerate`、`zip`、`chain`、`chunks`、`peekable`、`fold`、`collect`、`to_map`、`to_set`、`join` 等），以及 `iter.{empty, once, repeat, from_fn, successors, unfold}` |
| [`std.stream`](iter.md#stdstream--基于同步代数效应的生成器流) | 否 | 基于同步代数效应 `effect emit(value: Any) -> Unit` 的推式流：`emit`、`from_iter`、`collect`、`for_each`、`fold`、`take`、`map`、`filter` 与推转拉桥接器 `to_iter` |
| `std.scalar` | 是 | 全部整数、浮点、`bool`、`char` 的方法与进制格式化（`to_str_radix`、`checked_*`、`wrapping_*`、`saturating_*`） |
| `std.range` | 是 | `Range`（`a..b`）、`RangeInclusive`（`a..=b`）、`RangeFrom`（`a..`）及反向迭代 `RevRangeIterator`（`(0..10).rev()`） |
| `std.fmt` | `format`, `printf`, `eprintf` | 类型安全格式化：`sprintf` / `try_sprintf` / `format` / `printf` / `eprintf` 与 `FormatError` |
| `std.pattern` | 否 | 轻量字符类模式匹配引擎：`PatternMatch`、`PatternCaptures`、`PatternMatchIter`、`find`、`find_iter`、`match_once`、`captures`、`replace`、`replace_with`、`replace_map`、`escape` |
| `std.string` | 是 | 0-based UTF-8 `String` 方法、一等类型解析 `s.parse(T)` / `s.try_parse(T)`、`chars()` / `char_indices()` / `bytes()` / `lines()` 及零分配惰性迭代器 `chars_iter()` / `char_indices_iter()` / `bytes_iter()` / `lines_iter()` |
| `std.bytes` | `Bytes` | 零拷贝切片与写时复制 `Bytes`、惰性 `BytesIter`（`b.iter()`）、`validate_utf8() -> !Utf8Error usize`、`to_string_utf8() -> !Utf8Error String`、二进制 `pack` / `unpack`（`!PackError`）以及直接操作 `Bytes` 的 `to_hex` / `from_hex` / `to_base64` / `from_base64`（`!DecodeError`） |
| `std.math` | 是 | 数学常数（`PI`、`TAU`、`E`、`INFINITY`、`NEG_INFINITY`、`NAN`）、初等函数、`approx_eq`、`lerp`、`split_fract`、`frexp`、`ldexp` |
| `std.rand` | 否 | `Rng` 实例与 VM 默认 `Xoshiro256**` 随机源：`int_range`、`next_u64`、`float`、`bool`、`random_bytes`（及 `Rng.bytes`）、`choose`、`shuffle`，以及可拦截随机能力效应 `effect random_u64`（`with_rng` / `with_seeded_rng`） |
| `std.path` | `Path` | 名义路径类型 `Path`（支持 `p1 / "sub"` 或 `p.join(...)`、`normalize`、`parent`、`file_name`、`stem`、`extension`、`with_extension`、`with_file_name`、`components`）及跨平台路径函数 |
| `std.io` | `print`, `println`, `eprint`, `eprintln`, `IoError`, `File`, `OpenMode` | 名义打开模式 `OpenMode`（`read` / `write` / `append` / `read_write` / `create_new`）、`SeekFrom`（`start` / `current` / `end`）、`File.with_open(path, .mode = .read) do |f| { ... }`、惰性行迭代 `File.lines()`，以及控制台能力效应 `console_print` / `console_println` / `console_read_line`（`with_stdio`、`capture_output`） |
| `std.fs` | 否 | 非阻塞文件系统操作：名义 `FileKind`（`file` / `directory` / `symlink` / `other`）、`FileMetadata`、`DirEntry`、惰性目录迭代 `iter_dir(path)`、`read_to_string`、`read_bytes`、`read_raw_bytes`、`write_string`、`write_bytes`、`write_raw_bytes`、`append_string`、`create_dir(.parents = false)`、`remove_file`、`remove_dir(.recursive = false)`、`remove`、`rename`、`copy`、`canonicalize`、`temp_file_path` |
| `std.env` | 否 | 命令行参数 `args()`、环境变量 `get` / `get_or` / `vars`、工作目录 `current_dir() -> !IoError Path` / `set_current_dir`、宿主平台 `os()` / `arch()` |
| `std.process` | `exit` | 进程控制与名义 `Command` 构建器：`Command.new(prog).arg(...).cwd(...).stdin(...).run()!`、双变参声明式 `Command.spec { "git", "status", cwd: repo }!`、`run`、`shell`、`pid` |
| `std.time` | 否 | `Duration`、`Instant`、`Weekday`（`sunday`..`saturday`）、默认午夜字段的 `DateTime`（`from_unix(secs, .utc = true)` / `to_unix(.utc = true)` / `weekday()` / `format(fmt, .utc = true)`），以及可模拟时钟能力效应 `clock_now_ms` / `clock_sleep`（`with_system_clock`、`with_mock_clock`） |
| `std.collections` | `List`, `Map`, `HashMap`, `Set`, `Deque` | 动态数组 `List`、字符串键字典 `Map`、通用键值 `HashMap`、集合 `Set`、双端队列 `Deque`，全部原生实现 `IntoIterator` 并支持静态同构元素类型推导与 `!Error` / `#Effect` 回调传播 |
| `std.coroutine` | 否 | 基于多提示符定界延续的非对称有状态协程：`Coroutine`、`CoroutineStatus`（`suspended` / `running` / `dead`）、名义单步结果 `CoroutineStep`（`yielded(value)` / `finished(value)`）、`CoroutineError` 与 `GeneratorIter` |
| `std.runtime` | `assert`, `assert_eq`, `panic`, `error_message` | 运行时断言、错误格式化、GC 控制（`collect_garbage`、`gc_collections`）、调用栈 `backtrace` 与 JIT 遥测（`jit_enabled`、`jit_compiled_functions`、`jit_code_bytes`、`jit_deopt_count`） |
| `std.json` | `Json`, `JsonError` | RFC 8259 JSON 编解码：一等类型解码 `json.decode(MyStruct, input, .allow_unknown_fields = true)!`、类型化 `json.encode(value, .indent = 0)`、双变参构造器 `json.object { ... }` / `json.array(...)`、动态 `parse` / `stringify` 与 `derive Json for T` |
| [`std.ffi`](ffi.md) | 否 | 受检 C-ABI 外部函数接口：声明式块加载器 `ffi.load("lib") { ... }!` / `ffi.must_load("lib") { ... }`、`ffi.func(sig, .callconv = .c)`、`Library`（含 `bind` 与 GC 弱引用终结器 `attach_finalizer`）、名义指针/切片标记 `Ptr` / `CStr` / `PtrRo` / `PtrMut` / `SliceRo` / `BufMut` 与 `FfiError` |
| [`std.task`](task.md) | 否 | 结构化并发与超时组合子：`TimeoutError`、`Either`（`left` / `right`）、`await_all`、`await_any`、`await_first`、`with_timeout`、结构化并行 `par_map` / `par_for_each` / `race` |
| [`std.sync`](sync.md) | 否 | 任务级非阻塞同步原语：`Channel`（`!ChannelError`）、异常/取消安全 `Mutex` / `RwLock` / `Semaphore` / `Once` / `Lazy`、受检破损栅栏 `Barrier.wait() -> !SyncError bool` 及无锁 `AtomicInt` / `AtomicBool` |
| [`std.net`](net.md) | 否 | 基于 Reactor 的非阻塞网络库：`SocketAddr`、`TcpListener`、`TcpStream`、`UdpDatagram`、`UdpSocket` 与异步 DNS `resolve_host` |
| `std.wasm` | 否 | 内嵌 WebAssembly 模块桥接工具 |

此外，捆绑扩展包 `library/regex/`（包名 `nessa/regex`，`use regex.*`）已全面构建于新 [`std.ffi`](ffi.md) 与 [`std.iter`](iter.md) 之上，提供线性时间 $O(N)$ 正则匹配及惰性迭代器 `MatchIter`（`find_all`）、`ByteMatchIter`（`find_all_bytes`）与 `SplitIter`（`split_iter`）。

## 设计约束

1. **受检错误优先**：所有可恢复 I/O、解析、编解码、通道、栅栏、超时、格式、模式、协程与 FFI 失败均返回名义受检错误集（`!IoError`、`!ParseError`、`!Utf8Error`、`!DecodeError`、`!PackError`、`!ChannelError`、`!SyncError`、`!TimeoutError`、`!FormatError`、`!PatternError`、`!CoroutineError`、`!JsonError`、`!FfiError`），严禁返回静默伪默认值。
2. **非阻塞任务挂起与取消安全**：`std.time`、`std.fs`、`std.process`、`std.task`、`std.sync` 与 `std.net` 中所有等待操作仅挂起当前 `Task`，绝不阻塞底层 OS Worker 线程，并在结构化取消时按逆序确定性执行 `defer` 清理。
3. **预导入零全局初始化开销**：`std.prelude` 导出的符号不包含非内建模块级副作用初始化，确保轻量脚本零启动负担。
4. **Nessa 命名规范一致性**：所有公共与私有类型名、Error `enum` 变体使用 `PascalCase`；普通值构造 `enum` 变体（如 `OpenMode.read`、`FileKind.file`、`Weekday.monday`、`CallConv.c`、`CoroutineStep.yielded`）、代数效应名、函数名、字段名与模块名一律使用 `snake_case`。
