# Nessa 标准库重构迁移指南 (`std` & `regex`)

本文档面向所有使用旧版（Lua 5.4 映射风格）Nessa 标准库的代码库（含下游应用如 `lava`），提供完整的设计动机说明、逐模块 API 迁移对照表、改造前后代码对比与升级核查清单。

---

## 1. 重构背景与五大核心设计支柱

旧版 `std` 在早期孵化阶段直接照搬了大量 Lua 5.4 标准库接口（如 `tonumber`、1-based 正负切片 `String.sub`、`string.gsub`/`gmatch`、`math.huge`/`ult`/`math_type`、`os.difftime`、`io.open(path, "rb+")` 字符串模式、`utf8_len` 返回 `(?i64, ?i64)` 二元组、裸整数句柄 FFI 等），未能发挥 Nessa 语言本身的类型与控制流优势。

新版标准库围绕 Nessa 的五项核心语言特性彻底重塑：

1. **丰富的参数形态**：
   - 统一采用**命名可选参数**（`.radix: i64 = 10`、`.mode: OpenMode = .read`、`.recursive: bool = false`、`.indent: i64 = 0`、`.callconv: CallConv = .c`）消除 `foo` / `foo_with_*` / `foo_all` 冗余变体；
   - 利用**双变参块应用 DSL**（`(...items: List, ...props: Map)`）提供声明式 `ffi.load("lib") { ... }!`、`Command.spec { "git", "status", cwd: dir }!` 与 `json.object { host: "localhost", port: 8080 }`；
   - 资源管理与并发原语全面适配**后置 `do |x|` 块**（如 `File.with_open(path) do |f| { ... }`）。
2. **一等类型值（`Type`）驱动的编译期精化**：
   - 字符串解析：`"42".parse(i64)! -> !ParseError i64`、`"3.14".try_parse(f64) -> ?f64`、`conv.parse(u16, "ff", radix = 16)! -> !ParseError u16`；传入不支持的 `Type` 在编译期直接报错；
   - JSON 解码：`json.decode(ServerConfig, raw_text, allow_unknown_fields = false)! -> !JsonError ServerConfig`；
   - 受检 C FFI：`ffi.load("m") { hypot: fn(f64, f64) -> f64 }!` 直接以 Nessa 函数类型字面量表达 C 签名，编译期静态校验参数/返回类型合法性与调用点实参匹配。
3. **高效的 `?` / `!` / `#` 传播**：
   - 缺省查询统一返回 `?T`（配合 `?` 与 `let x? = ... else { ... }`）；
   - 可恢复失败统一返回名义受检错误集 `!E T`（`!IoError`、`!ParseError`、`!Utf8Error`、`!DecodeError`、`!SyncError`、`!FfiError` 等）；
   - 所有高阶组合子（`List.map`、`Iterator.filter`、`File.with_open`、`Mutex.update`、`RwLock.with_write`、`Once.call_once`、`task.par_map`、`stream.*` 等）自动传播内联闭包的 `!Error` 与 `#Effect`，遇错立即短路并在 `defer` 保护下安全释放资源。
4. **同步 `effect` 与异步 `async effect` 双代数效应**：
   - 新增 `std.stream`（`pub effect emit(value: Any) -> Unit`），支持推式流生成及通过一等定界延续转换为惰性拉式迭代器 `stream.to_iter`；
   - `std.io`、`std.time`、`std.rand` 提供可拦截的能力效应（`io.capture_output`、`time.with_mock_clock`、`rand.with_seeded_rng`），无需全局可变状态即可编写确定性单测；
   - `std.task` 新增结构化并发组合子 `task.par_map`、`task.par_for_each` 与 `task.race`。
5. **名义类型安全与严格命名规范**：
   - 引入 `Path`、`OpenMode`、`FileKind`、`Weekday`、`Command`、`CoroutineStep`、`Ptr`、`CStr`、`CallConv` 等名义类型；
   - 严格遵循 `docs/code-style.md`：类型名与 Error `enum` 变体使用 `PascalCase`，普通值构造 `enum` 变体（如 `OpenMode.read`、`SeekFrom.start`、`FileKind.file`、`Weekday.monday`、`CallConv.c`）与代数效应名一律使用 `snake_case`。

---

## 2. 预导入 (`std.prelude`) 变更

为避免污染全局命名空间，新版 `std.prelude` 仅导出高频核心类型与基础工具，不再整包通配符导出 `std.io.*` 与 `std.process.*`：

