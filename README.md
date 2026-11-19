# Nessa

**Nessa** 是一门以**代数效应（Algebraic Effects）**、**结构化并发（Structured Concurrency）**与**极快 AOT 编译**为核心设计的静态类型编程语言，底层引擎采用 Rust 实现，兼具脚本级的快速运行体验与系统级的运行时可控性。

---

## 核心特性

- **代数效应与多提示符定界延续**：通过同步 `effect` 与异步 `async effect` 统一建模副作用、生成器流与能力注入（如控制台 I/O、时钟、随机源），消除传统异步编程中的函数着色问题。
- **结构化并发与非阻塞调度**：基于 Task Tree 管理任务生命周期，内置 Work-Stealing 调度器、I/O Reactor 与任务级非阻塞同步原语（`Channel`、`Mutex`、`RwLock`、`Semaphore`、`Barrier` 等）。
- **静态类型与渐进灵活性**：以静态类型推导、名义类型、受检错误联合（`!Error`）、可选流（`?T`）与一等 `Type` 值为基础，同时通过 `Any` 类型提供动态灵活性与 C-ABI FFI 互操作。
- **多后端编译与执行管线**：
  - **极快 AOT 编译**：`Source (.ns) -> Lexer -> Parser -> Resolution -> NIR -> Codegen -> NSBC Archive (.nsbc)`，支持内容寻址 `.nsbc` 编译缓存与脱离源码的独立归档运行。
  - **分层执行引擎**：字节码解释器配合分层 JIT（`auto` / `off` / `tier1` / `tier2`）与 MMTk Immix 垃圾回收器。
  - **WebAssembly 编译目标**：支持将 Nessa 程序编译为 `wasm32-wasi` 与 `wasm32-freestanding`。

---

## 快速示例

下面的示例展示了 Nessa 的代数效应声明与 `in-place` 效应处理器（见 [`tests/hello.ns`](tests/hello.ns)）：

```nessa
effect yield(data: Any)

fn hello() -> #yield Any:
    yield("Hello, World!")
    yield(234324)

fn main():
    hello()# {
        yield(data) =>
            println(data)
    }
```

运行示例：

```bash
nessa run tests/hello.ns
```

---

## CLI 使用指南

`nessa` CLI（[`engine/nessa/src/main.rs`](engine/nessa/src/main.rs)）提供以下命令与选项：

```bash
# 运行源码、包目录、.nsbc 字节码归档或 .wasm 模块
nessa run <file.ns|file.nsbc|file.wasm|directory> [-- <args...>]

# 编译为 .nsbc 归档（默认）或 WebAssembly 目标
nessa build <file.ns|directory> [--target <nsbc|wasm32-wasi|wasm32-freestanding>] [-o <path>]

# 仅执行类型与效应检查
nessa check <file.ns|directory>

# 反汇编字节码或 .wasm
nessa disasm <file.ns|file.nsbc|file.wasm|directory>

# 查看版本与帮助
nessa version
nessa help
```

常用编译与运行选项：

- `-O0` / `-O1` / `-O2` / `-O3` / `-O`（`--opt-level <0|1|2|3>`）：设置 AOT 优化级别（`-O` 等价于 `-O2`，默认 `-O0`）
- `--jit <auto|off|tier1|tier2>`：配置 `run` 的分层 JIT 模式（默认 `auto`）
- `--locked`：严格校验 `package.lock`
- `--emit-ast-dump` / `--emit-bc-dump`：在输入旁输出 AST 或字节码反汇编转储
- 环境变量：`NESSA_OPT_LEVEL`、`NESSA_JIT`、`NESSA_CACHE_DIR`（设为 `off` 可禁用缓存）、`NESSA_NO_CACHE=1`

---

## 仓库结构

| 路径 | 说明 |
| --- | --- |
| `engine/lexer`、`engine/parser`、`engine/ast`、`engine/source_span`、`engine/diagnostic` | 词法分析、语法解析、AST、源码位置与诊断系统 |
| `engine/resolution`、`engine/type_pool`、`engine/nir`、`engine/codegen` | 名称解析、类型与效应分析、NIR 中间表示降级与优化、字节码生成 |
| `engine/nsbc`、`engine/nsbc_io` | Nessa 字节码格式定义与 `.nsbc` 归档序列化 |
| `engine/runtime`、`engine/interpreter`、`engine/jit`、`engine/wasm` | 运行时对象模型、字节码解释器、分层 JIT 与 WebAssembly 后端 |
| `engine/gc`、`engine/stack_pool`、`engine/scheduler` | MMTk GC 集成、定界延续栈段管理与任务调度器 |
| `engine/ffi`、`engine/regex_ffi`、`engine/str_interner`、`engine/initialization` | C-ABI FFI 桥接、正则 FFI、字符串驻留与引擎初始化 |
| `engine/driver`、`engine/pkg_manager`、`engine/nessa`、`engine/test` | 编译运行管线编排、包管理、CLI 入口与集成测试 |
| `library/std/` | Nessa 标准库（`builtin`、`iter`、`stream`、`collections`、`string`、`bytes`、`io`、`fs`、`net`、`task`、`sync`、`json`、`ffi`、`coroutine`、`wasm` 等） |
| `library/regex/` | 基于 `std.ffi` 与 `std.iter` 构建的扩展正则包（`nessa/regex`） |
| `docs/` | 语言规范、代数效应与并发设计、标准库文档、FFI 指南与引擎架构文档 |

---

## 构建与测试

仓库统一通过 `scripts/cargo.sh` 封装 Cargo 构建与缓存锁，并使用 `scripts/test-parallel.sh` 并行执行测试套件：

```bash
# 构建 nessa CLI
bash scripts/cargo.sh build -p nessa

# 检查全工作区编译
bash scripts/cargo.sh check --workspace --all-targets

# 并行运行全部 Rust 单元测试与集成测试
NESSA_TEST_JOBS=10 NESSA_TEST_THREADS=3 bash scripts/test-parallel.sh

# 代码格式与 Clippy 检查
bash scripts/cargo.sh fmt --all -- --check
bash scripts/cargo.sh clippy --workspace --all-targets -- -D warnings
```

---

## 相关文档

- [系统架构设计](docs/dev/architecture.md)
- [Nessa 代码风格与命名规范](docs/code-style.md)
- [标准库总览 (`std`)](docs/standard-library/overview.md)
- [FFI 外部函数接口](docs/ffi.md)

---

## 开源协议

本项目采用 **Mozilla Public License Version 2.0 (MPL-2.0)** 开源协议，完整协议文本见 [LICENSE](LICENSE)。
