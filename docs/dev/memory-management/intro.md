# 内存管理 (Memory Management)

Nessa 在 Unix（Linux / macOS）上采用 MMTk 0.32 `Immix` 移动式回收器 + 大对象空间（LOS），在 Windows（Windows 10 1709+）上采用原生精确 Stop-The-World Mark-Sweep + LOS 回收器（`win_gc.rs`），并结合分段式 `Task Stack Pool` 管理任务与定界延续栈段。

> **说明**：早期设计文档曾规划接入实验性的 `LXR` 并发回收器与每 Task 独立分配器；**当前实际运行实现**为**每 OS Worker 线程绑定独立 Mutator**（Unix 下为 `mmtk::Mutator<NessaVM>` bump-pointer 分配器，同一 Worker 上顺序切换的多个 Task 共享该线程的 Mutator 缓冲区，无需跨线程加锁；LXR 与并发标记仍属远期演进方向）。

## 设计概览

```
┌────────────────── Memory Subsystem ──────────────────────────────┐
│                                                                   │
│  ┌─────────────────────────────────────────────────────────────┐ │
│  │ Managed Heap Backend                                         │ │
│  │                                                              │ │
│  │  ┌──────────────┐  ┌──────────────┐  ┌───────────────────┐ │ │
│  │  │  Allocators  │  │  Collector   │  │   STW & Packets   │ │ │
│  │  │ (per-worker  │  │ (Unix Immix  │  │ (多 Worker 安全点  │ │ │
│  │  │  bump/freels)│  │ / Win MarkSwp│  │   协同与根集扫描)  │ │ │
│  │  └──────────────┘  └──────────────┘  └───────────────────┘ │ │
│  └─────────────────────────────────────────────────────────────┘ │
│                                                                   │
│  ┌─────────────────────────────────────────────────────────────┐ │
│  │ Task Stack Pool (Segmented List)                             │ │
│  │   mmap / VirtualAlloc 8MB slots, guard pages, demand-paged   │ │
│  └─────────────────────────────────────────────────────────────┘ │
│                                                                   │
│  ┌─────────────────────────────────────────────────────────────┐ │
│  │ Extern Object Table                                          │ │
│  │   FFI 分配的外部对象注册与析构（规划扩展）                     │ │
│  └─────────────────────────────────────────────────────────────┘ │
│                                                                   │
└──────────────────────────────────────────────────────────────────┘
```

## 分配路径

### 快速路径 (Hot Path)

在 Unix MMTk `Immix` 后端下，每个 OS Worker 线程独占持有一个 `Mutator<NessaVM>` bump allocator（Task 只在安全点切换，不在分配中途跨线程迁移）：

```
1. Worker 线程向 MMTk 请求本地分配块 (Immix line / block)
2. 该 Worker 上运行的任意 task 分配小对象时仅推进当前线程的 bump pointer
3. 本地块耗尽时向 MMTk 全局池申请新块；空间不足时触发协同 STW 回收
4. 分配快路径为线程本地操作（不跨线程争用锁，但并非每个 Task 单独持有 TLAB）
```

### 大对象分配

超过阈值的大对象（> 8KB）直接进入大对象空间（Large Object Space, LOS），避免普通块碎片化。

详见 [large-object-management.md](large-object-management.md)。

## GC 策略：Immix（Unix）与精确 Mark-Sweep（Windows）

- **Unix (`Immix` + `LOS`)**：按块/行（block/line）管理内存，支持常规标记复用空闲行及碎片整理（defrag 移动式压缩，自动更新所有已注册 `TaggedValue` 根槽位），并由 MMTk Work Packet 多线程并行执行回收。
- **Windows (`win_gc` Mark-Sweep + `LOS`)**：面向 Windows 10 1709+ 提供零外部 C++ 运行时依赖的精确根集标记-清扫回收器，支持多 Worker STW 屏障与按类大小空闲链表复用。
- **远期演进方向（未启用）**：并发标记或 `LXR` 低延迟回收器留待未来按需评估。

### GC 触发条件

```
1. Allocation failure: bump buffer 用尽且 MMTK 堆空间不足
2. Proactive GC: 堆使用率超过阈值（如 75%）时主动触发
3. Explicit request: 运行时代码显式请求 GC（通常在 idle 时）
```

### Safe-Point 与 STW

```
GC 需要 Stop-The-World 阶段（标记根集）:

1. GC 设置全局 gc_requested 标志
2. 每个 task 在 safe-point 处检查标志:
   - 函数调用/返回点
   - 循环回边 (back-edge)
3. task 到达 safe-point 后暂停，报告自己的 stack map
4. 所有 task 暂停后，GC 开始并发标记
5. 标记完成 → 回收/整理 → 恢复所有 task
```

### GC Root 枚举