| 类别 | `std.prelude` 现默认导出 | 需显式 `use` 的符号 |
| --- | --- | --- |
| 基础与标量 | `std.builtin.*`, `std.ordering.*`, `std.traits.*`, `std.range.*`, `std.scalar.*`, `std.math.*`, `std.string.*` | — |
| 容器与路径 | `List`, `Map`, `HashMap`, `Set`, `Deque`, `Path` | — |
| I/O 与格式 | `print`, `println`, `eprint`, `eprintln`, `format`, `printf`, `eprintf`, `IoError`, `File`, `OpenMode` | `use std.io.{SeekFrom, flush_stdout, read_stdin_line, capture_output, ...}` |
| 转换与错误 | `ParseError`, `Utf8Error`, `JsonError`, `type_of`, `to_i64`, `to_f64`, `to_string` | `use std.conv` |
| 运行时与进程 | `assert`, `assert_eq`, `panic`, `error_message`, `exit` | `use std.process.{Command, run, shell, pid}` |

---

## 3. 逐模块 API 迁移对照表

### 3.1 类型转换与字符串解析 (`std.conv` & `std.string` & `std.scalar`)

| 旧版 API（已移除） | 新版惯用 API | 说明 |
| --- | --- | --- |
| `tonumber(val)` / `io.tonumber(val, base)` | `"42".try_parse(i64)` / `"3.14".try_parse(f64)` | 可选解析返回 `?T`；需受检错误时用 `"42".parse(i64)! -> !ParseError i64` |
| `scalar.tonumber(s, base)` | `conv.try_parse(i64, s, radix = base)` / `s.parse(i64, radix = base)!` | 支持全部整数宽度（`i8..i128`, `u8..u128`, `isize`, `usize`）、`f32`/`f64`、`bool`、`char` |
| `"ff".parse_int_radix(16)` | `"ff".try_parse(i64, radix = 16)` 或 `"ff".parse(i64, radix = 16)!` | 统一收敛至一等 `Type` 解析入口 |
| `"42".parse_i64()` / `"3.5".parse_f64()` | `"42".try_parse(i64)` / `"3.5".try_parse(f64)` | 编译期直接精化为 `?i64` / `?f64` |
| `io.type_of` / `io.to_string` | `type_of` / `to_string`（位于 `std.conv` 并由 `prelude` 导出） | 职责归入 `std.conv` |

### 3.2 字符串与 UTF-8 (`std.string`)

| 旧版 API（已移除） | 新版惯用 API | 说明 |
| --- | --- | --- |
| `s.sub(i, j)`（Lua 1-based 正负闭区间） | `s.slice(start, end)` / `s.char_slice(start, end = ...)`（0-based 半开标量区间）或 `s.byte_slice(start, end = ...)` / `s.get(start, end)`（0-based 半开字节区间） | 彻底移除 1-based 索引陷阱 |
| `string.utf8_len(s)` / `s.utf8_len()` -> `(?i64, ?i64)` | `Bytes.from_string(s).validate_utf8()! -> !Utf8Error usize`（或已知合法 `String` 直接用 `s.char_len()`） | 用受检 `!Utf8Error` 取代 Lua 的 `(nil, err_pos)` 二元组 |
| `s.utf8_codes()` / `s.utf8_offset(...)` / `s.utf8_codepoint(...)` | `s.char_indices_iter()`（产出 `(usize, char)`）、`s.chars_iter()`、`s.encode_utf8()` | 原生惰性 `Iterator` |
| `string.utf8_charpattern` | `pattern.UTF8_CHAR_PATTERN` | 归入 `std.pattern` |
| 需惰性遍历字符串而非立即分配 `List` | `s.chars_iter()` / `s.bytes_iter()` / `s.lines_iter()` / `s.char_indices_iter()`（原有的 `s.chars()` / `s.bytes()` / `s.lines()` / `s.char_indices()` 保持返回 `List`） | 兼顾零分配流式短路与随机索引 |

### 3.3 字节缓冲与二进制编解码 (`std.bytes`)

