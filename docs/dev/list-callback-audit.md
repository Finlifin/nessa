# List 高阶回调实现与验收

具体动态 List 接通 map/filter/fold/each/foreach。元素和 fold accumulator 的公开
契约为 Any；filter 严格返回 bool，each/foreach 严格返回 Unit。泛型 List(T) 与
精确 accumulator 静态专化不是本轮结果。

std 方法先用 concat(List()) 保存输入槽位的浅快照，然后以函数栈上的 index 逐项
调用。输入顺序、次数、null/Unit、空列表以及元素对象共享均有具体值断言。
原列表的 push/pop/set 不改变本次输入；continuation 分支各自保留 index 和标量
accumulator，结果 List 和元素堆对象依然共享，没有深复制或隐含独立结果承诺。

类型解析复用 Fn 的渐进一致关系和上下文 lambda 检查。VM 的真正间接 closure
调用按实际函数 ABI 展开参数，再检查 Value 参数并建立数字表示；trait proof 的
既有检查保持。直接源码调用使用原 lowering 检查，避免失去可选 trait 的词法权限。
NIR 对间接返回值检查调用位置的准确类型，忽略结果的 Unit 调用同样检查。
合成 handler 可以没有源码 Fn 描述符，不能把 INVALID 当真实参数签名。

List/Map 源方法记录共享 instance_methods 目标事实；已知实例调用使用共享实参
计划，receiver 参与默认值绑定。初始化扫描沿已选择参数的实际回调追踪执行，
只在调用处扫描闭包体，支持具名/全局回调、无写入局部别名、参数转发及捕获参数的
闭包。静态 false 分支不执行回调。缓存按具体 default adapter 区分，流去重键含
body/root 上下文。任意可变函数指针、复杂动态回调来源仍依赖运行时全局守卫，
不宣称完整函数指针数据流或独立 trait 参数证明分析。

隔离作者只新增行为、GC 与独立 CLI 归档测试。root 比对 tar 基线：除明确刷新
的8个生产文件外，已有文件没有改变；只合并3个新测试。首轮真实 GC/多次恢复
暴露 handler 参数检查回归，既有可选 scoped trait 测试又暴露直接调用缺少词法
上下文的重复检查；root 修复调用边界，保留15/25/5次进入/合计42断言。真实 GC
检查至少4次完成，multishot 至少7次，最终活动栈0。归档先运行源码、build、删掉
测试自产源码，再以新进程执行 nsbc；同时验证动态 TypeError 与初始化软依赖环。
没有新增 opcode、ABI revision、归档布局或装载时补造接口。

本轮使用 graph-engineering-workflow 的有界并行与独立验收；graphify 缺少 CLI
和已有图，未生成或查询知识图。运行中追加记录 /tmp/nessa-list-callback-graph.json；
root 日志 /tmp/nessa-list-callback-{source,workspace,fmt,check,clippy,diff}.log。
首轮独立验收10/11：C4 无外部研究不适用，C12 发现参数连续转换的临时根缺失。
root 以注册的物理参数 scratch slots 保存原始和已转换值；每次分配后从实际根槽
读取，再将完整结果复制回参数数组，下一步不再分配而直接发布到 callee registers。
回归在两个 i64→i128 堆分配转换之间及之后强制实际 GC，核对根槽中的40和2、
两次完成收集和成功/失败后的临时根清理。第二轮独立复跑当前最终源码，全仓1337通过、0失败、1项已有doctest忽略，
fmt、all-targets check、严格Clippy与diff检查均exit0。最终调用点从已注册的
closure register 重新读取环境；现行collector采用pinning，不宣称通用移动GC保证。
独立rubric11/11适用项通过，C4无外部研究不适用；C1/C2/C7/C8为record/attested，
其余适用项为独立产物检查或实际复跑。gate为passed，保留首轮失败历史。
日志 /tmp/nessa-list-grader-final-{workspace,fmt,check,clippy,diff}.log 与
/tmp/nessa-list-grader-repair-unit.log。独立stress进程停止exit130，没有宣称语义复现。
参与者4/4、并发峰值2/4、wave4/4、retry1/1、depth1/1、workerturn6/6、graderround2/2，
最终验收观察2324/2400秒；报告与AGENTS文档在运行结束后整理，记录为2413秒。
修复复核属于已有验收节点的内部循环，没有新fanout。
没有提交、推送、部署、发布、删除项目内容、付款或外发。

泛型容器、完整 tacit lambda、尾随 do 回调、Hash、公共 trait carrier、跨包身份、
原生 SP/FP 和完整 async 等整体目标继续待完成。goal 保持 active。
