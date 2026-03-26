# Safe-Point 机制

## 什么是 Safe-Point

Safe-point 是程序执行中的一个点，在该点上 VM 可以安全地暂停 task 执行以进行 GC 或调度切换。"安全"意味着此时所有 GC root 的位置已知（通过 stack map），不存在中间状态的寄存器值。

## Safe-Point 插入位置

Codegen 阶段在以下位置插入 safe-point 检查指令：

```
1. 函数调用点 (CALL 指令前)
   → 调用前的寄存器状态已固定，可安全暂停

2. 函数返回点 (RETURN 指令处)
   → 隐式 safe-point

3. 循环回边 (back-edge)
   → while/for 循环体末尾跳回循环头的位置
   → 防止长循环阻塞 GC

4. Effect 调用点
   → effect 可能触发 task 切换，必须是 safe-point
```

## Safe-Point 检查实现

```
每个 safe-point 编译为:

  SAFEPOINT              // 伪指令，检查全局标志

实际生成的逻辑:
  if gc_flag || preempt_flag:
    save_state_to_stack_map()
    yield_to_scheduler()
```

safe-point 检查被设计为极低开销：通常只是一次内存读取 + 条件分支（分支预测几乎总是 not-taken）。

## Stack Map

每个 safe-point 关联一个 stack map entry，描述此时栈和寄存器中哪些 slot 是引用类型：

```
StackMapEntry {
    pc_offset: u32,              // 指令偏移
    ref_bitmap: BitVec,          // 哪些寄存器/栈 slot 包含 HeapObject 引用
    deopt_id: Option<u32>,       // 用于 deoptimization（JIT 预留）
}
```

GC 在暂停 task 后，根据当前 PC 查找对应的 stack map entry，精确枚举所有 GC root。

## 双重用途

Safe-point 同时服务于两个目的：

1. **GC**: 允许 collector 安全地枚举和移动堆对象
2. **调度**: 允许 scheduler 抢占当前 task（协作式抢占）

这避免了引入两套机制——检查一个标志位即可响应 GC 和调度请求。