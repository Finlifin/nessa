# Nessa 运行时全面性能剖析、架构重构与六语言全维度基准对比

本文档记录 Nessa 运行时的硬件级性能剖析（`perf stat` / `perf record` / POSIX `getrusage`）、跨 9 类代表性负载与 6 种语言运行时（13 种执行模式）的全维度对比、四阶段运行时深度重构（Waves R1–R4）以及重构前后的可复现实验数据。

复现脚本与原始采样数据位于仓库 [`benchmarks/`](../../benchmarks/) 目录：

- 基准套件驱动：[`benchmarks/run_benchmarks.py`](../../benchmarks/run_benchmarks.py)
- 重构前基线数据：[`benchmarks/results_before.json`](../../benchmarks/results_before.json)
- 重构后实测数据：[`benchmarks/results_after.json`](../../benchmarks/results_after.json)

---

## 1. 测试方法学与评测维度

### 1.1 参测语言与执行模式（6 语言 / 13 模式）

| 模式标识 | 语言 / 运行时版本 | 执行层级说明 |
| --- | --- | --- |
| `nessa_default` | Nessa CLI 默认 (`nessa <file.ns>`) | 源码编译（默认优化级别）+ Tier-1/Tier-2 自适应 JIT |
| `nessa_interp_O0` | Nessa (`NESSA_JIT=off -O0`) | 未优化字节码 + 纯解释器执行 |
| `nessa_interp_O3` | Nessa (`NESSA_JIT=off -O3`) | AOT `-O3` 优化字节码 + 纯解释器执行 |
| `nessa_jit_O3` | Nessa (`-O3` + JIT) | AOT `-O3` 优化 + Tier-1/Tier-2 x86_64 原生 JIT |
| `nessa_nsbc_jit` | Nessa (`nessa build -O3` -> `.nsbc`) | 预编译 `.nsbc` 字节码归档直接加载 + 原生 JIT（剔除前端编译耗时） |
| `nodejs` | Node.js `v26.13.0` (V8) | Sparkplug + Maglev + TurboFan 多层自适应优化 JIT |
| `lua` | PUC-Rio Lua `5.5.1` | 寄存器虚拟机解释器（C 实现） |
| `luajit_interp` | LuaJIT `2.1` (`-joff`) | 手写 x86_64 汇编解释器 |
| `luajit` | LuaJIT `2.1` (Trace JIT) | 汇编解释器 + SSA Trace JIT |
| `elisp_interp` | GNU Emacs `31.1` (`.el`) | AST / 列表求值解释器 |
| `elisp_byte` | GNU Emacs `31.1` (`.elc`) | 栈式字节码虚拟机 |
| `elisp_native` | GNU Emacs `31.1` (`.eln`) | `libgccjit` 原生编译（Native Compilation） |
| `cpython` | CPython `3.14.9` | PEP 659 自适应特化字节码解释器 + C 内建容器 |

### 1.2 9 大测试场景与全维度指标

所有跨语言场景均执行**严格一致的确定性算法与迭代规模**，每次运行自动比对标准输出 `CHECKSUM`；任一运行时校验和不一致即判定测试失败：

1. **`01_fib_rec`**：`fib(34)` 纯递归函数调用与栈帧分配/弹出吞吐（`CHECKSUM: 5702887`）。
2. **`02_int_sieve_lcg`**：`100,000` 规模素数筛（循环 30 轮）+ `5,000,000` 步 64 位线性同余整数位运算（`CHECKSUM: 205320702`）。
3. **`03_float_mandelbrot`**：`600 × 600` 网格、最大 `200` 次迭代的 IEEE-754 双精度浮点密集核（`CHECKSUM: 7030556`）。
4. **`04_struct_binary_trees`**：深度 `14` 二叉树递归堆对象分配、字段解引用与移动式 GC 存活压力（`CHECKSUM: -21826`）。
5. **`05_list_quicksort`**：`50,000` 元素动态数组原地快速排序（10 轮重填与分区交换，`CHECKSUM: 12010927`）。
6. **`06_map_word_freq`**：字符串哈希表高频插入、查找与频次累加（`CHECKSUM: 3550970`）。
7. **`07_string_processing`**：字符串拼接、切片、子串搜索与长度统计（`CHECKSUM: 1244831831`）。
8. **`08_closure_pipeline`**：高阶函数捕获闭包（`make_adder` / `make_linear` / `compose`）与 `2,000,000` 次间接调用（`CHECKSUM: 425345750`）。
9. **`09_nessa_effects`**：Nessa 独有特性负载——代数效应处理器（`effect`）、多发限定延续（multi-shot delimited continuations）与多核异步效应任务树（`async effect` + `Future.await_all`，`CHECKSUM: 265825003`）。