| 旧版 API（已移除） | 新版惯用 API | 说明 |
| --- | --- | --- |
| `bytes.to_hex(list_of_u8)` / `bytes.from_hex(s) -> !PackError List` | `buf.to_hex()` / `Bytes.from_hex(s)! -> !DecodeError Bytes`（或 `bytes.to_hex(buf)` / `bytes.from_hex(s)!`） | 直接基于紧凑 `Bytes` 而非盒装 `List`，错误细化为 `DecodeError.{InvalidLength, InvalidByte, InvalidPadding}` |
| `bytes.to_base64(list)` / `bytes.from_base64(s) -> !PackError List` | `buf.to_base64()` / `Bytes.from_base64(s)! -> !DecodeError Bytes` | 同上 |
| `buf.to_utf8_string()`（损失替换或未校验） | `buf.to_string_utf8()! -> !Utf8Error String` / `buf.to_utf8() -> ?String` / `buf.to_utf8_lossy() -> String` | 显式区分严格受检 UTF-8、可选解析与有损替换 |
| 手写下标遍历 `Bytes` | `for b in buf` 或 `buf.iter().map(...)` | `Bytes` 原生实现 `IntoIterator`（元素静态推导为 `u8`） |

### 3.4 数学与随机数 (`std.math` & `std.rand`)

| 旧版 API（已移除） | 新版惯用 API | 说明 |
| --- | --- | --- |
| `math.huge` | `math.INFINITY` / `math.NEG_INFINITY` | 标准 IEEE-754 命名 |
| `math.ult(a, b)` | `a.as(u64) < b.as(u64)` | 直接使用原生无符号标量类型 `u64` |
| `math.to_integer(x)` | `x.trunc_i64()`（浮点）或匹配 `v: i64` | 语义明确 |
| `math.math_type(x)` | `type_of(x)` 或模式匹配 `i64` / `f64` | 使用一等 `Type` |
| `math.modf(x)` | `math.split_fract(x)` / `x.split_fract()` | 返回 `(integral: f64, fractional: f64)` |
| `rand.random()` / `rand.random_int(lo, hi)` / `rand.random_up_to(n)` | `rand.float()`、`rand.int_range(lo, hi)`（闭区间 `[lo, hi]`）、`rand.next_u64()`、`rand.bool(.probability = 0.5)` | 亦支持显式 `let rng = rand.Rng.from_seed(42)` 与能力拦截 `rand.with_seeded_rng(42, \|\| ...)` |
| `rand.choice(xs)` | `rand.choose(xs) -> ?Any` | 空列表安全返回 `null` |

### 3.5 模式匹配 (`std.pattern`)

| 旧版 API（已移除） | 新版惯用 API | 说明 |
| --- | --- | --- |
| `pattern.gmatch(text, pat)`（返回 `List`） | `pattern.find_iter(text, pat)! -> !PatternError PatternMatchIter` | 惰性迭代产出 `PatternMatch { start, end, matched, captures }` |
| `pattern.gsub(text, pat, repl, .limit = ...)` | `pattern.replace(text, pat, repl, .limit = 0)! -> !PatternError (String, usize)` | 清晰英文动词命名 |
| `pattern.gsub_with` / `pattern.gsub_map` | `pattern.replace_with`（回调接收 `PatternMatch`，且自动传播 `!Error`/`#Effect`）/ `pattern.replace_map` | 类型安全回调 |
| `pattern.UTF8_CHARPATTERN` | `pattern.UTF8_CHAR_PATTERN` | `SCREAMING_SNAKE_CASE` 常量规范 |

### 3.6 路径、I/O、文件系统与子进程 (`std.path`, `std.io`, `std.fs`, `std.process`)

| 旧版 API（已移除） | 新版惯用 API | 说明 |
| --- | --- | --- |
| 裸 `String` 路径与 `path.file_stem(p)` | `Path.new("a/b.txt")`、`p.join("c")`、`p.stem()`、`p.with_file_name("d.txt")` | `std.io`、`std.fs`、`std.process` 所有路径参数同时接受 `Path` 与 `String` |
| `io.open(path, "r")` / `File.open(path, .mode = "wb")` | `File.open(path, .mode = .read)!` / `File.with_open(path, mode = .write) do \|f\| { ... }!` | 名义枚举 `OpenMode.{read, write, append, read_write, create_new}` |
| `io.warn(...)` / `f.read_number()` / `io.open_file` | `eprintln(...)`、`f.read_line()!` + `.parse(f64)!`、`File.open` | 移除 Lua 专用遗留接口 |
| 整文件二进制读写使用盒装 `List` | `fs.read_raw_bytes(path)! -> !IoError Bytes`、`fs.write_raw_bytes(path, bytes)!`（`File.read_bytes(.max_bytes = -1)` / `File.write_bytes(list)` 仍保留列表接口） | 原生紧凑 `Bytes` 零盒装传输 |
| `f.lines()` 返回一次性 `List` | `f.lines() -> FileLines`（惰性按行读取迭代器） | 流式大文件友好 |
| `fs.create_dir_all(p)` | `fs.create_dir(p, parents = true)!` | 命名可选参数 `.parents: bool = false` 合并重复函数 |
| `fs.remove_dir_all(p)` | `fs.remove_dir(p, recursive = true)!` 或通用 `fs.remove(p, recursive = true)!` | 明确目录递归删除语义（文件删除用 `fs.remove_file(p)!`） |
| `fs.metadata(p).file_type: String` 与 `.is_dir: bool` 字段 | `fs.metadata(p)!.kind: FileKind`（`FileKind.{file, directory, symlink, other}`）与 `.is_dir()` / `.is_file()` / `.is_symlink()` 方法 | 名义枚举替代字符串比较 |
| 手动遍历 `fs.read_dir(p)` 拼接路径判断类型 | `fs.iter_dir(p)! -> !IoError DirIter`（惰性产出 `DirEntry { name, path, kind: FileKind }`）或 `fs.read_dir_entries(p)!` | 直接携带 `kind: FileKind` 与 `entry.to_path()` |
| `process.exec(prog, .args = ..., .stdin_data = ..., .cwd = ...)` | `process.run(prog, .args = [], .stdin = null, .cwd = null)!`、`Command.new(prog).arg(...).run()!` 或 `Command.spec { prog, arg1, cwd: dir }!` | `CommandOutput.is_success()` 取代 `.success()`；`process.current_pid()` 收敛为 `process.pid()` |

