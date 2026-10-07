# 引擎初始化 (Engine Initialization)

引擎初始化是从 `nessa` CLI 入口到 root task 开始执行之间的完整启动过程。由 `initialization` crate 实现，`driver` crate 编排调用。

## 初始化流程

```
┌──────────────────────────────────────────────────────────────────┐
│ 1. CLI 解析                                                      │
│    nessa crate 解析命令行参数                                     │
│    → 编译模式 (dev/release/check)                                │
│    → 源文件路径或 .nsbc 路径                                      │
│    → GC 配置、日志级别、调试选项                                  │
│                                                                  │
│           ▼                                                      │
│                                                                  │
│ 2. MMTK GC 初始化                                                │
│    - 创建 MMTK 实例                                              │
│    - 配置 GC 策略 (LXR)                                          │
│    - 设置堆初始大小和最大大小                                     │
│    - 注册 object model (对象头布局、引用字段扫描函数)              │
│                                                                  │
│           ▼                                                      │
│                                                                  │
│ 3. Task Stack Pool 初始化                                        │
│    - mmap 第一个 segment (64 slots × 8MB = 512MB 虚拟地址空间)   │
│    - 设置 guard page 信号处理器 (SIGSEGV handler)                │
│    - 初始化 free list (所有 slot 可用)                            │
│    - 注册备用信号栈 (用于 stack overflow 恢复)                    │
│                                                                  │
│           ▼                                                      │
│                                                                  │
│ 4. TypePool 初始化                                                │
│    - 预注册所有 intrinsic 类型:                                   │
│                                                                  │
│      数值类型:                                                    │
│        u8(idx=0), u16(1), u32(2), u64(3), u128(4), usize(5)     │
│        i8(6), i16(7), i32(8), i64(9), i128(10), isize(11)       │
│        f32(12), f64(13)                                          │
│        BigInt(14)                                                │
│                                                                  │
│      其他内建类型:                                                │
│        bool(15), char(16), String(17), Unit(18)                  │
│        Any(19), NoReturn(20), Type(21)                           │
│                                                                  │
│    - 构建 TypeIndex → TypeID 映射                                 │
│    - 这些 TypeIndex 在 std 编译时通过 'builtin 视图引用           │
│                                                                  │
│           ▼                                                      │
│                                                                  │
│ 5. String Interner 初始化                                         │
│    - 分配 interning 哈希表                                       │
│    - 预注册保留关键字:                                            │
│        fn, let, const, if, else, while, for, in, match,         │
│        return, break, continue, use, pub, private, struct,       │
│        enum, mod, impl, extend, trait, type, effect, handle,     │
│        resume, do, when, else, true, false, null, self, ...     │
│    - 预注册内建标识符:                                            │
│        __init__, new, apply, update, iter                        │
│                                                                  │
│           ▼                                                      │
│                                                                  │
│ 6. Scheduler 初始化                                               │
│    - 检测 CPU 核心数 N                                            │
│    - 创建 N 个 Worker 线程                                        │
│    - 每个 Worker 初始化本地双端队列 (work-stealing deque)          │
│    - 设置全局 gc_flag 和 preempt_flag (原子变量)                  │
│    - Worker 线程进入等待状态                                      │
│                                                                  │
│           ▼                                                      │
│                                                                  │
│ 7. libuv Event Loop 初始化                                        │
│    - uv_loop_init 创建事件循环                                    │
│    - 绑定到主线程                                                 │
│    - 注册 timer 回调 (scheduler heartbeat)                        │
│    - 注册 async handle (worker → main 通知通道)                   │
│                                                                  │
│           ▼                                                      │
│                                                                  │
│ 8. 编译或加载字节码                                               │
│                                                                  │
│    开发模式 (nessa run hello.ns):                                 │
│      source → lexer → parser → resolution → nir → codegen        │
│      → 内存中的 NSBC (不写文件)                                   │
│                                                                  │
│    运行模式 (nessa run hello.nsbc):                                │
│      读取 .nsbc 文件 → 验证 checksum → 反序列化                   │
│      → 加载到内存中的 NSBC 结构                                   │
│                                                                  │
│    发布编译 (nessa build --release):                               │
│      source → 完整编译管线 → 写入 .nsbc 文件 → 退出               │
│                                                                  │
│    检查模式 (nessa check):                                        │
│      source → lexer → parser → resolution → 报告诊断 → 退出      │
│                                                                  │
│           ▼                                                      │
│                                                                  │
│ 9. 依赖包加载                                                     │
│    - 读取 package.lock 获取精确依赖列表                           │
│    - 按拓扑序加载每个依赖包的 .nsbc archive                       │
│    - 执行跨包链接:                                                │
│        IMPORTS 中的 resolved_id 填充为实际的运行时 ID              │
│    - 按拓扑序执行每个包的 __init__ 函数                           │
│                                                                  │
│           ▼                                                      │
│                                                                  │
│ 10. 创建 root Task 并执行                                         │
│    - 从 Stack Pool 分配一个 8MB stack slot                        │
│    - 创建 TaskState (status=Ready, pc=main函数入口)               │
│    - 放入 Scheduler 队列                                          │
│    - 唤醒 Worker 线程开始执行                                     │
│    - 主线程进入 event loop (uv_run)                               │
│                                                                  │
│           ▼                                                      │
│                                                                  │
│ 11. 主循环                                                        │
│    主线程: uv_run 处理 I/O 事件，唤醒等待中的 task                │
│    Worker 线程: 从队列取 task → 执行指令 → safe-point 检查        │
│    → root task 完成 → 收到通知 → 开始 shutdown                   │
│                                                                  │
└──────────────────────────────────────────────────────────────────┘
```