采集的五个正交维度：

- **`kernel_ms`**：进程内纯核计算耗时（排除进程启动与前端编译，3 轮中位数）。
- **`wall_ms`**：POSIX 子进程端到端墙钟时间（含进程启动、前端/加载与退出清理）。
- **`rss_mib`**：通过 `os.wait4` `ru_maxrss` 获取的真实子进程峰值常驻内存（MiB）。
- **`instructions` / `cycles` / `ipc`**：通过 `perf stat` 采集的 CPU 退役指令数、时钟周期与每周期指令吞吐。
- **`branch_misses`**：硬件分支预测失误率。

---

## 2. 重构前根因剖析（Profiling Root Causes）

通过 `perf record -g` 与硬件计数器对重构前基线进行逐场景热点分解，定位出制约 Nessa 性能的五大结构性瓶颈：

1. **标量堆装箱与类型池锁竞争（`f64` / `usize` / `TypeAssert`）**：
   - 原 `TaggedValue` 仅内联 `i57`/`u57`，**所有 `f64`、`usize`、`isize`、`u8..u32`、`i8..i32`、`f32` 均在 MMTk Immix 堆上分配 16B–24B 对象**。在 `03_float_mandelbrot` 中，仅浮点装箱就产生数千万次堆分配并将 RSS 推高至 `118.7 MiB`。
   - `String.len()` / `List.len()` 返回类型别名（如 `usize`）时，`value_is_exact_intrinsic_fast` 未先做 `canonical_type` 归一，退化到获取全局 `ShardedRwLock<TypePool>` 读锁的慢路径。
2. **调用帧 `SavedRegs` 固定数组拷贝与局部槽 `Vec` 反复堆分配**：
   - `perf record` 在 `01_fib_rec` 与 `04_struct_binary_trees` 中捕获到 **`22.17%` 的总 CPU 周期消耗在 `libc::memcpy`**：根源是每个 `CallFrame` 内嵌 168 字节的 `SavedRegs([r19..r28])` 栈数组，每次调用无论被调函数是否触碰被调者保存寄存器都保存并拷贝全部 10 个寄存器，且每次 `AllocateSlots` 都向系统分配器申请并释放 `Vec<TaggedValue>`。
   - `complete_return` 在每次嵌套子函数返回时都误查 `is_at_task_root()`（未先判 `call_stack.is_empty()`）。
3. **内建函数调用包装层、容器条带锁与字符串二次拷贝**：
   - 标准库 `.xxx'builtin` 调用全部经由 `<native:...>` 适配函数多压一层 `CallFrame`；`BuiltinCtx` 每次调用借用多个 `RefCell`；单 worker 场景下 `List`/`Map` 每次读写仍原子争用 `RwLock` 条带；`Map` 查找与字符串拼接/切片先构造临时 Rust `String` 再二次拷入 GC 堆。
4. **NIR 槽位跨类型复用阻碍 Tier-2 寄存器提升**：
   - `opt::dce::compact_local_slots` 将生命周期不重叠的 `bool` 与 `i64`/`f64` 局部槽合并到同一物理槽，导致 Tier-2 循环分析将该槽判为 `SlotKind::Mixed` 而无法提升到物理 GP/XMM 寄存器。
