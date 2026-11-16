# Nessa 标准库总览 (`std`)

Nessa 标准库位于 `library/std/`，在编译时作为独立受信任包 `std` 加载，完整支持源码运行（`nessa run <file.ns>`）与脱离源码的独立字节码归档运行（`nessa build` -> `nessa run <file.nsbc>`）。标准库覆盖 Lua 5.4 全部标准库能力（`base`、`coroutine`、`package`、`string`、`utf8`、`table`、`math`、`io`、`os`、`debug`），并提供符合 Nessa 代数效应、错误集与类型系统习惯的扩展模块。

## 模块列表与职责

| 模块 | 预导入 (`prelude`) | 主要类型与功能（对应 Lua 5.4 子库） |
| --- | --- | --- |
| `std.builtin` | 是 | 基础内建类型与契约（`String`、`Bytes`、`List`、`Map`、`Continuation`、`Future`、`Json` 等）及 `type_of` |
| `std.ordering` | 是 | `Ordering`（`less` / `equal` / `greater`）排序结果枚举 |
| `std.traits` | 是 | 核心 trait：`Eq`、`PartialEq`、`Ord`、`PartialOrd`、`Display`、`Hash`、`Json`、`Iterator`、`IntoIterator` |
| `std.scalar` | 是 | 全部整数、浮点、`bool`、`char` 的方法与 `Hash` / `Ord` / `Display` 实现；进制解析 `i64_from_str_radix` / `String.parse_int_radix` / `tonumber`（`tonumber(s, base)`） |
| `std.range` | 否（语法直连） | `..`、`..=` 半开/闭区间与 `Iterator` 实现 |
| `std.fmt` | `format`, `printf`, `eprintf` | `sprintf` 风格格式化（`%d/%i/%u/%o/%x/%X/%b/%f/%F/%e/%E/%g/%G/%a/%A/%c/%s/%q/%p/%%`，对应 `string.format`） |
| `std.pattern` | 否 | 完整 Lua 模式匹配引擎：`find`、`match_once`、`captures`、`gmatch`、`gsub`、`gsub_with`、`gsub_map`、`escape`、`UTF8_CHARPATTERN`（`%a/%c/%d/%g/%l/%p/%s/%u/%w/%x/%z`、`%BHH`、`%bxy`、`%f[set]`、捕获引用） |
| `std.string` | 是 | `String` 方法与 UTF-8 工具：`sub`（支持 1-based 正负索引）、`byte_slice`、`from_chars`、`from_codes` / `from_char_codes`、`utf8_codes` / `codepoints`、`utf8_len` / `utf8_len_bytes`（严格校验 RFC 3629 超长编码与代理项并返回错误字节偏移）、`utf8_offset`、`utf8_codepoint`、`utf8_charpattern`（完整覆盖 `string.*` 与 `utf8.*`） |
| `std.bytes` | `Bytes`（由 `std.builtin` 导出） | 紧凑 GC No-Scan 字节缓冲 `Bytes`（`O(1)` 零拷贝 `slice`、写时复制 `push`/`set`/`extend`、`concat`、`to_utf8_string`）及二进制打包/解包与编码：`pack`、`pack_size`、`unpack`（`<`/`>`/`=`/`!n`、`b/B/h/H/l/L/j/J/T/i[n]/I[n]/f/d/n/c[n]/z/s[n]/x/Xop`，对应 `string.pack*`）、Hex 与 Base64 编解码 |
| `std.math` | 是 | 三角/双曲/指数/对数、常数、`fmod`、`modf`、`frexp`、`ldexp`、`ult`、`to_integer`、`math_type`（完整覆盖 `math.*`） |
| `std.rand` | 否 | VM 级确定性/熵源 `Xoshiro256**` 伪随机数生成器：`random`、`random_int`、`random_up_to`、`seed`、`shuffle`、`choice`（对应 `math.random` / `math.randomseed`） |
| `std.io` | 是 | 标准流输出输入（`print`、`println`、`eprint`、`eprintln`、`tonumber`、`warn`、`flush_stdout`、`flush_stderr`）与 `File` 流句柄（`open` / `open_file`、`temp`、`read_to_string`、`read_line`、`read_chars`、`read_bytes`、`read_number`、`lines`、`write`、`write_bytes`、`flush`、`seek`、`close`，完整覆盖 `io.*`） |
| `std.fs` | 否 | 非阻塞文件系统操作（接入 `libuv` 异步文件系统请求，仅挂起当前 `Task` 而不阻塞计算 Worker）：`read_to_string`、`read_bytes`、`write_string`、`write_bytes`、`append_string`、`exists`、`is_file`、`is_dir`、`metadata`、`create_dir`、`create_dir_all`、`remove`、`rename`、`copy`、`read_dir`、`canonicalize`、`temp_file_path`（覆盖 `os.remove` / `os.rename` / `os.tmpname` 及扩展） |
| `std.path` | 否 | 跨平台路径解析与拼接：`join`、`join_all`、`normalize`、`components`、`parent`、`file_name`、`file_stem`、`extension`、`with_extension`、`is_absolute` |
| `std.env` | 否 | 命令行参数 `args`、环境变量 `get`/`get_or`/`vars`、工作目录 `current_dir`/`set_current_dir`、宿主平台 `os`/`arch`（覆盖 `os.getenv` 与 CLI `arg`） |
| `std.time` | 否 | Unix 时间戳 `unix_secs`/`unix_millis`、单调时钟 `monotonic_nanos` / `Instant`、`Duration`、进程 CPU 时间 `cpu_secs`、基于 `libuv` 定时器的非阻塞 `sleep_ms` / `sleep`、日历分解 `DateTime`（`from_unix_utc`/`from_unix_local`/`to_unix_utc`/`to_unix_local`，承担 Lua `*t`/`!*t` 日历表角色）、`difftime`、`format_date`（`strftime` 格式化，完整覆盖 `os.time` / `os.date` / `os.clock` / `os.difftime`） |
| `std.process` | 是 | `exit`、`panic`、`assert`、`assert_eq`、基于 `libuv` `uv_process_t` + `uv_pipe_t` 的非阻塞子进程执行 `exec` / `shell`（返回 `CommandOutput`，含 `status` / `success()`，任务取消时自动 `SIGKILL` 并回收子进程与管道）与进程号 `pid`（覆盖 `os.execute` / `os.exit` / `assert` / `error`） |
| `std.collections` | `HashMap`, `Set`, `Deque`（`List`/`Map` 由 `std.builtin` 导出） | 多线程安全动态数组 `List`（含原地 `sort`/`sort_by`、`insert`/`remove`/`copy_to`/`join_range`/`pack`、支持静态元素精化的 `await_all`/`await_any`，配合语言层 `...list` 展开覆盖 `table.*`）、字符串键字典 `Map`、基于 `Hash + Eq` 的通用 `HashMap` / `Set` 及双端队列 `Deque` |
| `std.coroutine` | 否 | 基于多提示符定界延续的非对称有状态协程与生成器：`Coroutine`（`create`、`try_resume`/`resume`、`step`、`yield_val`、`wrap`、`status`、`running`、`is_yieldable`、`close`）与 `Generator`（单任务局部栈段切换，完整覆盖 `coroutine.*`） |
| `std.runtime` | 否 | 运行时内省、垃圾回收与 JIT 遥测：`VERSION`、`collect_garbage`、`gc_collections`、`backtrace`、`jit_enabled`、`jit_compiled_fns`、`jit_code_bytes`、`jit_deopt_count`（覆盖 `_VERSION`、`collectgarbage` 与 `debug.traceback`） |
| `std.json` | `Json`, `JsonError` | 原生单趟扫描 RFC 8259 JSON 内核与类型化编解码：动态 `parse` / `stringify`、类型化 `encode` / `to_json_pretty` / `T.from_json`、支持 `derive Json for MyType`（`struct` 与单元/负载 `enum`，含独立 `.nsbc` 归档持久化、Unicode 代理对解码、128 层递归深度上限检查 `DepthLimitExceeded`、循环引用检测 `CircularReference` 与非有限浮点数拒绝） |
| [`std.task`](task.md) | 否 | 异步效应与结构化并发组合子：`TimeoutError`、`Either`（`Left` / `Right`）、`yield_now`、`sleep_ms`、`sleep`、`current_id`、`tree_snapshot`、`is_ready`、支持编译期静态类型精化的 `await_all` / `await_any` / `await_first` / `with_timeout` |
| [`std.sync`](sync.md) | 否 | 任务级非阻塞同步与通信原语：`ChannelError`、有界/无缓冲 MPMC `Channel`、作用域安全 `Mutex` / `RwLock` / `Semaphore`、单次初始化 `Once` / `Lazy`、多任务栅栏 `Barrier` 及无锁原子 `AtomicInt` / `AtomicBool` |
| [`std.net`](net.md) | 否 | 基于 `libuv` I/O Reactor 的非阻塞网络库：`SocketAddr`、`TcpListener`、`TcpStream`、`UdpDatagram`、`UdpSocket` 与异步 DNS 解析 `resolve_host` |

