# Continuation 栈段 ABI

## 目标与语义

在 delimiter 边界切换到独立栈段，捕获和恢复通过转移栈段链接完成，避免把
当前帧复制到堆对象、恢复时再压回另一条栈。一个 task 可以同时拥有根栈、
嵌套 delimiter 栈、暂停的 continuation 栈，以及多个独立计算分支。

无 `catch` 的效应继续使用 in-place handler。语言层 `Continuation` 的多次
调用、延迟调用和显式 `clone` 语义保持不变；内部执行句柄是线性的，每个
实际执行分支有独立的栈段。需要重复执行的语言调用先建立分支，再恢复该分支；
编译器能证明唯一使用时可以直接转移原分支。目前仅证明新捕获 handler 参数在
整个 handler body 中只有一次引用且是直接尾调用；别名、逃逸、clone、循环或
多次引用保留多次恢复路径，不把一次语法调用等同于任意场景的单次运行。

## 执行上下文与所有权

`TaskState` 持有 `TaskStacks`，当前寄存器、PC、函数、调用帧和 handler 帧属于
当前栈段。段之间通过父链接连接；根段没有 prompt，一个消除块的 delimiter 段
可同时具有该块中多个可捕获效应的 prompt。
解释器的每个栈段上下文存放在独立的稳定地址，配有栈池 slot。

当前函数的 local value slots 也由 StackContext 拥有。普通调用将它们的 Vec
所有权移动到 CallFrame，返回时移回；底层 payload 地址保持稳定。捕获/恢复不
复制当前槽或保存帧槽，fork 才为分支复制这些值。任务结束释放槽的分配。

1. **进入**：父计算保存进入后的 PC；delimiter body 在新段中执行，仅显式参数
   写入新段的参数寄存器，不复制父调用帧或整个寄存器文件。
2. **捕获**：从当前段沿父链接寻找最近匹配的 prompt。分离该段以下的父链接，
   将当前段到匹配段的整个链登记为暂停的 continuation，切回父计算。
3. **恢复**：验证句柄属于当前 task 且尚未消费；将捕获链的边界连接到当前调用者，
   消费执行句柄，切回捕获点并写入恢复值。帧、寄存器存储与栈槽保持原地址。
4. **返回**：普通函数返回先展开段内调用帧；delimiter body 返回时释放该段，
   切回父调用者并把结果写入其 r0。嵌套段依次返回。
5. **克隆**：显式分支操作复制暂停链的 VM 上下文，分配新的栈槽；分支之间的
   寄存器与保存帧独立，指向同一语言堆对象的值仍保留原本的共享语义。
6. **丢弃**：释放整个暂停链。task 完成、取消和销毁同时回收所有活动及暂停段。

捕获的查找成本取决于 delimiter 深度；重新连接的恢复不遍历或复制调用帧。
克隆成本取决于捕获的状态大小。这里是操作范围说明，尚无性能基准支持倍率结论。

## NSBC v3 编码

沿用 32-bit 指令：opcode 位于 `[31:24]`，`amode` 为零，payload 位于 `[21:0]`。

| 指令 | opcode | payload `[21:17]` | `[16:12]` | `[11:0]` |
| --- | --- | --- | --- | --- |
| RESET | 0xC5 | prompt 寄存器 | 参数数量 | body FuncId |
| SHIFT | 0xC4 | prompt 寄存器 | 恢复值目标寄存器 | 必须为零 |
| RESUME | 0xC6 | 执行句柄寄存器 | 恢复值寄存器 | 必须为零 |
| CLONE_CONTINUATION | 0xC8 | 新句柄目标寄存器 | 原句柄寄存器 | 必须为零 |
| DROP_CONTINUATION | 0xC9 | 执行句柄寄存器 | 必须为零 | 必须为零 |
| RESET_CLOSURE | 0xCC | body 闭包寄存器 | 已安装 handler 数量 | 必须为零 |
| RESUME_CONTINUATION | 0xCD | 语言 Continuation 寄存器 | 恢复值寄存器 | 必须为零 |
| RESUME_CONTINUATION_ONCE | 0xCE | 语言 Continuation 寄存器 | 恢复值寄存器 | 必须为零 |

- prompt 为非负 32-bit 整数。重复 prompt 匹配最近的动态边界。
- RESET 将 r0 开始的参数传入 body；数量须与 body 的声明一致。此窄指令支持
  12-bit FuncId 和最多 31 个参数，更宽编码需要后续配套实现。