5. **x86_64 JIT 频繁退回解释器与高开销慢路径**：
   - 原 JIT 遇到**任何函数调用**（`Call` / `CallBuiltin` / `CallMethod` / `CallIndirect`）均退回外层 Rust `execute_task` 主循环，每次调用经历完整的原生栈拆卸、上下文重建与 OSR 重入。
   - `Tier-1` 缺少内联旋转 `f64` SSE2 运算、`IMM_I_SMALL` 整除、`IMM_USIZE` 比较、`List` 内联读写及单对象快速分配；`Tier-2` 仅使用 6 个 XMM 寄存器，且退栈桩（`ExitStub`）无条件写回已死亡的循环寄存器槽。

---

## 3. 四阶段运行时重构总览（Waves R1–R4）

### Wave R1：全标量零分配内联 `TaggedValue` 与零锁类型/数值快路径
- **旋转内联 IEEE-754 `f64`**（[`engine/runtime/src/lib.rs`](../../engine/runtime/src/lib.rs)）：利用指针低 3 位全 `0`、原立即数标签低 3 位为 `001` 的编码正交性，将 `f64` 原始位循环左移 4 位后利用指数偏移编码进低 3 位 `0b010..=0b111` 区间（覆盖全部有限正规数、正负零、`±Inf` 与静默 `NaN`；仅极罕见次正规数回退堆盒），实现 **零堆分配 `f64` 表示**。
- **全窄标量与 `usize`/`isize` 内联编码**：新增 `IMM_USIZE (0x59)`、`IMM_ISIZE (0x61)` 与 `IMM_NARROW (0x69)`（精确携带 `Intrinsic` 标签与 `u8..u32`/`i8..i32`/`f32` 位宽语义），在不损失渐进类型精确性的前提下彻底消除标量装箱。
- **Worker 级无锁类型缓存**（[`engine/interpreter/src/types.rs`](../../engine/interpreter/src/types.rs)、[`error_values.rs`](../../engine/interpreter/src/error_values.rs)）：在 `WorkerMetaCache` 中缓存 `CachedTypeOpKind` 与 `has_error_shape`，并对类型别名先归一至 `canonical_type`，消除热路径上的 `TypePool` 读锁。
- **ASCII UTF-8 零扫描快路径**：在 `with_heap_string_str` 中对纯 ASCII 字节块按 8 字节字长快速放行。

### Wave R2：8B 惰性 `SavedRegs`、帧槽对象池、预解码字节码与零拷贝容器/字符串
- **`FunctionExecMeta` 与 8 字节 `SavedRegs`**（[`engine/runtime/src/lib.rs`](../../engine/runtime/src/lib.rs)）：在函数装载时一次性预解码指令流（`decoded`）并静态分析该函数是否写入 `r19..=r28`（`clobbers_callee_saved`）；将 `SavedRegs` 改为 `Option<Box<SavedRegsBuf>>`（大小从 **168B 降至 8B**），叶函数与常规小函数调用完全跳过寄存器保存拷贝。
- **Worker 本地帧槽池 `slot_vec_pool`**（[`engine/interpreter/src/lib.rs`](../../engine/interpreter/src/lib.rs)）：调用返回与取消展开时回收容量 `<= 256` 的 `Vec<TaggedValue>`，使递归与高频调用过程实现零 `malloc/free`。
- **单 Worker 无锁容器快路径与同步原语移交修复**（[`engine/interpreter/src/lib.rs`](../../engine/interpreter/src/lib.rs)、[`sync_primitives.rs`](../../engine/interpreter/src/sync_primitives.rs)）：在未派生辅助 OS worker 线程（`!aux_started`）时跳过 `container_stripes` 读写锁；修复 `RwLock` 唤醒读者批次（`woken_readers`）中首个读者极快解锁时提前唤醒等待写者的竞态条件。
- **零拷贝 `Map` 与 `String` 内建函数**（[`maps.rs`](../../engine/interpreter/src/maps.rs)、[`builtin_fns/string.rs`](../../engine/initialization/src/builtin_fns/string.rs)）：新增 `try_dispatch_fast_builtin` 绕过 `BuiltinCtx` 构造，`MapLayout::find_key` 支持借用 `&str` 查找，`alloc_string_concat` / `alloc_string_subrange` 直接在固定 GC 根保护下一次性分配目标堆字符串并 `memcpy`。