此外，仓库随附基于通用受检 C FFI（`[ffi] allow = true`）实现的捆绑扩展包 `library/regex/`（包名 `nessa/regex`，支持 `use regex.*`），提供线性时间 $O(N)$ 正则匹配、捕获组、迭代搜索、分割与替换，无需向编译器或字节码引入正则专用语法与指令。

## 设计约束

1. **错误语义**：可恢复 I/O、网络、通道、超时、格式、模式、打包、协程与 JSON 错误均使用受检错误集（`!IoError`、`!ChannelError`、`!TimeoutError`、`!FormatError`、`!PatternError`、`!PackError`、`!CoroutineError`、`!JsonError`）返回，不返回静默默认值。
2. **非阻塞任务挂起**：`std.time`、`std.fs`、`std.process`、`std.task`、`std.sync` 与 `std.net` 中所有等待操作均只挂起当前 `Task`，绝不阻塞底层 OS Worker 线程，且在结构化取消时自动撤销挂起句柄并按逆序执行 `defer` 清理。
3. **预导入零全局初始化开销**：`std.prelude` 导出模块不包含非内建运行时模块级全局初始化，确保简单脚本不承担无关子系统开销。
4. **聚合类型显示边界**：`List` 与 `Map` 不实现全局 `Display` trait（避免掩盖异构动态容器的类型检查），显式文本化使用 `List.format_list()`、`fmt.format` 或 `json.stringify`。
