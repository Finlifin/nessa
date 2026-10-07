# 单次 next 静态迭代协议验收

新源码的 builtin Iterator 使用必须绑定的 Item，next 返回准确
IterationStep(Item)；IntoIterator 必须绑定 Iter，into_iter 返回该具体类型。
所有实现经过声明签名检查，未使用的 IntoIterator 也检查 Iter 的 Iterator
义务。普通具体返回值不携带隐藏的跨作用域证明，调用者在自己的有效作用域
选择 Iterator；公共动态关联 carrier 仍未实现。

ForLoopPlan 在 body 检查前确定准确 Item、结果类型和精确 impl/scope，完成
trait 检查后冻结源函数。默认方法的共享 AST 保存、保留和恢复各实现独立的
循环 facts；NIR 直接调用冻结目标。隐式 next/into_iter 也进入初始化依赖分析。
输入及 into_iter 各执行一次，header 每轮只调用一次 next；done 结束，yielded
载荷匹配成功后执行 body，不匹配则跳过。break、continue、嵌套标签和 tuple、
enum、guard 模式均有执行断言。未支持的模式继续诊断。

List 的准确 Item 是 Any，允许 null/Unit；ListIterator 持有元素引用的浅快照
和独立游标，原列表后续增删改不影响本次遍历。快照中的对象仍共享。真实 GC
与两次 continuation 恢复验证堆 String 和 null 元素保活，至少四次完成收集，
结果42，活动栈释放。测试也核查独立迭代器、耗尽后重复 done 和迭代期间修改。

Required 关联声明使用 TPOL10 expression tag7，不以 Any 或循环默认作占位。
元数据往返、必须提供绑定、禁止嵌套 Required、1–9降版拒绝均有回归。其他池
保留5–9策略。新声明仅由源 Resolver 安装，TypePool.with_intrinsics 和 reader
不升级旧协议。手写旧 ABI1 归档保存 has_next/next=None 与空关联列表，读取和
字节一致重存后独立 CLI 输出42。新源码删除后的独立执行覆盖过滤/消费次数、
List 快照、不同作用域默认方法循环。

并行测试写入者只修改 `/tmp/nessa-single-next-test-worker` 的隔离副本，root
合并交付测试并修复生产路径；另一只读调查代理实际运行旧归档探针。
运行期间追加记录位于 `/tmp/nessa-single-next-graph.json`。全仓检查日志位于
`/tmp/nessa-single-next-{full,fmt,check,clippy,diff}.log`；相关路径证据另见
`{main,target,metadata,pipeline-dev,archive,archive-full,legacy}.log`。
新上下文的只读审查者独立复跑：全 workspace 1266 passed、0 failed、1 个原有
ignored；目标测试32、类型与元数据测试263、CLI 归档测试98均通过。fmt、全仓
check、严格 Clippy 和 diff 检查全部 exit0。独立日志位于
`/tmp/nessa-single-next-independent-{target,metadata,archive,full,fmt,check,clippy,diff}.log`。

固定 C1–C12 rubric 最终12/12通过，第一轮 gate 为 passed，没有不适用、失败或
不可达项。C1、C2、C7、C8 是运行记录的 attested 结论，其余由独立源码检查或
实际重跑取得。修复循环处理了隔离测试暴露的生产缺陷，并修正独立复核发现的
一条陈旧研究记录；修复后的最终证据已复核。

本轮实际使用5个 worker（上限6）、峰值并发3（上限4）、2波（上限3）、
1次派发重试（上限1）、深度1（上限1）、5个 worker turn（上限6）、
1轮 grader（上限2）；用时不足60分钟。新兼容性代理派发曾被主机线程上限
拒绝，随后复用已有只读代理完成调查，没有将失败派发当成已完成工作。
root 负责合并，测试写入者使用隔离副本；graph-engineering-workflow 用于
依赖、边界和验收管理。graphify 因本机没有 CLI 或现成图而未运行。
没有待解冲突，也未请求或执行提交、推送、部署、发布、付款或对外发送。

本轮完成静态单步 Iterator、for 和 List 垂直接入。公共动态关联 carrier、泛型
List、更多模式、其余类型与标准库设计目标仍需继续，完整项目目标保持 active。