### Wave R3：NIR 内建直降、类型分区槽着色与常量折叠增强
- **类型化 `'builtin` 直接降级**（[`engine/nir/src/expr.rs`](../../engine/nir/src/expr.rs)）：将参数定长的 `.xxx'builtin` 直接降级为带精确参数/返回值 `TypeAssert` 的 `NirExpr::CallBuiltin`，消除 `<native:...>` 包装函数调用；内联器（[`opt/inline.rs`](../../engine/nir/src/opt/inline.rs)）在实参静态类型已满足形参约束时自动消去冗余 `TypeAssert`。
- **类型分区槽着色**（[`engine/nir/src/opt/dce.rs`](../../engine/nir/src/opt/dce.rs)）：`compact_local_slots` 按规范化类型分区复用局部槽，杜绝 `bool` 与 `i64`/`f64` 混用同一槽位，确保 Tier-2 循环寄存器分配火力全开。
- **前置死函数消除与内建常量折叠**（[`opt/mod.rs`](../../engine/nir/src/opt/mod.rs)、[`opt/const_fold.rs`](../../engine/nir/src/opt/const_fold.rs)）：在 `O1..O3` 管线入口先行裁剪未引用的标准库函数，补齐字符串长度内建与自反标量比较的编译期折叠。

### Wave R4：x86_64 Tier-1 / Tier-2 原生 JIT 全面扩展
- **原生栈内直接调用链**（[`engine/interpreter/src/jit_bridge.rs`](../../engine/interpreter/src/jit_bridge.rs)、[`engine/jit/src/tier1.rs`](../../engine/jit/src/tier1.rs)）：新增零拷贝 `helper_call_direct_fast`、`helper_call_op` 与 `helper_call_builtin`，在受控原生递归深度（`MAX_NATIVE_JIT_CALL_DEPTH = 64`）内直接在 JIT 代码间完成 `CallFrame` 推入、原生子调用与单次借用快速弹栈，不再退回解释器主循环。
- **Tier-1 全方位内联机器码生成**：
  - 旋转内联 `f64` 的纯寄存器 SSE2 `addsd`/`subsd`/`mulsd`/`divsd`/`xorpd`/`ucomisd` 快路径（含 `NaN` 与次正规数完备慢路径回退）；
  - 内联 `IMM_I_SMALL` 有符号 `idiv`（含除零与溢出守卫）及 `IMM_USIZE`/`IMM_U_SMALL` 无符号比较；
  - 基于静态必达槽数的 `LoadSlot`/`StoreSlot` 边界检查消除与 `AllocateSlots` 零操作快跳过；
  - 扩展 `TypeAssert`/`TypeCast` 内联快路径、`LoadField`/`StoreField` 单态内联缓存（覆盖可选结构体、内联标量与精确堆名义类型）、`helper_alloc_object_fast` 以及单 worker 下内联 `LIST_LEN`/`LIST_GET`/`LIST_SET`。
- **Tier-2 循环优化器扩容**（[`engine/jit/src/tier2.rs`](../../engine/jit/src/tier2.rs)）：修复旋转内联 `f64` 常量的识别，将循环 XMM 物理寄存器池从 **6 个扩展至 14 个**（`Xmm0..Xmm5, Xmm8..Xmm15`），并在 `ExitStub` 处基于向上暴露活跃槽掩码（upward-exposed liveness）裁剪死槽写回。

---

## 4. 重构前后 Nessa 自身加速比对比

以下数据取自 [`results_before.json`](../../benchmarks/results_before.json) 与 [`results_after.json`](../../benchmarks/results_after.json)（Release 构建，3 轮中位数 + `perf stat`）：

