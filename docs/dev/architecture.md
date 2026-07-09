# Nessa 系统架构设计

## 一、设计目标

- **极快 AOT 编译**：编译速度足以覆盖脚本使用场景，做到"写完即运行"的体验
- **代数效应驱动**：以代数效应为核心统一副作用处理，消除函数着色问题
- **结构化并发**：基于 Task Tree 的并发模型，保证安全的生命周期管理
- **渐进式类型**：静态类型为主，`Any` 类型提供动态灵活性和 FFI 互操作
- **跨平台**：支持 x86_64 / ARM64 / RISCV64，Linux / macOS / Windows

---

## 二、总体架构概览

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                          Nessa Engine 总体架构                               │
│                                                                             │
│  ┌─────────────────────── 编译期 (Compile-Time) ──────────────────────────┐ │
│  │                                                                        │ │
│  │  ┌──────┐    ┌──────┐    ┌─────┐    ┌────────────┐    ┌───────────┐   │ │
│  │  │Source│───▶│Lexer │───▶│Parse│───▶│ Resolution │───▶│    NIR    │   │ │
│  │  │ .ns  │    │      │    │  r  │    │            │    │  Lowering │   │ │
│  │  └──────┘    └──────┘    └─────┘    └────────────┘    └─────┬─────┘   │ │
│  │                                                             │         │ │
│  │                                          ┌──────────────────┤         │ │
│  │                                          ▼                  ▼         │ │
│  │                                   ┌────────────┐    ┌────────────┐    │ │
│  │                                   │  Codegen   │    │  NIR Opts  │    │ │
│  │                                   │ (Bytecode) │    │ (可选优化)  │    │ │
│  │                                   └─────┬──────┘    └────────────┘    │ │
│  │                                         │                             │ │
│  │                                         ▼                             │ │
│  │                                  ┌─────────────┐                      │ │
│  │                                  │ NSBC Archive│                      │ │
│  │                                  │  (.nsbc)    │                      │ │
│  │                                  └─────────────┘                      │ │
│  │                                                                        │ │
│  └────────────────────────────────────────────────────────────────────────┘ │
│                                                                             │
│  ┌─────────────────────── 运行期 (Runtime) ───────────────────────────────┐ │
│  │                                                                        │ │
│  │  ┌──────────────────────────────────────────────────────────┐          │ │
│  │  │              Interpreter (字节码解释器)                    │          │ │
│  │  │                                                          │          │ │
│  │  │  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌─────────┐ │          │ │
│  │  │  │ Dispatch │  │ Register │  │  Effect  │  │  Error  │ │          │ │
│  │  │  │   Loop   │  │   File   │  │  Stack   │  │  Stack  │ │          │ │
│  │  │  └──────────┘  └──────────┘  └──────────┘  └─────────┘ │          │ │
│  │  └──────────────────────────────────────────────────────────┘          │ │
│  │                                                                        │ │
│  │  ┌──────────────────────────────────────────────────────────┐          │ │
│  │  │              Runtime Subsystems                           │          │ │
│  │  │                                                          │          │ │
│  │  │  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌─────────┐ │          │ │
│  │  │  │Type Pool │  │  MMTK    │  │Scheduler │  │  libuv  │ │          │ │
│  │  │  │& Metdata │  │  LXR GC  │  │  (W.S.)  │  │Event Lp │ │          │ │
│  │  │  └──────────┘  └──────────┘  └──────────┘  └─────────┘ │          │ │
│  │  │                                                          │          │ │
│  │  │  ┌──────────┐  ┌──────────┐  ┌──────────┐              │          │ │
│  │  │  │  Task    │  │  String  │  │   FFI    │              │          │ │
│  │  │  │Stack Mgr │  │ Interner │  │  Bridge  │              │          │ │
│  │  │  └──────────┘  └──────────┘  └──────────┘              │          │ │
│  │  └──────────────────────────────────────────────────────────┘          │ │
│  │                                                                        │ │
│  └────────────────────────────────────────────────────────────────────────┘ │
│                                                                             │
│  ┌───────────── 基础设施 (Infrastructure) ────────────────────────────────┐ │
│  │                                                                        │ │
│  │  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐              │ │
│  │  │Diagnostic│  │   Pkg    │  │ NSBC I/O │  │  Driver  │              │ │
│  │  │& Errors  │  │ Manager  │  │ (序列化)  │  │(编排入口)│              │ │
│  │  └──────────┘  └──────────┘  └──────────┘  └──────────┘              │ │
│  │                                                                        │ │
│  └────────────────────────────────────────────────────────────────────────┘ │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## 三、三类数据域

整个引擎的数据严格分为三类，生命周期和访问模式各不相同：

| 数据域 | 生命周期 | 内容 | 典型结构 |
|--------|---------|------|---------|
| **编译时数据** | 单次编译过程 | AST、NIR、符号表、诊断信息 | `Arena<Node>`, `SymbolTable`, `DiagnosticBag` |
| **运行时元数据** | 引擎全生命周期（只读） | 字节码、类型池、方法表、字符串池、Effect 签名 | `TypePool`, `MethodTable`, `BytecodeStore` |
| **运行时数据** | 动态（GC 管理） | 堆对象、栈帧、寄存器值、Task 状态 | `TaggedValue`, `CallFrame`, `TaskState` |

