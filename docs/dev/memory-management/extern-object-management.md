# 外部对象管理 (Extern Object Management)

## 问题

FFI 调用可能返回 C 侧分配的内存（如 `malloc` 分配的结构体、文件句柄、socket 等）。这些对象不在 MMTK 管辖范围内，但 Nessa 代码持有对它们的引用。需要解决两个问题：

1. **GC 安全**: Nessa 堆对象引用外部指针时，GC 不能追踪/移动外部对象
2. **析构保证**: 当 Nessa 侧的引用不再可达时，需要调用对应的 C 析构函数

## Extern Object Table

引擎维护一个全局的外部对象注册表：

```
ExternObjectTable {
    entries: Map<ExternId, ExternEntry>,
    next_id: AtomicU64,
}

ExternEntry {
    ptr: *mut c_void,               // C 侧指针
    destructor: fn(*mut c_void),    // 析构回调
    ref_count: AtomicU32,           // Nessa 侧引用计数
    pinned: bool,                   // 是否正在 FFI 调用中
}
```

## 生命周期流程

```
1. FFI 调用返回 C 指针 p
2. 引擎注册: table.register(p, destructor_fn) → ExternId
3. 创建 Nessa 包装对象 (HeapObject)，内含 ExternId
4. 正常 GC 追踪 Nessa 包装对象
5. 包装对象变为不可达 → GC 回收时触发 destructor:
   table.release(extern_id) → ref_count--
   ref_count == 0 → 调用 destructor(ptr) → 从 table 移除
```

## FFI 调用期间的 Pin

在 FFI 调用期间，传出的 Nessa 堆对象需要被 pin（阻止 GC 移动）：

```
1. 进入 FFI 调用 → pin 所有传出的 HeapObject 引用
2. C 函数执行期间，GC 若触发：
   - pin 的对象被标记为不可移动
   - 其他对象正常参与 GC
3. FFI 返回 → unpin 所有对象
```

## 线程安全

- ExternObjectTable 的操作全部使用原子操作或读写锁
- 析构函数由 GC finalizer 线程调用，不在 mutator 线程上执行