### 4.1 核心模式加速比与指令数缩减

| 场景 | 重构前默认 `kernel_ms` | 重构后默认 `kernel_ms` (加速比) | 重构前 `.nsbc`+JIT `kernel_ms` | 重构后最佳 JIT `kernel_ms` (**总加速比 vs 原默认**) | `.nsbc` 退役指令数缩减 (`Before -> After`) | `.nsbc` 峰值 RSS (`Before -> After`) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `01_fib_rec` | 4,986.62 | 566.18 (**8.81x**) | 3,794.55 | **522.56** (**9.54x**) | 78,065.8M -> 11,788.0M (**6.62x**) | 23.5 -> **23.0 MiB** |
| `02_int_sieve_lcg` | 10,043.13 | 300.52 (**33.42x**) | 5,103.57 | **115.56** (**86.91x**) | 96,783.6M -> 3,872.0M (**25.00x**) | 100.8 -> **69.1 MiB** |
| `03_float_mandelbrot` | 369.92 | 184.22 (**2.01x**) | 5,978.07\* | **87.33** (**67.16x** vs 原 `-O3`) | 105,581.2M -> 2,406.3M (**43.88x**) | 92.1 -> **22.7 MiB** |
| `04_struct_binary_trees` | 4,145.83 | 330.22 (**12.55x**) | 3,932.71 | **311.14** (**13.32x**) | 73,288.5M -> 7,740.1M (**9.47x**) | 91.2 -> **91.2 MiB** |
| `05_list_quicksort` | 9,165.59 | 205.20 (**44.67x**) | 5,727.56 | **35.86** (**255.62x**) | 104,924.5M -> 1,317.7M (**79.62x**) | 97.2 -> **33.0 MiB** |
| `06_map_word_freq` | 775.16 | 66.99 (**11.57x**) | 501.38 | **39.16** (**19.80x**) | 9,539.1M -> 1,192.0M (**8.00x**) | 35.2 -> **29.2 MiB** |
| `07_string_processing` | 240.34 | 50.61 (**4.75x**) | 156.82 | **28.64** (**8.39x**) | 3,332.2M -> 960.7M (**3.47x**) | 39.1 -> **39.1 MiB** |
| `08_closure_pipeline` | 1,828.87 | 215.67 (**8.48x**) | 1,200.72 | **170.85** (**10.70x**) | 22,692.7M -> 4,251.4M (**5.34x**) | 95.7 -> **38.9 MiB** |
| `09_nessa_effects` | 121.52 | 88.63 (**1.37x**) | 105.13 | **82.16** (**1.48x**) | 1,172.6M -> 908.9M (**1.29x**) | 90.1 -> **81.2 MiB** |

> \*注：重构前 `03_float_mandelbrot` 在 `-O3` 下因局部槽合并将 `bool` 与 `f64` 混入同一槽位，触发 Tier-2 回退至慢速路径（`5,865.11 ms`）；Wave R1+R3+R4 修复后，`-O3` + JIT 进一步比默认优化快 `2.11x`（达 **`87.33 ms`**），`.nsbc` 峰值内存从 `92.1 MiB` 骤降至 **`22.7 MiB`**（下降 **75.4%**）。

### 4.2 各执行层级几何平均提升（Geometric Mean）

| Nessa 执行模式 | 8 个跨语言场景几何平均加速比 | 全部 9 个场景几何平均加速比 | 全部 9 个场景几何平均指令缩减 |
| --- | ---: | ---: | ---: |
| `nessa_interp_O0`（纯解释器 `-O0`） | **2.00x** | **1.90x** | **1.61x** |
| `nessa_interp_O3`（纯解释器 `-O3`） | **2.54x** | **2.35x** | **1.79x** |
| `nessa_default`（CLI 默认） | **10.56x** | **8.42x** | **3.98x** |
| `nessa_nsbc_jit`（预编译 `.nsbc` + JIT） | **18.85x** | **13.98x** | **10.05x** |
| `nessa_jit_O3`（`-O3` + JIT） | **19.60x** | **14.38x** | **4.14x** |