---

## 四、编译管线 (Compile Pipeline)

### 4.1 阶段总览

```
Source Text
    │
    ▼
┌──────────────────────────────────────────────────────────────────────┐
│ Phase 1: Lexical Analysis (lexer)                                    │
│   Source Text ──▶ TokenStream                                        │
│   - 关键词识别、缩进敏感的布局 token 生成                               │
│   - 字符串插值展开                                                    │
│   - Span 追踪（用于诊断）                                             │
└──────────────────────────────────────────────────────────────────────┘
    │ TokenStream
    ▼
┌──────────────────────────────────────────────────────────────────────┐
│ Phase 2: Parsing (parser)                                            │
│   TokenStream ──▶ AST                                                │
│   - 递归下降解析，覆盖完整文法                                         │
│   - 产出带 Span 的 AST 节点（Arena 分配）                              │
│   - 错误恢复：跳过到下一个同步点继续解析                                │
└──────────────────────────────────────────────────────────────────────┘
    │ AST
    ▼
┌──────────────────────────────────────────────────────────────────────┐
│ Phase 3: Resolution (resolution)                                     │
│   AST ──▶ Resolved AST + SymbolTable + TypePool                      │
│                                                                      │
│   3a. Name Resolution                                                │
│       - 构建作用域树（Type ↔ Scope 双射，类型/函数/块层层嵌套）             │
│       - 解析所有标识符绑定                                              │
│       - use 语句展开与可见性检查                                         │
│       - 统一 . 投影解析（mod.fn 与 struct.method 同一机制）               │
│                                                                      │
│   3b. Type Resolution                                                │
│       - 构建类型格（Type Lattice）: Any ↔ NoReturn                      │
│       - 类型推断（双向类型推断 Bidirectional Type Inference）             │
│       - Qualified Type 展开（#Effect / !Error / ?Optional）            │
│       - 计算 128-bit TypeID                                           │
│       - 构建 TypePool 和 Type Index 映射                               │
│                                                                      │
│   3c. Effect Resolution                                              │
│       - 收集所有 effect 声明                                            │
│       - Evidence Passing 编译：为每个 effect 生成隐式参数                 │
│       - 验证 handler 覆盖完整性                                         │
│       - 标记需要 dynamic dispatch 的 effect call                       │
│                                                                      │
│   3d. Trait & Method Resolution                                      │
│       - Trait 约束求解                                                 │
│       - 方法查找与 vtable 构建                                         │
│       - 特殊函数（new/apply/update/iter）绑定                           │
└──────────────────────────────────────────────────────────────────────┘
    │ Resolved AST + Metadata
    ▼
┌──────────────────────────────────────────────────────────────────────┐
│ Phase 4: NIR Lowering (nir)                                          │
│   Resolved AST ──▶ NIR (Normalized Intermediate Representation)      │
│                                                                      │
│   - 语法糖脱糖（for → while+iter, ?/! → match, string interp → concat  │
│   - 控制流标准化（所有分支 → 条件跳转 + 基本块）                            │
│   - Effect 调用标准化（evidence passing 参数注入）                       │
│   - 模式匹配编译（decision tree / backtracking automaton）              │
│   - Lambda 捕获分析与闭包转换                                           │
│   - 常量折叠与简单优化                                                  │
└──────────────────────────────────────────────────────────────────────┘
    │ NIR
    ▼
┌──────────────────────────────────────────────────────────────────────┐
│ Phase 5: Code Generation (codegen)                                   │
│   NIR ──▶ NSBC (Nessa Serialized ByteCode)                           │
│                                                                      │
│   - 寄存器分配（32 个通用寄存器，ARM 风格 ABI，线性扫描）                 │
│   - 32-bit 指令编码（含 2-bit 寻址模式）                                 │
│   - Safe point 插入（循环回边、函数调用点）                               │
│   - Stack map 生成（GC root 追踪）                                     │
│   - 常量池构建                                                         │
│   - 调试信息生成（源码映射）                                              │
└──────────────────────────────────────────────────────────────────────┘
    │ NSBC
    ▼
┌──────────────────────────────────────────────────────────────────────┐
│ Phase 6: Archive Emission (nsbc_io)                                  │
│   NSBC ──▶ .nsbc Archive File                                        │
│                                                                      │
│   - Section 组装（CODE / METADATA / STACK_MAPS / ...）                │
│   - 可读文本格式 与 压缩二进制格式 双相                                │
│   - SHA-256 校验和                                                    │
│   - 目标平台标记（arch + os）                                         │
└──────────────────────────────────────────────────────────────────────┘
```

### 4.2 编译模式

| 模式 | 说明 | 路径 |
|------|------|------|
| **开发模式** | 最小化优化，最快编译，保留全部调试信息 | Lex → Parse → Resolution → NIR → Codegen（无优化）→ 直接执行 |
| **发布模式** | 完整优化，生成归档文件 | Lex → Parse → Resolution → NIR → NIR Opts → Codegen → Archive |
| **检查模式** | 仅做类型检查和诊断，不生成代码 | Lex → Parse → Resolution → 报告诊断 |

---

## 五、运行时架构 (Runtime Architecture)

### 5.1 值表示 (Value Representation)