## 关闭流程

```
Root task 完成后:
  1. 设置全局 shutdown 标志
  2. 通过 safe-point 通知所有 Worker 线程
  3. 等待所有运行中的 task 完成或 cancel
  4. 销毁 Scheduler (join Worker 线程)
  5. uv_loop_close 关闭事件循环
  6. 释放 TypePool 和 String Interner 内存
  7. 释放 MMTK GC (释放所有堆内存)
  8. munmap Stack Pool 所有 segment
  9. 返回 root task 的退出码
```

## 错误处理

初始化阶段的错误立即终止进程（不经过 effect 系统）：

| 阶段 | 可能的错误 | 处理 |
|------|----------|------|
| GC 初始化 | mmap 失败 | 打印错误，exit(1) |
| Stack Pool | mmap 失败 | 打印错误，exit(1) |
| 编译 | 语法/类型错误 | 通过 diagnostic 报告，exit(1) |
| 包加载 | 找不到依赖 / checksum 不匹配 | 报告错误，exit(1) |
| __init__ | panic | 报告 panic 信息，exit(1) |

## 配置选项

通过命令行或环境变量配置：

```
--heap-size=<bytes>       初始堆大小 (默认: 256MB)
--max-heap-size=<bytes>   最大堆大小 (默认: 4GB)
--workers=<N>             Worker 线程数 (默认: CPU 核心数)
--stack-size=<bytes>      每个 task 的栈大小 (默认: 8MB)
--gc-strategy=<name>      GC 策略 (默认: lxr)
```

## 当前实现边界（2026-10-07）

driver 源码编译已解析并合并7个真实 std 源文件，通过可信节点权限暴露 builtin，
以真实模块与 `use std.prelude.*` 提供用户 API。用户入口明确来自根文件 main。
已生成 File/Module/Struct/Enum initializer、bootstrap 和全局类型/可变性 schema。
VM 先安装 schema；各已加载作用域按源码顺序执行顶层值和语句，再调用无参数、
返回 Unit 的 `__init__`，最后执行用户 main 并保留返回值。初始化错误阻止 main。
未引用作用域的 hook 不运行；import/reference 决定加载集合，已知初始化值读取
及 helper 调用决定硬依赖。软 import 环按稳定顺序执行，硬值依赖环诊断；同
作用域前向读取报 UninitializedGlobal。动态依赖通过受检全局读取发现错误。

共享槽拒绝非法类型/索引、错误类型写入与 const 二次写入；闭包不捕获全局旧值，
globals 纳入 GC 根。分支、循环和带 guard/标签的 break/continue 初始化路径已
覆盖。源码和归档现在共用 `driver::install_artifact`：验证类型池、字节码与
builtin ABI/ID/名称之后，依次安装类型池、global schema、函数和常量，再执行
保存的入口。CLI build 写完整 `CompiledArtifact`，run 的 `.nsbc` 输入按字节读取；
加载不需要原源码，也不重新安排模块初始化。完整产物带 checksum、目标兼容
检查和方法名称重定位；源文件删除及 interner 扰动的独立进程回归已通过。
低层 `write_archive` 仍拒绝非空 globals，因为它没有类型池与 entry；完整
`write_artifact` 已支持这些字段。

Newtype/Impl/Extend 关联作用域、通用包发现、跨包链接和完整参数绑定 ABI 仍未
完成；optional/default/variadic 缺省实参没有生成，缺参调用明确拒绝。上文多
worker、事件循环、完整配置和关闭流程仍是设计目标。原生 SP/FP 切换、语言
continuation 最后引用回收及精确 stackmap 也未因独立加载验证通过而完成。
