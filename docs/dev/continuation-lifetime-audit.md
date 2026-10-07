# Continuation 暂停栈的 GC 生命周期

本阶段补齐语言 continuation 的不可达模板回收。全项目设计目标仍未完成，
本专项通过不能替代全设计审计。捕获/恢复仍分离或重接 delimiter 栈段，
只有 fork/clone 复制上下文；指令、wrapper 单字 immediate payload 和 NSBC 格式未改变。

## 行为与安全边界

TaskStacks 区分原始/待发布捕获与语言所有者。前者和活动链是强根；后者
通过弱 wrapper 登记。GC 到达 wrapper 后才扫描暂停链，写回移动后的槽值；
新发现的 wrapper 在下一轮闭包后加入，直到没有新增所有者才释放其余模板。
因此可达引用链与循环存活，不可达 self/cross cycle 在任务结束前归还栈池。

RootRegistration 的稳定地址及注册锁保护普通/条件扫描，STW 屏障提供独占访问。
弱访问不扫描已消费/结束句柄；清除不可达链之前无尚未完成的栈槽工作。
wrapper 分配时捕获仍为强根，初始化后登记弱所有者，handler 参数通过临时根
发布到帧。clone 使用独立所有者；once 恢复移除登记并使原栈成为活动强根。

普通寄存器、local slots 仍按物理槽保守扫描，源码作用域结束不能证明根已清空。
本阶段不声称精确源码生命周期或原生机器栈切换已经完成。GC 默认 Immix 的对象
移动有实际断言；两阶段 forwarding hook 已接入，未以 Immix 测试证明其他 plan。

## 验证证据

- 隔离 runtime 作者：36 个单元测试通过，新增4组所有权、嵌套访问次序、稳定
  地址、原始 fork 及消费/finish/释放计数测试。日志
  `/tmp/nessa-continuation-runtime-{test,clippy}.log`。
- 隔离测试作者：9组生命周期测试、32次实际完成 GC；具体断言任务仍为 Ready、
  捕获及 pool 精确计数、heap String、raw/pending、self/cross cycle、外部根、
  三所有者不动点、clone/once、嵌套链。实际移动测试要求 weak-only 下游 wrapper
  和 captured String 地址都变化，句柄和值保持正确，最后清根回收。
  日志 `/tmp/nessa-continuation-gc-tests-nine-final.log` 和 `clippy-final.log`。
- root 合并后8组旧版本生命周期回跑通过；全 workspace 最终包含9组新版测试，
  1446通过、0失败、1项已有忽略；现有源码 forced-GC multishot 与删源新进程
  archive 回归包含在全套测试。日志 `/tmp/nessa-continuation-lifetime-workspace.log`。
- fmt、workspace all-targets check、strict Clippy、diff whitespace 通过。
  日志 `/tmp/nessa-continuation-lifetime-{fmt,check-workspace,clippy-workspace,diffcheck}.log`。

初始 compile 的指针 const/旧测试调用和严格 lint 的 unsafe block/嵌套 if 均修复，
失败日志保留。移动 fixture 初始连续垃圾没有足够空洞，未满足移动断言；改为
垃圾与保留对象交错分配，失败日志保留于 `movement-first.log`，未弱化断言。
未执行 baseline-red，不以它支持结论。

## 并行与独立验收

使用 graph-engineering-workflow：先只读调查，runtime 与测试在两个隔离副本
并行，root 唯一合并 GC/interpreter；独立 grader 仅收到本阶段 rubric、最终源码、
基线、差异、运行记录和日志。调查阶段中断后以新的有界实现记录继续，没有
将调查当作已验收实现。记录 `/tmp/nessa-continuation-lifetime-implementation-graph.json`；
基线 `/tmp/nessa-continuation-lifetime-baseline.tar`，差异 `integrated.patch`。

冻结 C11：提前回收、循环、不动点、原始/待发布所有权及移动安全。
冻结 C12：发布/clone/once、多次恢复、原栈链接 ABI、全 workspace 质量及现有
源码/归档行为。独立验收首轮11/11适用项通过，C4不适用；
修复与全 rubric 重评循环开启，最多2轮，首轮无未解决缺陷。
C1/C2/C7/C8为运行记录核对，其余为产物检查或独立复跑。

独立全套复跑同为1446通过、0失败、1忽略；serial 生命周期9/9、32次完成
GC、runtime36/36，所有质量检查 exit 0。日志
`/tmp/nessa-continuation-lifetime-grader-{workspace,gc,fmt,check,clippy,diff,artifacts}.log`。
最终 graph 消耗4/5角色、峰值3/4并发、2/4波次、0/1重试、1/1深度、
4/8可观测 worker turn、1/2验收轮，未超过3600秒限制；独立验收时快照2422秒。