```
Tagged Pointer (64-bit)
┌────────────────────────────────────────────────────────────────┬───┐
│                      Payload (61 bits)                         │Tag│
│                                                                │3b │
└────────────────────────────────────────────────────────────────┴───┘

Tag 编码:
  000  HeapObject    → payload 为堆对象指针（8 字节对齐，低 3 位自然为 0）
  001  Immediate     → payload 为立即数，内部再编码区分具体类型（见下）
  01x  Reserved      → 保留扩展
  1xx  Reserved      → 保留扩展

Immediate 子编码 (payload 61 bits 内部):
  高 4 位为子类型 tag，剩余 57 bits 为数据：

  0000  i_small    → 57-bit 有符号整数（覆盖 i8~i64/isize 常见值域）
                     溢出时自动提升为堆上 BigInt 对象
  0001  u_small    → 57-bit 无符号整数（覆盖 u8~u64/usize 常见值域）
                     溢出时自动提升为堆上 BigUint 对象
  0010  f64_bits   → 57-bit 压缩浮点（truncated mantissa）
                     精度不足时回退为堆上 f64 对象
  0011  bool       → bit 0 = true/false
  0100  symbol     → 内部符号值（enum variant tag、.ok/.err 等）
  0101  null       → 表示 ?T 的空值（optional 的 None 状态）
  0110  unit       → Unit 值 ()
  0111  char       → Unicode 码点（21 bits 即够用）
  1xxx  reserved   → 保留扩展

  注：u128/i128/big int 始终分配在堆上，不适用立即数编码。
      f32 值在运算时提升为 f64 处理，存储时使用 f64_bits 或堆对象。

  数值类型完整列表（与 std intrinsic 定义一致）：
    无符号整数: u8, u16, u32, u64, u128, usize
    有符号整数: i8, i16, i32, i64, i128, isize
    大整数:     BigInt（任意精度）
    浮点数:     f32, f64
    布尔:       bool
    字符:       char
```

堆对象布局 (Heap Object Layout):

```
┌──────────────────────────────────────────────────────────┐
│ Object Header (16 bytes)                                  │
│  ┌─────────────────┬──────────────────────────────────┐  │
│  │  TypeIndex (u32) │  GC Metadata (u32)               │  │
│  ├─────────────────┴──────────────────────────────────┤  │
│  │  Identity Hash / Forwarding Pointer (u64)           │  │
│  └─────────────────────────────────────────────────────┘  │
├──────────────────────────────────────────────────────────┤
│ Payload (按类型定义的字段排列)                             │
│  field_0: TaggedValue                                     │
│  field_1: TaggedValue                                     │
│  ...                                                      │
└──────────────────────────────────────────────────────────┘

TypeIndex → TypePool 查询获取完整的 128-bit TypeID 和类型元数据
```

### 5.2 字节码虚拟机 (Bytecode VM)

```
┌─────────────────────────── VM per Task ───────────────────────────┐
│                                                                    │
│  ┌────────────────────────────────────────────────────────────┐   │
│  │ Register File (32 GP registers × 64-bit TaggedValue)       │   │
│  │  r0..r31: TaggedValue (ABI: r0–r7 args, r19–r28 callee)    │   │
│  └────────────────────────────────────────────────────────────┘   │
│                                                                    │
│  ┌────────────────────────────────────────────────────────────┐   │
│  │ Call Stack (在 Task Stack 上)                               │   │
│  │                                                            │   │
│  │  ┌──────────────────────────────────────────────────────┐ │   │
│  │  │ CallFrame                                            │ │   │
│  │  │  return_pc: u32        (返回地址)                     │ │   │
│  │  │  base_reg: u8          (寄存器窗口基址)               │ │   │
│  │  │  func_id: FuncId       (当前函数标识)                 │ │   │
│  │  │  evidence: &[Handler]  (当前 effect handler 证据链)   │ │   │
│  │  │  saved_regs: [..]      (溢出的寄存器值)               │ │   │
│  │  └──────────────────────────────────────────────────────┘ │   │
│  │  ┌──────────────────────────────────────────────────────┐ │   │
│  │  │ CallFrame (caller)                                   │ │   │
│  │  │  ...                                                 │ │   │
│  │  └──────────────────────────────────────────────────────┘ │   │
│  │  ...                                                      │   │
│  └────────────────────────────────────────────────────────────┘   │
│                                                                    │
│  ┌────────────────────────────────────────────────────────────┐   │
│  │ PC: InstructionPointer                                      │   │
│  │ Status: Running | Suspended | Waiting | Finished            │   │
│  └────────────────────────────────────────────────────────────┘   │
│                                                                    │
└────────────────────────────────────────────────────────────────────┘

指令格式 (32-bit 固定宽度):
┌────────┬──────┬────────────────────────────────────────────────┐
│ Opcode │ amode│              Payload (22 bits)                 │
│ 8 bits │ 2 b  │  R: dst/src1/src2 · A: dst/base/imm12 · …     │
└────────┴──────┴────────────────────────────────────────────────┘

注: 指令编码格式按类别使用不同布局（R / A / J / C / E），
    Opcode 高 2 位编码格式类别；amode 仅对 Load 等寻址指令有效。
```

### 5.3 Task Stack Manager

