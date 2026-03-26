# 内存管理 (Memory Management)

Nessa 采用 MMTK (Memory Management Toolkit) 框架集成 LXR GC，结合 Work Packet 并行化设计和 mmap'd Task Stack Pool 管理。

## 设计概览

```
┌────────────────── Memory Subsystem ──────────────────────────────┐
│                                                                   │
│  ┌─────────────────────────────────────────────────────────────┐ │
│  │ MMTK Framework                                               │ │
│  │                                                              │ │
│  │  ┌──────────────┐  ┌──────────────┐  ┌───────────────────┐ │ │
│  │  │  Allocators  │  │  LXR GC      │  │   Work Packets   │ │ │
│  │  │  (per-task   │  │  (collector)  │  │   (并行 GC 任务   │ │ │
│  │  │  bump alloc) │  │              │  │    调度框架)      │ │ │
│  │  └──────────────┘  └──────────────┘  └───────────────────┘ │ │
│  └─────────────────────────────────────────────────────────────┘ │
│                                                                   │
│  ┌─────────────────────────────────────────────────────────────┐ │
│  │ Task Stack Pool (Segmented List)                             │ │
│  │   mmap'd 8MB slots, guard pages, demand-paged               │ │
│  └─────────────────────────────────────────────────────────────┘ │
│                                                                   │
│  ┌─────────────────────────────────────────────────────────────┐ │
│  │ Extern Object Table                                          │ │
│  │   FFI 分配的 C 对象注册与析构                                 │ │
│  └─────────────────────────────────────────────────────────────┘ │
│                                                                   │
└──────────────────────────────────────────────────────────────────┘
```

## 分配路径

### 快速路径 (Hot Path)

每个 Task 持有一个 per-task bump allocator：

```
1. task 向 MMTK 请求一个 allocation buffer (如 32KB)
2. 后续分配只需 bump pointer += object_size
3. buffer 用尽时向 MMTK 请求新 buffer
4. 无锁、无竞争——每个 task 独立分配
```

### 大对象分配

超过阈值的大对象（如 > 8KB）直接走 MMTK 的 large object space，避免碎片化。

详见 [large-object-management.md](large-object-management.md)。

## GC 策略：LXR

LXR (Lattice-based Cross-reference) GC 的核心特性：

- **分代收集**: 新生代 (nursery) 使用 copying collector，老生代使用标记-整理
- **并发标记**: 标记阶段可与 mutator 并发执行（减少 STW 时间）
- **Work Packet 并行化**: GC 工作被拆分为细粒度的 work packet，由 worker 线程并行处理

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
  1. 每个 task 的 register file (20 个 TaggedValue)
  2. 每个 task 的 call stack (通过 stack map 精确定位引用)
  3. 全局变量表 (global declarations)
  4. effect handler 证据链中持有的 handler 对象
  5. FFI pin table 中的已 pin 对象
```

## Task Stack Pool

详见 [architecture.md](../architecture.md) 5.3 节。

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

## 详细文档

- [large-object-management.md](large-object-management.md) — 大对象分配与管理
- [extern-object-management.md](extern-object-management.md) — FFI 外部对象的生命周期管理