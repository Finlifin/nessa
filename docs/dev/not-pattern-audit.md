# not 模式实现与验收

本轮完成文档规定的 `not pattern`，贯穿 parser、名称解析、类型检查、NIR
分支和 match/matches/for。否定完整子模式，保留输入单次求值、按路径 guard
执行、失败元素跳过，以及原有 nominal enum、准确字段类型检查。

否定内部绑定使用独立子作用域，只供内部 guard 和闭包捕获；不会导出给成功
arm 或外部 guard。not 优先级95，高于 as90、or20、guard10，低于构造100、
投影110。这是文档未指定细节的当前实现选择，已向用户提出可选确认并同步规范。
修正旧 parser 前缀 ! 错误标记为 PatternNot 的偏差，保留其错误模式未实现诊断，
不使它获得逻辑否定行为；后缀 ErrorOk 不变。

生产变更集中在5个既有文件。NIR 复用子模式 branch 并交换成功/失败目标，
无新 VM opcode、运行时布局或 NSBC 格式。并行测试发现裸 E.none 在 ForLoop
缺少枚举引用解析，已将其与 CaseArm 统一；普通裸 enum/or 迭代也有回归覆盖。

两个隔离作者分别新增行为测试和 GC/归档测试，root 为唯一合并者。基线为
`/tmp/nessa-not-pattern-baseline.tar`，逐文件比较隔离副本，只允许5个生产刷新
与各自新测试文件。运行中记录在 `/tmp/nessa-not-pattern-graph.json`。
初次共享 target 的基线测试受到已构建新依赖缓存影响，弃用该红测结论；内存
作者用独立 target 重编得到 GC0/3、归档0/4 的真实基线失败。没有把缓存结果
或保留字、缩进及 Any Tuple fixture 修正当成生产证据。

行为11组覆盖标量、Any/null/Unit/Unicode、嵌套枚举/Tuple、not/or/as、内部
guard局部绑定及外部同名变量、trace142/1242/12342、输入快照、matches 与
for。17个静态拒绝案例断言明确诊断、无生成函数及无法生成 artifact。
边界4组验证真实 AST/不同 SymbolId 和作用域、关联 Item i64/String 默认适配、
实际回调初始化依赖软环、普通裸 enum/or 迭代。

GC3组共4个fixture，强制完成收集并断言结果42、活动栈0；guard捕获在否定
成功/失败后均可逃逸，outer alias持有heapTuple，multishot两枝21/21、trace132。
串行内存测试日志18次完成收集。归档4组包含5个成功fixture，源码执行、build、
删除测试创建源码、独立进程执行NSBC精确42；含冻结Self proof40+2、初始化
guard和multishot。3个绑定逃逸负例build失败且无归档。

实际日志位于 `/tmp/nessa-not-pattern-root-*.log` 与
`/tmp/nessa-not-pattern-memory-*.log`；行为修复结果在
`/tmp/nessa-not-pattern-behavior-final.log`。root全workspace1384通过、0失败、
1项已有忽略；fmt、workspace all-targets check、严格Clippy和diff检查均通过。
独立验收者复跑得到相同1384/0/1及全部质量门通过，并额外执行6个CLI探针：
for同名变量隔离、nominal不匹配跳过内部guard、double-not绑定不得逃逸、
内部alias不得逃逸、旧!仍拒绝、Tuple字段准确类型拒绝；失败build无归档。
其日志位于 `/tmp/nessa-not-pattern-grader-*.log`。

C1–C10使用技能固定rubric，C11/C12在实现前冻结；独立首轮11/11适用项通过，
C4无外部研究不适用，C1/C2/C7/C8为运行记录核对，其余来自产物检查或复跑。
本轮实施期间按责任边界修复1个for解析缺陷，独立评分后无修复轮。
实用5个worker角色、峰值3个并发agent、3波分派、1次窄复查、深度1、6个
可观测worker turns、1轮评分，分别低于5/4/4/1/1/8/2的限制。
独立评分时运行1477秒，低于2400秒限制；角色复用了既有代理的独立当前任务
上下文，未把新代理数量或代币预算写成已测量事实。未提交或发布代码。

整体 Nessa 目标继续进行。Any Tuple 模式仍需静态 Tuple 输入，声明/参数的
可失败模式、其他模式能力、泛型、完整 tacit lambda、Hash、公共trait carrier、
跨包稳定身份与完整 async 均未因本轮测试通过而视为完成。