```
┌─────────────────── Virtual Memory Space ──────────────────────────┐
│                                                                    │
│  mmap'd Task Stack Pool (segmented list 管理，可扩展)              │
│                                                                    │
│  ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌──────────┐             │
│  │ Stack 0  │ │ Stack 1  │ │ Stack 2  │ │ Stack 3  │  ...        │
│  │ (8 MB)   │ │ (8 MB)   │ │ (8 MB)   │ │ (8 MB)   │             │
│  │          │ │          │ │          │ │          │             │
│  │ ┌──────┐ │ │ ┌──────┐ │ │          │ │ ┌──────┐ │             │
│  │ │Active│ │ │ │Active│ │ │  (free)  │ │ │Active│ │             │
│  │ │8K+   │ │ │ │8K    │ │ │          │ │ │16K   │ │             │
│  │ │      │ │ │ │      │ │ │          │ │ │      │ │             │
│  │ │      │ │ │ │      │ │ │          │ │ │      │ │             │
│  │ │      │ │ │ │      │ │ │          │ │ │      │ │             │
│  │ ├──────┤ │ │ ├──────┤ │ │          │ │ ├──────┤ │             │
│  │ │Guard │ │ │ │Guard │ │ │          │ │ │Guard │ │             │
│  │ │ Page │ │ │ │ Page │ │ │          │ │ │ Page │ │             │
│  │ └──────┘ │ │ └──────┘ │ │          │ │ └──────┘ │             │
│  └──────────┘ └──────────┘ └──────────┘ └──────────┘             │
│                                                                    │
│  分配策略:                                                         │
│    1. spawn task → 从 free list 取一个 8MB slot                    │
│    2. madvise 预提交 8KB                                           │
│    3. 超出 8KB 时由 OS page fault 自动提交                          │
│    4. task 结束 → madvise(DONTNEED) 回收物理页，slot 归还 free list │
│                                                                    │
│  Segmented List 增长规则:                                          │
│    Pool 不是一次性 mmap 一整块巨大连续空间，而是采用分段列表管理：    │
│                                                                    │
│    Segment 0 (初始)     Segment 1           Segment 2              │
│    ┌────────────────┐   ┌────────────────┐   ┌────────────────┐    │
│    │ 64 slots       │   │ 128 slots      │   │ 256 slots      │    │
│    │ (64×8MB=512MB  │   │ (128×8MB=1GB   │   │ (256×8MB=2GB   │    │
│    │  虚拟地址空间)  │   │  虚拟地址空间)  │   │  虚拟地址空间)  │    │
│    └────────────────┘   └────────────────┘   └────────────────┘    │
│                                                                    │
│    - 初始时仅 mmap 第一个 segment（如 64 个 slot）                  │
│    - 当 segment 内所有 slot 用尽时，mmap 一个新 segment             │
│    - 新 segment 容量翻倍（64 → 128 → 256 → ...），摊还 O(1)       │
│    - 各 segment 不要求虚拟地址连续，由指针数组索引                   │
│    - 已 mmap 的 segment 在进程生命周期内不释放（仅 madvise 内部页） │
│    - 优势：无需预设最大容量，按需增长，不浪费连续虚拟地址空间        │
│                                                                    │
└────────────────────────────────────────────────────────────────────┘
```

### 5.4 Scheduler (Work-Stealing)

```
┌──────────────────── Scheduler Architecture ──────────────────────┐
│                                                                    │
│  ┌────────────────────────────────────────────────────────────┐   │
│  │ Main Thread (libuv event loop)                              │   │
│  │   - I/O 事件分发                                            │   │
│  │   - Timer 管理                                              │   │
│  │   - 外部回调入口                                            │   │
│  └────────────┬───────────────────────────────────────────────┘   │
│               │ wake up / submit ready tasks                      │
│               ▼                                                    │
│  ┌────────────────────────────────────────────────────────────┐   │
│  │ Worker Threads (N = CPU cores)                              │   │
│  │                                                             │   │
│  │  Worker 0          Worker 1          Worker 2               │   │
│  │  ┌───────────┐    ┌───────────┐    ┌───────────┐           │   │
│  │  │ Local Deq │    │ Local Deq │    │ Local Deq │           │   │
│  │  │ [T][T][T] │    │ [T]       │    │ [T][T]    │           │   │
│  │  └─────┬─────┘    └─────┬─────┘    └─────┬─────┘           │   │
│  │        │                │                │                  │   │
│  │        └───── steal ────┼───── steal ────┘                  │   │
│  │                         │                                   │   │
│  └────────────────────────────────────────────────────────────┘   │
│                                                                    │
│  调度策略:                                                         │
│    - 每个 Worker 拥有本地双端队列                                   │
│    - 新 task 优先 push 到当前 worker                               │
│    - 空闲 worker 从其他 worker 的队列尾部 steal                     │
│    - Safe-point 调度：在循环回边和函数调用处检查抢占标志              │
│    - Task 状态机: Ready → Running → Suspended/Waiting → Finished  │
│                                                                    │
│  Task Tree:                                                        │
│    main()                                                          │
│    ├── Task A (handler for async effect X)                         │
│    │   ├── Task A1                                                 │
│    │   └── Task A2                                                 │
│    └── Task B (handler for async effect Y)                         │
│        └── Task B1                                                 │
│                                                                    │
│    保证:                                                           │
│    - 父 task 退出时自动 cancel 所有子 task                          │
│    - handler 作用域边界自动 implicit await                          │
│    - 通过 task tree 进行 dynamic effect handler 查找                │
│                                                                    │
└────────────────────────────────────────────────────────────────────┘
```