- SHIFT 将内部执行句柄作为 delimiter 的结果写入父计算的 r0；恢复值写入
  捕获段指定的寄存器，继续执行 SHIFT 后的指令。
- RESUME 消费一个分支的内部句柄；计算返回值写入 resume 调用者的 r0。
- 内部句柄以 unsigned immediate 编码，进程内不重复分配；外来、伪造的未知或
  已消费句柄返回运行时错误。语言层使用具有 `Continuation` 类型头的堆对象，
  payload 为带 unsigned immediate tag 的内部句柄，避免被通用 GC 槽扫描误认为
  指针；其他堆对象或不合法 payload 不能作为 continuation 恢复。
- `PUSH_HANDLER_CLOSURE` (0xCA) 的 payload 为 closure 寄存器:5、effect TypeIndex:17；
  `PUSH_CAPTURING_HANDLER` (0xCB) 将 effect 字段换为 metadata 常量索引:17。
  metadata 为 UInt，低 32 位为 effect TypeIndex，高 32 位为 catch 参数位置。
- `RESET_CLOSURE` 传入闭包捕获参数并保留可见 handler 环境；重复的祖先绑定被
  去除。多次恢复默认先 fork 模板；`RESUME_CONTINUATION_ONCE` 直接消费模板。
- v1/v2 及未知版本 archive 被拒绝，避免按新的 ABI 解释旧的控制指令；旧产物需重新编译。

## GC 与安全约束

活动栈、被暂停的父调用者、原始 ABI 句柄及尚未发布为语言值的捕获链是强根。
语言 continuation 模板通过其堆对象的弱所有者登记；只有该对象已被 GC 到达，
才扫描模板中的寄存器、调用帧保存值、当前与保存帧的 local slots、display 状态、
闭包环境和 handler 环境。扫描写回移动对象的新地址。

弱处理采用不动点：新到达的所有者暴露暂停栈中的引用，完成这些引用的传递闭包后，
继续发现其他所有者。直到一轮没有新增所有者，才释放剩余不可达的模板。因此外部
可达的相互引用链保持有效，无外部根的 self/cross cycle 会在 task 结束前回收。
弱所有者地址也随对象移动更新；两阶段 collector 的转发阶段重新处理保留的栈槽。

捕获链在 wrapper 分配期间仍为强根；初始化并登记 wrapper 后，将其作为 handler
参数临时登记，直到参数安装到调用帧。显式 clone 产生新的独立所有者；一次恢复
消费所有者登记，原链成为活动强根。task 完成、取消或 VM 销毁仍释放所拥有的栈段。

栈池 slot 由拥有它的段释放一次；continuation 不能跨 task 恢复。被取消 task 的
内部句柄不再有效。语言层跨 task 操作应给出诊断或运行时错误，不能依赖未定义行为。

## 当前实现范围与后续要求

已接入解释器栈段、调度器生命周期、GC 根枚举及上述字节码指令；测试覆盖
地址稳定性、嵌套 prompt、普通调用帧、多分支计算和取消释放。

源码 `catch` 声明、handler 参数与闭包、NIR/codegen、语言 continuation 对象及
调用已接通。源码测试覆盖 in-place、最近 handler、多 prompt、多次/延迟恢复、
再次捕获、显式 clone 和放弃恢复；单次使用证明的正反案例检查指令与结果。

效应声明返回类型现已约束语言 continuation 的恢复输入，弱所有者保留输入类型
及捕获 scope，所有恢复在分支/消费前检查；稳定 catch/alias/clone 有静态精化，
普通 handler 的提前 return/resume 按独立闭包边界检查。完整静态 continuation
类型和精确 answer 约束仍须完成，已有异构恢复结果保持不变。另须完成 evidence passing、
async handler、完整的活跃区间/寄存器优化及原生参数传递。frame slots 已消除
永久寄存器重用与跨调用值丢失；编码容量在 checked codegen 路径诊断。不可达语言
continuation 的模板链已由 GC 弱处理释放；但普通寄存器和 local slots 仍按物理槽
保守扫描，失去最后一个源码引用不代表这些槽已清空。精确源码生命周期仍需要
活跃区间或槽清除支持。上述证明以外的单次调用仍然建立分支，有待更完整的所有权分析。

当前解释器在 Rust 主机栈上执行调度循环，VM 调用帧由 Rust 容器保存；栈池为每段
预留独立映射。未来原生执行路径还须实现 SP/FP 和平台保存寄存器切换，以及
native/FFI 跨 delimiter 调用的约束。当前实现不声称已经完成原生机器栈切换。