### 3.7 时间、JSON、协程、任务与同步 (`std.time`, `std.json`, `std.coroutine`, `std.task`, `std.sync`)

| 旧版 API（已移除） | 新版惯用 API | 说明 |
| --- | --- | --- |
| `time.difftime(t2, t1)` / `DateTime.from_unix_local(ts)` | `t2 - t1` / `DateTime.from_unix(ts, .utc = true)` 与 `dt.to_unix(.utc = true)` | 合并 UTC/本地时间转换参数；支持 `time.with_mock_clock(start_ms, \|\| ...)` 测试拦截 |
| `DateTime` 字段 `weekday: i64` | 名义枚举方法 `dt.weekday() -> Weekday`（`sunday`..`saturday`） | 消除 0/1 起始星期编号歧义 |
| `MyStruct.from_json(raw)!` / 手工拼 `Map` | `json.decode(MyStruct, raw, .allow_unknown_fields = true)!`、`json.encode(val, .indent = 0)`、`json.object { a: 1, b: "ok" }` | 一等 `Type` 解码与双变参对象字面量构建 |
| `CoroutineStatus.normal`、`co.step()` 返回 `(String, Any)` | `CoroutineStatus.{suspended, running, dead}`、`co.step()! -> !CoroutineError CoroutineStep`（`yielded(val)` / `finished(val)`） | 精确反映多提示符定界延续状态与名义步骤枚举 |
| `Semaphore.release_many(n)` | `Semaphore.release(count = n)` | 命名默认参数 `.count: i64 = 1` |
| `Barrier.wait() -> bool`（破损时未受检） | `Barrier.wait()! -> !SyncError bool`（`SyncError.BrokenBarrier`） | 参与者取消导致栅栏破损时可受检恢复 |

### 3.8 迭代器、流与受检 FFI (`std.iter`, `std.stream`, `std.ffi`, `regex`)

| 旧版写法 | 新版惯用 API |
| --- | --- |
| 仅 `List` 有 `.map`/`.filter`，每次操作分配新 `List`；`Range`/`Set`/`Map`/`Deque`/`Bytes` 无法链式组合 | 任意集合/区间/生成器调用 `.iter().filter(...).map(...).enumerate().take(k).collect()` 全程惰性求值，并推导静态元素类型 |
| 手写复杂状态机实现自定义 `Iterator` | `use std.stream` 后编写普通函数调用 `stream.emit(x)#`，再用 `stream.to_iter(\|\| gen()#)` 转为标准惰性 `Iterator` |
| 私有内建 `.__ffi_dlopen'builtin` / `.__ffi_dlsym'builtin` / `.__ffi_call2'builtin` + 裸 `i64` 地址 | `use std.ffi` + `let c = ffi.load("m") { hypot: fn(f64, f64) -> f64, puts: ffi.func(fn(CStr) -> i32, callconv = .c) }!`，配合 `Ptr`、`CStr`、`SliceRo`、`BufMut` 与 `c.attach_finalizer(obj, "free_sym", raw_ptr)!`（详见 [`docs/ffi.md`](ffi.md)） |

---

## 4. 改造前后代码对比