### 5.5 GC (MMTK + LXR)

```
┌───────────────────── Memory Management ──────────────────────────┐
│                                                                    │
│  ┌────────────────────────────────────────────────────────────┐   │
│  │ MMTK (Memory Management Toolkit)                            │   │
│  │                                                             │   │
│  │  ┌─────────────┐  ┌─────────────┐  ┌─────────────────┐    │   │
│  │  │  Allocator  │  │  Collector  │  │   Work Packets  │    │   │
│  │  │  (per-task  │  │  (LXR GC)   │  │   (并行 GC 工作  │    │   │
│  │  │  bump alloc)│  │             │  │    单元化分派)   │    │   │
│  │  └─────────────┘  └─────────────┘  └─────────────────┘    │   │
│  │                                                             │   │
│  │  GC Root 来源:                                              │   │
│  │    1. 寄存器文件 (register file)                             │   │
│  │    2. 调用栈 (stack maps 定位引用)                           │   │
│  │    3. 全局变量表                                             │   │
│  │    4. Effect handler 证据链                                  │   │
│  │                                                             │   │
│  │  Safe-point 机制:                                           │   │
│  │    - 循环回边 (back-edge) 处插入 safe-point 检查              │   │
│  │    - 每次函数调用/返回为隐式 safe-point                      │   │
│  │    - safe-point 触发时：暂停 task、扫描 stack map、           │   │
│  │      标记可达对象                                            │   │
│  │                                                             │   │
│  └────────────────────────────────────────────────────────────┘   │
│                                                                    │
└────────────────────────────────────────────────────────────────────┘
```

---

## 六、Effect 系统运行时支持

### 6.1 Evidence Passing (核心路径 — 静态分派)

```nessa
-- 编译前:
fn greet():
    let name = read_line()#     -- 触发 effect
    print("hello, {name}")

-- 编译后 (evidence passing 转换):
fn greet(ev_read_line: &Handler_read_line):
    let name = ev_read_line.invoke()   -- 直接调用 handler, O(1)
    print("hello, {name}")

-- 调用处:
greet() # {
    read_line() => resume("world")
}
-- 展开为:
let handler = Handler_read_line { impl: |_| "world" }
greet(&handler)
```

### 6.2 Dynamic Effect Dispatch (回退路径 — Any 类型)

```
当值类型为 Any 无法静态确定 effect handler 时:
  1. 沿当前 task 的调用栈搜索 handler frame
  2. 若未找到，沿 task tree 向父 task 搜索
  3. 找到 handler 后缓存到 effect call cache (per-task)
  4. 未找到则触发 unhandled effect 运行时错误
```

### 6.3 Delimited Continuation (高级控制流)

```
shift/reset 机制:
  reset {
    let x = shift k => {
      k(1) + k(2)   // k 是 delimited continuation
    }
    x * 10
  }
  // 结果: 10 + 20 = 30

运行时实现:
  - shift 捕获当前栈帧到 reset 点之间的所有帧
  - 帧被复制到堆上形成 Continuation 对象
  - resume(k, val) 恢复 continuation：
    将保存的帧压回栈，设置返回值，跳转到捕获点
  - one-shot continuation 可以 move 而非 copy（零开销），提供显示clone方法
```

---

## 七、Crate 依赖架构

```
                    ┌─────────┐
                    │  nessa  │ (bin: CLI 入口)
                    └────┬────┘
                         │
                    ┌────▼────┐
                    │ driver  │ (编译 & 执行编排)
                    └────┬────┘
                         │
          ┌──────────────┼──────────────┬──────────────┐
          ▼              ▼              ▼              ▼
    ┌──────────┐  ┌───────────┐  ┌──────────┐  ┌───────────┐
    │  parser  │  │ resolution│  │  codegen  │  │interpreter│
    └────┬─────┘  └─────┬─────┘  └────┬─────┘  └─────┬─────┘
         │              │             │               │
    ┌────▼─────┐  ┌─────▼─────┐  ┌───▼────┐   ┌─────▼─────┐
    │  lexer   │  │ type_pool │  │  nir   │   │  runtime  │
    └────┬─────┘  └─────┬─────┘  └───┬────┘   └─────┬─────┘
         │              │            │               │
    ┌────▼─────┐        │       ┌────▼──────┐  ┌────▼──────┐
    │diagnostic│        │       │instruction│  │    gc     │
    └──────────┘        │       └───────────┘  └───────────┘
                        │                            │
                   ┌────▼──────┐              ┌──────▼─────┐
                   │str_interner│              │ scheduler  │
                   └───────────┘              └────────────┘

    横向共享 crate:
    ┌──────────┐  ┌──────────┐  ┌──────────────┐
    │  ast     │  │ nsbc_io  │  │ pkg_manager  │
    └──────────┘  └──────────┘  └──────────────┘
    (parser,       (codegen,     (driver)
     resolution,    driver)
     nir)

    外部依赖:
    ┌──────────┐  ┌──────────┐
    │  MMTK    │  │  libuv   │
    │ (via FFI)│  │ (via FFI)│
    └──────────┘  └──────────┘
```

### Crate 职责速查

