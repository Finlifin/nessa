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
生效，进程首次初始化决定 plan 和容量。对象头保留 u16 payload words，字符串
超出当前容量时返回错误，不截断长度。Immix 使用 side mark metadata，与 LOS
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