```
Root 来源:
  1. 每个 task 所拥有的全部栈段的 register file (32 个 TaggedValue)
  2. 每个栈段的 call stack，包括已捕获但尚未恢复的 continuation
  3. 全局变量表 (global declarations)
  4. effect handler 证据链中持有的 handler 对象
  5. FFI pin table 中的已 pin 对象
```

## Task Stack Pool

详见 [architecture.md](../architecture.md) 5.3 节。

slot 的分配单位是栈段。一个 task 的根栈、每个可捕获 delimiter 的栈和
显式克隆的 continuation 分支分别拥有独立 slot。捕获与恢复仅修改栈段链接，
不复制帧；普通 in-place handler 不因效应调用额外分配栈。

每个 slot 由运行时所有权对象持有，delimiter 完成、continuation 丢弃、task
取消或销毁时归还。GC 必须扫描活动、父调用者及暂停的 continuation 栈段，
包括寄存器、保存的寄存器和闭包环境。详见
[continuation ABI](../continuation-stack-abi.md)。

核心结构：

```
StackPool {
    segments: Vec<Segment>,   // 分段列表
    free_list: Vec<SlotId>,   // 空闲 slot 队列
}

Segment {
    base: *mut u8,            // mmap 基地址
    slot_count: usize,        // 该段的 slot 数量
}

增长规则:
  初始 segment: 64 slots (64 × 8MB = 512MB 虚拟地址空间)
  后续 segment: 容量翻倍 (128 → 256 → ...)
  新 segment 独立 mmap，不要求与前一段虚拟地址连续
```

## 当前解释器实现与验证范围

当前 MMTk 0.32 默认使用 Immix；上述 LXR、并发收集、外部对象及 per-task
分配器仍是目标，不能视作已实现。heap 配置通过 `gc_trigger=FixedHeapSize`
生效，进程首次初始化决定 plan 和容量。常规对象头保留 `u16` payload words；超出 `u16::MAX` 槽位的含引用容器/对象自动切换为 `PayloadRole::LargeTagged`（前置 8B `u64` 字数 + MMTk LOS，上限 `67,108,864` words），超过 `8,168 B` 的大 `String` 与 `Bytes` 缓冲自动切换为 `PayloadRole::ExternalRc`（Immix 3-word 薄壳 + 堆外原子引用计数缓冲 + STW `WeakObject` 清扫与外部内存压力记账，上限 `1 GiB`），超出上限时返回 `ObjectTooLarge`，不截断长度。Immix 使用 side mark metadata，与 LOS
metadata 分开，worker 和 mutator 各有稳定的非空 TLS 身份。

每个 mutator 通过 operation 记录运行深度与线程 owner；collector 用 epoch
等待全部实例停顿，Idle Engine 的根也保留。登记和销毁使用 lifecycle reservation，
确保 MMTk bind/flush/on_destroy 不能与收集重叠。解释器在完整指令边界 poll，
GC 停顿后继续执行，抢占才交给 scheduler 重新入队。

根域独立存放在稳定 UnsafeCell 中，与 Heap 分离；公开 API 获取拥有的数据，
不暴露可直接修改的根容器。字符串常量和 builtin 返回在同一操作内分配、初始化、
入根。交给 builtin 的堆参数有 context 生命周期内的临时根，允许覆盖原寄存器
后再收集。同线程跨 Engine 的分配、执行或主动收集当前明确报错。

真实 Immix 集成回归使用独立测试进程、32 MB 堆：多 Engine、跨线程非分配循环、
连续收集、闲置常量和大字符串、捕获帧/slots/closure，以及恢复后的具体值均已验证。
早期回归使用 pinning roots。普通根的移动 slot 写回现已随 Error 阶段合入，
全仓与独立验收已通过；其他 plan、通用 HostRoot/shadow API 仍未完成。
Native SP/FP 切换仍见 continuation ABI 的缺口。

完整 Error 实施正在迁移普通托管根为可更新槽位。读回实际实现发现，原来的
pinning-root 路径固定所有直接根，因此不能证明 wrapper 本身在移动后仍然
正确。迁移需要同时保证槽位来自独占可写遍历、跨分配载荷重新从根读取，
以及 native 已交付复制值的保留契约。无法由宿主更新的 native 副本只在其
builtin 生命周期内固定；可重新载入的根句柄允许对象移动。这项工作已合入
根工作区，独立复跑观察到实际地址变化、完整128位错误标签与正确载荷。
分配压力下集合增长、字段转换与多次continuation恢复均通过；这不表示每个
分配点都发生过收集，也不宣称其他collector plan或原生栈ABI已完成。

## 详细文档

- [large-object-management.md](large-object-management.md) — 大对象分配与管理
- [extern-object-management.md](extern-object-management.md) — FFI 外部对象的生命周期管理
