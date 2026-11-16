# Nessa 分层热点 JIT 与循环 OSR 架构设计 (`engine/jit`)

本文档记录 Nessa 虚拟机已实现的 **Tiered Hot-Loop & Function JIT with On-Stack Replacement (OSR)** 子系统架构、寄存器与标签位级不变量、Immix 移动式 GC 协同协议、去优化（Deoptimization）路径与 CLI / 标准库内省接口。

---

## 1. 模块职责与依赖拓扑

JIT 编译器位于独立 crate `engine/jit`，通过窄桥接模块 `engine/interpreter/src/jit_bridge.rs` 嵌入解释器调度循环，不引入任何重量级外部编译器后端依赖：

| 路径 | 职责 |
| --- | --- |
| `engine/jit/src/code_cache.rs` | POSIX `mmap` / `mprotect` **严格 W^X** 可执行内存管理与每 `Vm` 默认 `16 MiB` 容量预算控制 |
| `engine/jit/src/x86_64.rs` | 零外部依赖 `x86_64`（SysV64 ABI）原生机器码汇编器（64-bit GP、SSE2 标量浮点 `XMM`、双遍标签回填） |
| `engine/jit/src/arch.rs` | 架构抽象分派（`x86_64` Unix 原生后端 + `aarch64`/非原生平台平滑回退） |
| `engine/jit/src/tier1.rs` | **Tier-1 基线全函数 JIT**：消除取指分派、内联 57-bit `IMM_I_SMALL` 算术与比较快路径、单态 `LoadField`/`StoreField` 内联缓存（IC）、每 PC 原生偏移表（支持即时循环 OSR 与调用返回重入） |
| `engine/jit/src/tier2.rs` | **Tier-2 自然循环特化 JIT**：非 SSA 局部活跃性与定值分析、循环 Preheader / OSR 标签守卫、GP (`i64`) 与 SSE2 `XMM`（混合 `IMM_F64` + 堆装箱 `F64`）寄存器拆箱、零分配热循环体、Box-on-Exit 与算术溢出去优化 |
| `engine/jit/src/lib.rs` | 位级布局编译期断言、`JitEngine` 状态机、热度计数、IC 元数据与 `JitRuntimeCtx` / `JitHelpers` C ABI 定义 |
| `engine/interpreter/src/jit_bridge.rs` | `Vm` 与 `JitEngine` 桥接：常量分类、运行时慢路径 Helper、Safepoint 轮询、单态 IC 填充与方法作用域恢复 |
| `library/std/runtime.ns` | 只读诊断内省函数：`jit_enabled`、`jit_compiled_functions`、`jit_code_bytes`、`jit_deopt_count` |

---

## 2. 外置 `StackContext` Trampoline ABI 与 Continuation / GC 安全

Nessa 的代数效应（Algebraic Effects）与多提示符定界延续（Multi-shot Delimited Continuations）依赖独立的 `StackPool` 栈段（`StackContext`），捕获与恢复仅重接栈段链表而不走宿主 C 调用栈。为保证 `Continuation` 捕获、`clone` 与恢复语义 **100% 零改动兼容**，JIT 严守以下外置帧契约：

### 2.1 入口签名

```rust
pub type JitEntryFn = unsafe extern "C" fn(
    rt_ctx: *mut JitRuntimeCtx,
    regs_ptr: *mut u64,
    entry_native_offset: usize,
    out_exit: *mut JitExit,
);
```

- **不在宿主 C 栈保存 Nessa 调用帧**：JIT 入口仅在单次函数执行切片内借用当前激活栈段的 `regs_ptr`（`ctx.registers.regs.as_mut_ptr()`）与 `slots_ptr`（`ctx.local_slots` 当前帧窗口），并通过 `entry_native_offset` 直接跳入函数头（`pc = 0`）或任意循环回边目标（OSR Prologue / 指令原生偏移）。
- **控制流交接（`JitExit`）**：
  - `Return(raw_val)`：交由 `Vm::execute_task` 调用 `complete_return`，统一处理根任务子 `Future` 等待、栈段边界弹出与父帧恢复（随后若调用方亦已 JIT 编译，外层循环在下一拍立即切入调用方原生代码）。
  - `InterpretAt(pc)` / `InterpretOne(pc)`：遇到普通/间接/方法调用、`CallBuiltin`、`EffectCallDyn`、`Push*Handler`、`ResetClosure`、`ResumeContinuation*`、`Await*` 时，JIT 先将全部脏寄存器写回 `StackContext`，再精确退回 `Vm::execute_task` 在 `pc` 处执行，确保 `display_state`、栈段切分与 `Continuation` 克隆不感知任何宿主栈帧。
  - `TierUp(pc)`：Tier-1 循环回边热度达到 Tier-2 阈值（`65,536`）时退出触发即时重编译，并在同一 `pc` 切入 Tier-2 OSR 入口。