| Crate | 职责 | 关键输入/输出 |
|-------|------|-------------|
| `nessa` | CLI 入口，参数解析 | 命令行参数 → Driver 调用 |
| `driver` | 编译和执行流程编排 | Source path → 编译/运行/检查 |
| `lexer` | 词法分析 | Source text → TokenStream |
| `parser` | 语法分析 | TokenStream → AST |
| `ast` | AST 节点定义（Arena-based） | 被 parser/resolution/nir 共享 |
| `resolution` | 名称解析 + 类型推断 + Effect 解析 | AST → Resolved AST + SymbolTable |
| `type_pool` | 类型注册与查询、TypeID 计算 | Type definitions → TypeIndex 映射 |
| `nir` | 规范化中间表示 | Resolved AST → NIR（脱糖、标准化） |
| `codegen` | 字节码生成 | NIR → Instruction stream + StackMaps |
| `instruction` | 字节码指令定义与编码 | 指令枚举 + 编码/解码 |
| `nsbc_io` | 字节码归档序列化 | In-memory NSBC ↔ .nsbc 文件 |
| `interpreter` | 字节码解释执行 | Instructions + Runtime → 执行结果 |
| `runtime` | 值表示、调用栈、Effect 栈 | 为 interpreter 提供执行环境 |
| `gc` | MMTK 集成、对象分配与回收 | Allocation requests → managed memory |
| `scheduler` | Work-stealing 调度器、Task 管理 | Task spawn/suspend/resume |
| `str_interner` | 字符串驻留（编译期 + 运行期共享） | String → InternedId |
| `diagnostic` | 编译错误/警告报告 | 诊断信息 → 格式化输出 |
| `pkg_manager` | 包解析、依赖管理 | package.toml → 依赖图 |
| `initialization` | 引擎初始化（类型池预填充、GC 启动等） | Config → 初始化完毕的 Engine |

---

## 八、NSBC Archive 格式

```
┌──────────────────────────────────────────────────────────────────┐
│                    .nsbc Archive File Format                      │
│                                                                  │
│  ╔══════════════════════════════════════════════════════════════╗ │
│  ║ File Header (60 bytes, 固定大小)                             ║ │
│  ║                                                              ║ │
│  ║  magic:          [4 bytes]  "NSBC"                           ║ │
│  ║  version:        [4 bytes]  major.minor.patch                ║ │
│  ║  checksum:       [32 bytes] SHA-256 (header 以外所有数据)     ║ │
│  ║  target_arch:    [2 bytes]  0=Any 1=X86_64 2=ARM64 3=RV64   ║ │
│  ║  target_os:      [2 bytes]  0=Any 1=Linux 2=Darwin 3=Win    ║ │
│  ║  flags:          [4 bytes]  bit0=debug bit1=compress ...     ║ │
│  ║  section_count:  [4 bytes]  节数量                           ║ │
│  ║  section_table:  [8 bytes]  section table 偏移量              ║ │
│  ╚══════════════════════════════════════════════════════════════╝ │
│                                                                  │
│  ╔══════════════════════════════════════════════════════════════╗ │
│  ║ Section Table                                                ║ │
│  ║  每个 Entry 32 bytes:                                        ║ │
│  ║    name_idx:   [4 bytes]  string pool index                  ║ │
│  ║    type:       [4 bytes]  CODE|METADATA|STACKMAP|...         ║ │
│  ║    offset:     [8 bytes]  文件内偏移                          ║ │
│  ║    size:       [8 bytes]  数据大小                            ║ │
│  ║    alignment:  [4 bytes]                                     ║ │
│  ║    flags:      [4 bytes]                                     ║ │
│  ╚══════════════════════════════════════════════════════════════╝ │
│                                                                  │
│  Sections:                                                       │
│    CODE        字节码指令流（按函数组织）                         │
│    METADATA    类型表 + 方法表 + 字段表 + 字符串池                │
│    STACK_MAPS  每函数的 safe-point 位图与 deopt 信息              │
│    CONSTANTS   常量池（数值、字符串字面量等）                     │
│    DEBUG_INFO  源码映射、行号表（可剥离）                         │
│    IMPORTS     外部依赖声明                                      │
│    EXPORTS     导出符号表                                        │
│                                                                  │
│  双相格式:                                                       │
│    .nsbc       → 二进制压缩格式（生产部署）                      │
│    .nsbc.text  → 可读文本格式（调试/审查）                       │
│                                                                  │
│  两层组织:                                                       │
│    Archive → 单个编译单元的产物                                  │
│    Package → 多个 Archive + 元数据（分发单元）                   │
│                                                                  │
└──────────────────────────────────────────────────────────────────┘
```

---

## 九、FFI 架构

```
┌──────────────────────── FFI Bridge ───────────────────────────────┐
│                                                                    │
│  Nessa Code                    C / Native Code                     │
│  ┌──────────────┐              ┌──────────────┐                   │
│  │              │   extern     │              │                   │
│  │  nessa_fn()  │─────────────▶│  c_function() │                   │
│  │              │   marshal    │              │                   │
│  │              │◀─────────────│              │                   │
│  └──────────────┘   unmarshal  └──────────────┘                   │
│                                                                    │
│  数据编组 (Marshaling):                                            │
│    TaggedValue (Immediate数值) → C primitive (int64/double/bool)   │
│    TaggedValue (HeapObject)    → C pointer (opaque + ref count)   │
│    String                      → C char* (UTF-8, null-term copy)  │
│    struct                      → C struct (layout-compatible)     │
│                                                                    │
│  GC 安全:                                                         │
│    - FFI 调用期间 pin 所有传出的堆对象（阻止 GC 移动）              │
│    - C 分配的内存通过 extern object table 注册析构器                │
│    - FFI 返回后 unpin 并恢复正常 GC 追踪                           │
│                                                                    │
│  WASM 互操作:                                                     │
│    - 嵌入 WASM runtime 执行 WASM 模块                          │
│                                                                    │
└────────────────────────────────────────────────────────────────────┘
```