### 示例 1：配置读取、解析与命令执行

**旧版（Lua 风格）：**

```nessa
use std.fs
use std.path
use std.process

fn run_job(root: String) -> !IoError i64 {
    let cfg_path = path.join(root, "port.txt")
    let raw = fs.read_to_string(cfg_path)!
    let port_num = tonumber(raw.trim())
    if port_num == null {
        return error IoError.InvalidInput("bad port")
    }
    let out = process.exec("echo", args = ["port=" ++ to_string(port_num)], cwd = root)!
    if out.success() { port_num.as(i64) } else { -1 }
}
```

**新版（Nessa 惯用风格）：**

```nessa
use std.fs
use std.process.Command

fn run_job(root: Path) -> ![IoError, ParseError] i64 {
    let port = fs.read_to_string(root.join("port.txt"))!.trim().parse(i64)!
    let cmd = Command.spec { "echo", "port={port}", cwd: root }!
    let out = cmd.run()!
    if out.is_success() { port } else { -1 }
}
```

### 示例 2：惰性迭代流水线 + 受检错误短路

```nessa
fn parse_positive_sum(lines: String) -> !ParseError i64 {
    -- 全程零中间 List 分配；遇到非法数字立即短路返回 !ParseError！
    lines.lines_iter()
        .map(|s| s.trim())
        .filter(|s| not s.is_empty())
        .fold(0, |acc, s| -> !ParseError i64 {
            let n = s.parse(i64)!
            if n > 0 { acc + n } else { acc }
        })!
}
```

### 示例 3：受检声明式 C FFI（`std.ffi`）

```nessa
use std.ffi
use std.ffi.types.{CStr, Ptr}

const libm = ffi.must_load("m") {
    hypot: fn(f64, f64) -> f64,
    floor: ffi.func(fn(f64) -> f64, callconv = .c),
}

fn main() {
    let d: f64 = libm.hypot(3.0, 4.0)
    assert_eq(libm.floor(d), 5.0)
}
```

---

## 5. 下游项目（如 `lava`）升级核查清单

1. **检查 `prelude` 隐式导入**：若源码直接使用了 `exec` / `shell` / `SeekFrom` / `flush_stdout`，补充显式 `use std.process.{Command, run, shell}` 或 `use std.io.{SeekFrom, flush_stdout}`。
2. **全局替换 `tonumber` 与 `parse_i64`/`parse_f64`/`parse_int_radix`**：改为 `expr.try_parse(i64)`（返回 `?i64`）或 `expr.parse(i64)!`（返回 `!ParseError i64`）。
3. **检查 `String.sub` 与 `utf8_len`**：将 1-based `s.sub(...)` 改为 0-based `s.slice(start, end)` / `s.get(start, end)`；将 `utf8_len` 改为 `validate_utf8()!` 或 `char_len()`。
4. **更新 `File.open`、`fs` 与 `process` 调用**：字符串打开模式 `"r"` / `"w"` / `"a"` 改为 `.read` / `.write` / `.append`（或 `OpenMode.read`），推荐升级为 `File.with_open(path, mode = .read) do |f| { ... }!`；`fs.create_dir_all(p)` -> `fs.create_dir(p, parents = true)`，`fs.remove_dir_all(p)` -> `fs.remove_dir(p, recursive = true)`，`metadata.file_type == "dir"` / `entry.is_dir` -> `metadata.kind == FileKind.directory` / `entry.is_dir()`；`process.exec(..., stdin_data = ...)` -> `process.run(..., stdin = ...)` 且 `.success()` -> `.is_success()`。
5. **善用惰性迭代接口**：`File.lines` 现返回惰性 `FileLines`（若需要 `List` 追加 `.collect()`）；`String` 保留 `.chars()`/`.bytes()`/`.lines()` 返回 `List`，同时新增零分配惰性版 `.chars_iter()`/`.bytes_iter()`/`.lines_iter()`/`.char_indices_iter()`。
6. **检查 `Barrier.wait()`**：现返回 `!SyncError bool`，在调用处添加 `!` 或 `! { SyncError.BrokenBarrier => ... }`。
7. **检查枚举变体命名**：确认所有自定义及标准库非 Error 枚举变体遵循 `snake_case`（`Ordering.less`、`Either.left`、`OpenMode.read`、`SeekFrom.start`、`FileKind.file`、`Weekday.monday`、`CallConv.c`），Error 枚举变体遵循 `PascalCase`（`IoError.NotFound`、`ParseError.InvalidFormat`、`FfiError.SymbolNotFound`）。