### 2.2 精确移动式 Immix GC 根约束

MMTk Immix 在分配慢路径或显式 `Safepoint` 时可能触发 Stop-The-World 移动式回收，搬迁堆对象并原地更新 `StackContext.registers` 与 `StackContext.local_slots` 中的指针（甚至因 `local_slots` 扩容改变 `slots_ptr` 基址）。因此：

1. **调用前全量刷回（Flush before Call）**：在调用任何可能分配、触发类型强制转换或轮询 Safepoint 的 Helper（`alloc_slots`、`safepoint`、`numeric_slow`、`cmp_slow`、`type_op`、`load_field_ic`、`store_field_ic`、`store_boxed_f64`、`step_inline_op`）之前，生成代码必须将所有已修改的物理寄存器装箱写回 `ctx.registers` / `ctx.local_slots`。
2. **调用后重取基址与重载（Reload after Call）**：Helper 返回前调用 `Vm::refresh_runtime_ctx` 刷新 `JitRuntimeCtx.{regs_ptr, slots_ptr, slice_safepoints, has_other_ready}`；原生代码从 `JitRuntimeCtx` 重新加载 `regs_ptr`（及 `slots_ptr`）并重新读取存活值，彻底杜绝跨 Safepoint 悬垂堆指针。

---

## 3. TaggedValue 位级快路径与内联缓存不变量

`engine/jit/src/lib.rs` 通过编译期 `const` 断言与调试断言严格锁定 `TaggedValue` 与 `ObjectHeader` 布局：

- **`IMM_I_SMALL`（57-bit 有符号小整数，低 7 位 `0x01`）**：
  - 双操作数同为 `IMM_I_SMALL` 守卫：`(((a ^ 0x01) | (b ^ 0x01)) & 0x7f) == 0`。
  - **加法 `Add`**：`tmp = a - 1; res = tmp + b`；`x86_64` 有符号溢出标志 `JO` 当且仅当 57-bit 有符号和溢出 `[-2^56, 2^56 - 1]` 时置位！未溢出直接得合法 `TaggedValue`；`JO` 跳入慢路径平滑提升至 1-word 堆装箱 `I64` 或 2-word `I128`（绝不静默截断回绕）。
  - **减法 `Sub`**：`tmp = a - b`；`JO` 精确检测 57-bit 差溢出；未溢出时 `res = tmp | 0x01`。
  - **乘法 `Mul`**：`va = (a as i64) >> 7; vb_sh = (b - 1) as i64; imul va, vb_sh`；`JO` 精确检测 57-bit 积溢出；未溢出时 `res = prod | 0x01`。
  - **比较 `CmpLt/Le/Gt/Ge/Eq/Ne`**：双 `IMM_I_SMALL` 时直接执行 64-bit 有符号 `cmp a, b` 产出 `TRUE (0x99)` / `FALSE (0x19)`；非双小整数的 `CmpEq/Ne` 必须调用 `cmp_slow` 以保持 `-0.0 == 0.0`、`0i64 == 0u64`、`NaN != NaN` 与 `String` 按值相等语义。
- **混合 `f64` 表示（Hybrid Float）**：
  - Nessa 浮点数有两种运行时形态：低 7 尾数位全零时压缩为立即数 `IMM_F64`（`(bits & !0x7f) | 0x11`，如 `0.0`、`1.5`、`2.0`）；否则分配为 1-word 堆 `Ordinary` 对象（`header.type_index == Intrinsic::F64 (13)`，如 `0.1`、`0.3`）。
  - Tier-1 `TypeAssert(F64)` 与 Tier-2 循环拆箱守卫同时识别这两种表示：立即数掩去低 7 位装入 `XMM`，堆对象验证非空堆指针、`type_index == 13` 且 `(gc_meta & 0x003F_FFF8) == (1 << 3)` 后直接 `movsd xmm, [ptr + 0]`。
- **单态字段内联缓存（Monomorphic `LoadField` / `StoreField` IC）**：
  - `NewObject` 将未初始化字段填为 `TaggedValue::UNIT (0x31)`。为防止读取非 `Unit` 类型的未初始化字段绕过类型检查，`LoadField` IC 命中（`is_heap && header.type_index == cached_ty && Ordinary role & payload_words 匹配`）并读出 `val = *(ptr + idx * 8)` 后，额外检查 `val != 0x31`；若等于 `0x31` 则走慢路径交由 `Vm` 校验字段静态类型是否允许 `Unit`。
  - `StoreField` IC 仅在写入值无需隐式数值拓宽或 `Newtype` 包装（`Any`、`I64 + IMM_I_SMALL`、`Bool + BOOL_TAG`）时执行单指令原地写入，其余情况走慢路径保持完整强制转换语义。
  - 所有依赖编译期调用点作用域的指令慢路径均携带 `(func_id, pc)`，恢复 `method_call_scopes` 查询上下文。