---

## 5. 六语言全维度横向对比（重构后实测）

### 5.1 纯核计算时间 `kernel_ms` 对比（越低越快，单位：ms）

| 场景 | Nessa (最佳 JIT) | Nessa (默认) | Node.js (V8) | Lua 5.5 | LuaJIT (`-joff`) | LuaJIT (JIT) | Elisp (`.el`) | Elisp (`.elc`) | Elisp (`.eln`) | CPython 3.14 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `01_fib_rec` | 522.56 | 566.18 | 29.16 | 178.18 | 119.53 | **21.19** | 3,151.37 | 191.46 | 199.46 | 301.17 |
| `02_int_sieve_lcg` | 115.56 | 300.52 | 41.80 | 56.13 | 60.45 | **18.60** | 612.15 | 95.25 | 98.57 | 247.47 |
| `03_float_mandelbrot` | **87.33** | 184.22 | **11.77** | 92.82 | 70.19 | 12.01 | 4,987.41 | 1,915.09 | 1,776.31 | 484.93 |
| `04_struct_binary_trees` | 311.14 | 330.22 | **20.77** | 201.83 | 154.46 | 120.28 | 1,223.20 | 215.15 | 180.43 | 294.42 |
| `05_list_quicksort` | **35.86** | 205.20 | **17.23** | 38.40 | 47.04 | 19.35 | 439.51 | 46.23 | 32.95 | 88.76 |
| `06_map_word_freq` | 39.16 | 66.99 | 2.37 | 2.24 | 1.29 | **0.33** | 58.03 | 5.37 | 2.83 | 5.89 |
| `07_string_processing` | 28.64 | 50.61 | 8.29 | 7.30 | 8.93 | **5.14** | 54.45 | 27.02 | 26.43 | 7.53 |
| `08_closure_pipeline` | 170.85 | 215.67 | 7.03 | 10.45 | 9.30 | **3.00** | 212.35 | 31.21 | 27.13 | 23.24 |
| **8 场景几何平均** | **102.12** | **187.88** | **12.77** | **32.45** | **28.14** | **9.30** | **492.66** | **79.36** | **67.30** | **74.73** |

### 5.2 端到端墙钟时间 `wall_ms` 与峰值内存 `rss_mib`（8 场景几何平均）

| 运行时模式 | 几何平均 `kernel_ms` | 几何平均 `wall_ms`（含启动/编译） | 几何平均峰值 RSS (`MiB`) | 几何平均退役指令数 (`M`) |
| --- | ---: | ---: | ---: | ---: |
| `luajit` | 9.30 | 11.07 | 19.47 | 125.9 |
| `nodejs` | 12.77 | 30.62 | 58.94 | 357.5 |
| `luajit_interp` | 28.14 | 30.45 | 19.40 | 502.2 |
| `lua` | 32.45 | 34.42 | 19.12 | 870.2 |
| `elisp_native` (`.eln`) | 67.30 | 128.94 | 59.32 | 1,973.1 |
| `cpython` | 74.73 | 95.47 | 18.39 | 2,172.5 |
| `elisp_byte` (`.elc`) | 79.36 | 139.48 | 59.47 | 2,105.6 |
| **`nessa_jit_O3`** | **102.12** | 596.54（含全套静态/效应/AOT `-O3` 编译） | 63.71 | 11,477.8 |
| **`nessa_nsbc_jit`** | **105.57** | **130.76**（预编译归档秒级启动） | **38.49** | **2,931.5** |
| `nessa_default` | 187.88 | 545.02 | 60.91 | 10,304.0 |
| `elisp_interp` (`.el`) | 492.66 | 572.35 | 59.95 | 11,851.9 |

### 5.3 核心对标结论