---

## 十、灾难恢复与 VM 异常处理

```
非 effect/error 的底层异常（VM 级别）:

1. Stack Overflow
   → guard page 触发 SIGSEGV
   → 信号处理器识别为 stack overflow
   → 在备用信号栈上执行恢复逻辑
   → 终止当前 task，向父 task 报告 TaskPanic

2. 非法指令 / 致命 GC 错误
   → 捕获信号，记录诊断信息（栈回溯、寄存器状态）
   → 尝试优雅关闭其他 task
   → 输出 crash dump

3. OOM (Out of Memory)
   → GC 回收后仍不足 → 触发 OOM handler
   → 可配置策略: abort / 回收所有 soft ref 后重试 / 通知用户

4. Task Panic 传播
   → child task panic → 通知 parent task
   → parent 可选择 recover 或继续 propagate
   → 到达 root task 则进程终止
```

---

## 十一、初始化流程

```
Engine 启动:
  1. 解析命令行参数（nessa crate）
  2. 初始化 MMTK GC
     - 配置堆大小、GC 策略
     - 分配初始堆区域
  3. 初始化 Task Stack Pool
     - mmap 初始 pool 区域
     - 设置 guard page 信号处理器
  4. 初始化 TypePool
     - 注册内置 intrinsic 类型:
       数值: u8, u16, u32, u64, u128, usize, i8, i16, i32, i64, i128, isize, f32, f64
       其他: bool, char, String, Unit, Any, NoReturn
     - 构建内置类型的 TypeIndex 映射
  5. 初始化 String Interner
     - 预注册关键字和内置标识符
  6. 初始化 Scheduler
     - 启动 N 个 Worker 线程
     - 初始化 work-stealing 队列
  7. 初始化 libuv Event Loop
     - 绑定到主线程
  8. 加载字节码 / 执行编译
     - 开发模式: source → compile → 直接执行
     - 运行模式: 加载 .nsbc archive
  9. 创建 main task，入队 scheduler
  10. 进入主循环（event loop + scheduler 协作）
```

---

## 十二、系统交互流：一次完整的程序执行

```
用户执行: nessa run hello.ns

  ┌─ CLI 解析 ─┐
  │  nessa     │
  └─────┬──────┘
        │
   ┌────▼─────┐      ┌────────┐     ┌────────┐     ┌─────┐
   │  driver   │─────▶│ lexer  │────▶│ parser │────▶│ ast │
   │ (compile) │      └────────┘     └────────┘     └──┬──┘
   └────┬──────┘                                       │
        │                                              ▼
        │         ┌────────────┐     ┌─────┐     ┌────────────┐
        │         │ type_pool  │◀────│ nir │◀────│ resolution │
        │         └────────────┘     └──┬──┘     └────────────┘
        │                              │
        │         ┌────────────┐   ┌───▼────┐
        │         │instruction │◀──│codegen │
        │         └────────────┘   └────────┘
        │
   ┌────▼─────┐
   │  driver   │
   │  (run)    │
   └────┬──────┘
        │
   ┌────▼──────────┐    ┌───────────┐    ┌────────┐
   │initialization │───▶│ scheduler │───▶│main task│
   └───────────────┘    └─────┬─────┘    └────┬───┘
                              │               │
                         ┌────▼────┐     ┌────▼──────┐
                         │ workers │     │interpreter│
                         │(execute)│     │(dispatch) │
                         └─────────┘     └─────┬─────┘
                                               │
                              ┌────────────────┬┴───────────────┐
                              ▼                ▼                ▼
                         ┌─────────┐    ┌──────────┐    ┌───────────┐
                         │ runtime │    │    gc    │    │ scheduler │
                         │ (values)│    │(allocate)│    │(spawn/    │
                         └─────────┘    └──────────┘    │ suspend)  │
                                                        └───────────┘
```

---

## 十三、包管理架构

### 13.1 package.toml 与 Domain 机制

```
每个 nessa 包由 package.toml 定义：

[package]
name = "my_server"
domain = "com.example"           # 域名反转，天然身份标识
version = "0.3.1"
type = "exe"                     # exe | lib
min_nessa_version = "1.0.0"

[dependencies]
"com.example/utils" = "^1.0.0"  # 语义化版本约束
"org.nessa/http" = "~2.1.0"

包唯一性标识：
  对 package.toml 内容（排除版本号）+ 所有依赖的版本约束 ADT
  进行 Merkle Tree 128-bit hash → 包的 TypeID 基础

版本锁定：
  package.lock 记录精确版本解析结果，纳入版本控制
```

### 13.2 包管理器工作流