---

## 4. 热度剖析、即时回边 OSR 与 Tier-2 循环拆箱

### 4.1 热度模型与即时 OSR

- 函数入口（`pc == 0`）：`hotness += 16`
- 后向跳转回边（`offset <= 0`，`target_pc <= instr_pc`）：`hotness += 1`
- **Tier-1 阈值（`4,096`）**：即使 `main` 或批处理函数在整个进程生命周期内**只被调用 1 次**，只要其内部循环迭代约 4,000 次，解释器在后向跳转处立即编译该函数，查 `pc_native_offsets[target_pc]` 在**当次循环迭代原地切入原生机器码（OSR）**。
- **Tier-2 阈值（`65,536`）**：Tier-1 回边继续累加函数热度；跨越 `65,536` 时返回 `JitExit::TierUp(target_pc)`，升级编译 Tier-2 并在 `target_pc` 跳入该自然循环的专属 OSR Unbox Prologue。

### 4.2 Tier-2 自然循环分析与零分配浮点/整数热循环

1. **自然循环识别与活跃性分析**：扫描回边 `latch_pc -> header_pc`，要求单连通无内嵌帧逃逸/调用且块内仅操作纯标量算术、比较、分支与局部槽读写；通过向上暴露使用（Upward-Exposed Use）分析精确区分在循环头必须读取旧值的 `live_in` 变量与在先读前必被覆盖的纯临时寄存器。
2. **类型推导与物理寄存器分配**：结合 `TypeAssert`、常量池与算术指令推导循环携带标量类型（`I64`、`F64`、`Bool`），将高频 `I64`/`Bool` 分配至 `LOOP_GP_POOL`（`rbx, r8..r11, rsi, rdi`），将 `F64` 分配至 `LOOP_XMM_POOL`（`xmm0..xmm5`）。
3. **无装箱热循环体与精确去优化（Deoptimization）**：
   - 循环体内 `i64` 直接运行 64-bit `add/sub/imul/cmp`，移去每次迭代的 7-bit 移位与标签拼装；每次写回目标前检查结果是否仍在 57-bit 有符号整数区间 `[-2^56, 2^56 - 1]`，一旦越界立即跳入 `DeoptStub`。
   - 循环体内 `f64` 直接运行原生 SSE2 `addsd/subsd/mulsd/divsd/ucomisd`（`divsd` 前检查除数 `±0.0` 以精确保持 Nessa 的 `DivisionByZero` 异常语义；`CmpEq/Ne` 检查 `JP` 奇偶标志精确处理 `NaN`），**循环迭代期间堆分配次数严格为 0**。
   - **Box-on-Exit / Safepoint / Deopt**：
     - 正常循环出口（`ExitStub`）将修改过的物理寄存器一次性编码写回 `StackContext`（`f64` 先尝试零分配 `IMM_F64` 压缩，需要堆装箱时调用根安全的 `store_boxed_f64`）。
     - 算术溢出或浮点除零触发 `DeoptStub`：将指令前状态完整装箱写回 `StackContext`，递增全局与单函数 `deopt_count`（单函数累计超过 `MAX_DEOPTS_PER_FUNCTION = 8` 次后固定回退至 Tier-1，避免反复去优化抖动），返回 `InterpretAt(pc)` 由解释器无缝执行 64/128-bit 大整数提升或错误抛出。

---

## 5. CLI 控制、环境变量与 `std.runtime` 内省接口

### 5.1 命令行与环境变量

`nessa run` 支持显式选择 JIT 模式（优先级高于环境变量 `NESSA_JIT`，未指定时默认为 `auto`）：

```sh
nessa run app.ns                  # 默认 auto（4096 -> Tier-1 + OSR，65536 -> Tier-2 + OSR）
nessa run --jit=off app.ns        # 纯字节码解释执行
nessa run --jit=tier1 app.ns      # 首次进入函数即编译 Tier-1
nessa run --jit=tier2 app.ns      # 首次进入函数即编译 Tier-2（无自然数值循环时自动回退 Tier-1）
NESSA_JIT=off nessa run app.ns    # 通过环境变量控制（便于全量差分测试）
```

### 5.2 `std.runtime` 诊断 API

`library/std/runtime.ns` 提供四个零副作用只读查询函数（对应内建 ID `688..=691`）：

```nessa
use std.runtime

let active: bool = runtime.jit_enabled()
let fns: usize = runtime.jit_compiled_functions()
let bytes: usize = runtime.jit_code_bytes()
let deopts: usize = runtime.jit_deopt_count()
```