1. **数值、浮点与数组算法全面反超 CPython 与多款解释器/字节码引擎**：
   - 在 `03_float_mandelbrot` 上，Nessa (`87.33 ms`) **快于 Emacs Lisp `.eln` 20.3 倍、快于 CPython 5.55 倍、并反超 C 实现的 Lua 5.5 (`92.82 ms`)**。
   - 在 `05_list_quicksort` 上，Nessa (`35.86 ms`) **快于 CPython 2.48 倍、快于 LuaJIT 解释器 1.31 倍、快于 Emacs Lisp `.elc` 1.29 倍、并快于 Lua 5.5 (`38.40 ms`)**。
   - 在 `02_int_sieve_lcg` 上，Nessa (`115.56 ms`) **快于 CPython 2.14 倍**。
   - 在 `04_struct_binary_trees` 与 `07_string_processing` 上，Nessa (`311.1 ms` / `28.6 ms`) 已追平 CPython (`294.4 ms`) 与 Emacs Lisp `.eln` (`26.4 ms`) 量级。
2. **内存与启动开销表现优异**：
   - 预编译 `.nsbc` 模式下几何平均峰值 RSS 仅为 **`38.49 MiB`**（内置 64 MiB 虚拟地址池的 MMTk Immix 移动式分代/区域堆），低于 Node.js (`58.94 MiB`) 和 Emacs Lisp (`59.32 MiB`)；端到端 `wall_ms` (`130.76 ms`) 已快于 Emacs Lisp `.elc` (`139.48 ms`)。
3. **语义完备性零妥协**：
   - 上述全部提速均在 **100% 保留** Nessa 渐进类型精确断言、整数算术溢出检测、MMTk Immix 移动式 GC 精确根追踪、多发限定延续（delimited continuations）与多核任务树取消/结构化并发的前提下取得。

---

## 6. 剩余性能差距的客观归因与后续演进方向

与工业级顶配多层/追踪编译器（LuaJIT `9.30 ms`、Node.js V8 `12.77 ms`）以及字符串驻留哈希表高度特化的运行时相比，Nessa 当前剩余差距具有清晰的架构边界：

1. **字符串哈希与驻留缓存（`06_map_word_freq`：`39.16 ms` vs CPython `5.89 ms` / Lua `2.24 ms`）**：
   - Lua 与 CPython 在字符串对象头缓存哈希值（Lua 短字符串全局驻留，哈希比较退化为单指令指针相等），而当前 Nessa `HeapString` 每次 `Map` 查找仍对字节内容计算 SipHash/FxHash 并逐字节 `memcmp`。在 `HeapString` 对象头或前缀字缓存 64 位预计算哈希可将该场景再提速 `4x–8x`。
2. **间接闭包调用内联缓存与逃逸分析（`08_closure_pipeline`：`170.85 ms` vs Lua `10.45 ms` / V8 `7.03 ms`）**：
   - `CallIndirect` 目前仍经由 `helper_call_op` 分派并校验动态函数契约；为 `CallIndirect` 引入基于 `(func_id, abi_epoch)` 的单态/多态调用点内联缓存（Call-Site Polymorphic IC）与 JIT 帧内尾调用/小闭包展开，可消除 C 辅助桥接开销。
3. **原生栈帧无桥接调用（`01_fib_rec`：`522.56 ms` vs Lua `178.18 ms` / V8 `29.16 ms`）**：
   - `helper_call_direct_fast` 虽已消除退回解释器的开销，但每次递归仍在堆上压入 `runtime::CallFrame` 以支持随时可能发生的限定延续捕获与取消展开。后续可采用“原生机器栈帧 + Safepoint/Effect 按需栈重构（Deopt/Lazy Frame Materialization）”将纯函数递归压缩至接近 C 调用开销。
4. **默认 CLI 编译级别与增量缓存**：
   - `nessa <file.ns>` 默认优化级别尚未开启 `-O3` 全量跨函数内联与类型特化（因此 `nessa_jit_O3` 比 `nessa_default` 几何平均再快 `1.84x`）。后续可在 CLI 运行入口针对热模块启用轻量级 `-O2` 默认管线以兼顾编译延迟与核内吞吐。
