# 大对象管理 (Large Object Management)

## 大对象阈值

对象大小超过 **8 KB** 时视为大对象，不使用 per-task bump allocator，而是直接从 MMTK 的 Large Object Space (LOS) 分配。

## 分配策略

```
1. codegen/interpreter 在分配前检查对象大小
2. size ≤ 8KB → bump allocator (快速路径)
3. size > 8KB → MMTK LOS 分配:
   - 按页对齐分配
   - 对象独占连续页
   - 不参与 nursery copying（直接进入老生代）
```

## 回收策略

大对象的回收与普通对象不同：

- **不移动**: 大对象在 GC 期间不会被 copy/compact，原地标记回收
- **独立追踪**: LOS 维护自己的 free-list，回收后页直接归还
- **碎片化可控**: 每个大对象独占整页，回收后不产生内部碎片

## 典型大对象

- 超大 `String`（> 8KB 的文本）
- 大 `List`/`Map` 的内部 buffer
- 编译期常量池（一次分配，运行期只读）
- Continuation 对象（捕获大量栈帧时）