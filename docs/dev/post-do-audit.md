# 尾随 do 回调实现与验收

当前语法规范和 effect/concurrency 示例包含 expr do(lambda|block)。本轮明确
等价展开：f do lambda 为 f(lambda)，f(args) do lambda 为追加普通位置实参，
do block 为零参数 Lambda。原 parser 的 PostDo AST 与源 span 保留；resolution
先遍历可达树，按子节点优先正规化，再创建任何名称、类型或默认 adapter facts。
PostDo wrapper 保留原 NodeIndex；原 callee/实参/body 节点身份不变，block 仅新增
Lambda wrapper。原 inner Call 成为不可达 arena 历史节点，不被名称/类型遍历。
准确源码 node 权限、package/import 身份不会按名字重新授予。

后续完全复用 Call/Lambda：callee和源码实参单次求值、共享 CallArgumentPlan、
上下文 Fn、optional/named/单List变参、具体/动态apply、closurecaptures、回调
return与loop控制边界、初始化 invoked callback 依赖和 associated/default adapter
facts。闭包创建不执行 body，block 没有隐式参数。命名与位置实参混用沿用原绑定
规则，不能因 do 重新推断已按名字供给的位置槽。无新增NIR/opcode/ABI/归档布局。

graph-engineering-workflow 使用只读调查、root唯一生产作者、两个隔离测试作者
并行和独立验收。Host拒绝新agent thread，转用已有独立agent context的新任务；
验收者没有本轮builder transcript。graphify没有CLI或现成图，未生成知识图。
实时记录 /tmp/nessa-post-do-graph.json，baseline /tmp/nessa-post-do-baseline.tar。
root读回并比较两个isolatedtree；仅3个明确授权resolution刷新文件不同，其余
原文件字节一致。只合并3个新测试，随后root增加boundary与archive精确证明。

行为作者11组验证裸callee/已有Call、block/显式lambda、类型alias/Fn反射、普通
默认/命名/变参绑定、callee及args trace1234和214、精确单次求值、创建closure时
观察先前实参写入、返回capturedclosure、heapref共享、null/Unit、嵌套/chains、
callback-local return、15个静态拒绝/noartifact和动态TypeError/MethodNotFound。
GC作者4组验证heapString capture、浅快照、共享Cell、返回closure与multishot。
实际收集最低5/4/4/7次，结果42、最终活动栈0；multishot两次15/25且entries5。
归档作者5组source-run/build/删除测试自产source/freshprocess runNSBC，明确检查
输出与错误；root增加第6组，覆盖Item i64/String、冻结scope proof及init软依赖环。
root另4组证明原PostDo identity/span/原callee/args、准确CallArgumentPlan与result
类型，associated/default body、Self alias closure 和 break/continue 不越过lambda。

作者初版named fixture与Any整数callable预期不符普通契约，已按原绑定及apply
MethodNotFound修正并新增普通调用对照拒绝。GC作者初版field compound语法和算术
误写、root初版trait缺少assoc默认值/derive关键词均为fixture纠正，没有生产修复。
这些历史失败保留在实际logs，不作为产品回归或绿测证据。

当前root logs /tmp/nessa-post-do-{source,boundary,archive,workspace,fmt,check,clippy,diff}.log。
root和独立验收者全workspace1362通过、0失败、1项已有doctest忽略；fmt、
all-targets check、严格Clippy及diff检查均exit0。独立首轮11/11适用项通过，
C4无外部研究不适用，gate为passed，未进入生产修复轮。另6个CLI探针验证shadowed
callee、连续do block、嵌套do及factory目标/Unit结果/未授权builtinimpl拒绝，均
检查具体输出或build失败无产物。日志 /tmp/nessa-post-do-grader-*。

C1/C2/C7/C8依运行中记录核对，属于record/attested；其余适用项由独立读取或
实际重跑验证。参与者5/5，并发峰值3/4，wave3/4，retry0/1，depth1/1，worker
turn5/7，graderround1/2；最终独立观察973/2400秒。没有未通过或不可达到的标准。
没有提交、推送、部署、发布、删除项目内容、付款或外发。

完整tacit lambda、泛型、Hash、公共trait carrier、精确动态函数指针初始化流、
跨包稳定身份、原生SP/FP与完整async等总体设计尚未全部完成；goal保持active。