```
┌────────────────────── Package Manager ──────────────────────────┐
│                                                                  │
│  nessa add "com.example/utils" "^1.0.0"                         │
│                                                                  │
│  1. Resolve: 解析依赖图，处理版本约束                             │
│     - 兼容更新 ^: >=1.0.0, <2.0.0                               │
│     - 近似版本 ~: >=1.0.0, <1.1.0                               │
│     - 精确/范围约束                                              │
│                                                                  │
│  2. Fetch: 从 registry 下载包                                    │
│     - 验证 SHA-256 完整性                                        │
│     - 缓存到全局 ~/.nessa/cache/                                 │
│                                                                  │
│  3. Link: 链接到项目                                             │
│     - 符号可见性检查（pub / 包内 / private）                      │
│     - 更新 package.lock                                          │
│                                                                  │
│  4. Build: 编译依赖图                                            │
│     - 拓扑排序，并行编译无依赖关系的包                            │
│     - 产物缓存（基于内容 hash 判断是否需要重新编译）               │
│                                                                  │
└──────────────────────────────────────────────────────────────────┘
```

---

## 十四、标准库架构 (std)

标准库采用与 Rust std 类似的分层设计：`core` → `alloc` → `std`。

### 14.1 分层结构

```
┌─────────────────────── std (完整标准库) ──────────────────────────┐
│                                                                    │
│  对外统一入口，自动导入 prelude                                     │
│  隐式 `use std.prelude.*`                                          │
│                                                                    │
│  ┌──────────────────────────────────────────────────────────────┐  │
│  │ std.io          文件、标准流、缓冲读写                        │  │
│  │ std.net         TCP/UDP/HTTP（基于 libuv）                   │  │
│  │ std.fs          文件系统操作                                  │  │
│  │ std.os          平台特定接口                                  │  │
│  │ std.process     子进程管理                                    │  │
│  │ std.env         环境变量、命令行参数                           │  │
│  │ std.sync        原子类型、同步原语                            │  │
│  │ std.task        Task 创建与管理 API                           │  │
│  └──────────────────────────────────────────────────────────────┘  │
│                                                                    │
├─────────────────────── alloc (需要分配器) ────────────────────────┤
│                                                                    │
│  ┌──────────────────────────────────────────────────────────────┐  │
│  │ alloc.string    String 实现                                   │  │
│  │ alloc.list      List (动态数组)                               │  │
│  │ alloc.map       Map (哈希表)                                  │  │
│  │ alloc.set       Set                                           │  │
│  │ alloc.boxed     堆分配包装                                    │  │
│  └──────────────────────────────────────────────────────────────┘  │
│                                                                    │
├─────────────────────── core (零依赖) ────────────────────────────┤
│                                                                    │
│  ┌──────────────────────────────────────────────────────────────┐  │
│  │ core.builtin    intrinsic 类型定义（u32, f64, bool, ...）     │  │
│  │ core.ops        运算符 trait (Add, Sub, Mul, Eq, Ord, ...)   │  │
│  │ core.iter       迭代器 trait 与组合子                         │  │
│  │ core.fmt        格式化 trait (Show, Debug)                    │  │
│  │ core.option     ?T 的方法 (map, and_then, unwrap, ...)       │  │
│  │ core.result     !E T 的方法                                   │  │
│  │ core.convert    类型转换 trait (From, Into, As)               │  │
│  │ core.math       数学常量与函数（sin, cos, sqrt...）           │  │
│  └──────────────────────────────────────────────────────────────┘  │
│                                                                    │
└────────────────────────────────────────────────────────────────────┘
```

### 14.2 Intrinsic 注入机制

内建类型并非语言层特殊存在，而是通过 intrinsic 机制在 std 中定义的普通类型：

```nessa
-- core.builtin 中:
pub typealias u32 = .u32'intrinsic
pub typealias f64 = .f64'intrinsic
pub typealias bool = .bool'intrinsic
pub typealias String = .String'intrinsic

-- 通过 impl 注入方法（与用户类型一致）:
impl u32 {
    pub const MAX: u32 = 0xFFFFFFFF
    pub const MIN: u32 = 0
    pub fn to_string(self) -> String { ... }
    pub fn checked_add(self, other: u32) -> ?u32 { ... }
}

-- intrinsic 函数:
pub const sin: fn(f64) -> f64 = .sin'intrinsic
pub const cos: fn(f64) -> f64 = .cos'intrinsic
```

`'intrinsic` view 操作仅在 `std` 包内允许，用户代码无法直接使用。
引擎在初始化 TypePool 时，预注册这些 intrinsic 的 TypeIndex，
std 编译时通过 `'intrinsic` view 将 symbol 映射到引擎内部实现。

### 14.3 Prelude

每个源文件隐式导入 `std.prelude`，包含最常用的类型和函数：

```nessa
-- std.prelude（隐式导入）:
pub use core.builtin.{
    u8, u16, u32, u64, u128, usize,
    i8, i16, i32, i64, i128, isize,
    f32, f64, bool, char, String, Unit, Any, NoReturn,
}
pub use core.ops.{Add, Sub, Mul, Div, Eq, Ord, Show}
pub use core.iter.{Iter, IntoIter}
pub use core.option.*
pub use core.result.*
pub use alloc.list.List
pub use alloc.map.Map
pub use alloc.set.Set
pub use std.io.{print, println}
```