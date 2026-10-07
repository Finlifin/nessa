# and is 约束模式实现与验收

本轮完成文档语法 `pattern and expr is pattern`，原实现仅将 and 两边当成
普通模式，没有消费 is，类型检查也未实现。现在 PatternAndIs 明确保存三个
固定子节点：左模式、计算表达式、右模式；AST node_type 已改为 TripleChildren，
dump、合并和通用遍历使用实际基数。不制造 BoolMatches 节点绕过其私有绑定规则。

名称解析顺序为左模式、表达式、右模式。左右成功绑定都向后可见，预分配的
右侧 SymbolId 不会使其名称提前可见；绑定收集只读两个模式，排除中间表达式。
左右重复绑定和 or 的名称/准确类型契约沿用已有检查。右侧匹配按计算表达式
类型检查，不复用原输入类型。not 的内部作用域和 matches 的非导出规则不变。

NIR 左模式成功后才 lower 计算表达式，snapshot 保存值及 trait proof，然后
调用既有 branch 匹配右侧；任一失败通向同一 failure。无新增 VM opcode、
continuation ABI 或 NSBC 格式。and 优先级30、左结合，高于 or20、guard10，
低于 as90、not95；表达式解析保留 no_extended_call，避免吸收 for 的 body。

原文档未明确右侧绑定是否导出，本轮先按模式组合的默认语义导出左右成功
绑定，随后用户明确确认“导出左右两侧绑定”。当前实现、规范与测试保持一致。
声明/参数仍拒绝此可失败模式，动态 Any Tuple 仍须先提供静态 Tuple 类型。

运行中记录 `/tmp/nessa-and-is-pattern-graph.json`，实现前冻结 C1–C10 固定
技能rubric以及 C11语义/C12 GC归档质量要求。调查后两个隔离测试作者与唯一
生产owner并行。隔离基线 `/tmp/nessa-and-is-pattern-baseline.tar`；root逐文件
比较两副本，仅6个授权生产刷新和各自新文件变化，读回断言后复制3个测试文件。
本轮可选baseline-red未执行，没有借共享构建缓存声称红测证据。

行为11组包括文档Tuple示例、计算函数/闭包/投影、独立右侧类型、null/Unit/
Unicode、链约束、左右alias、or重试、not私有绑定、trace12342/1242/142、
输入和计算结果快照、matches/for过滤及循环后捕获。14个静态负例断言诊断、
无生成函数及无法生成artifact。root边界4组验证真实三子节点与SymbolId、
关联Item i64/String默认方法、冻结receiver RHS proof40+2、初始化实际回调依赖。

GC4组共5个fixture验证计算String/Enum/Tuple、左右输入各自根引用和alias、
失败guard的逃逸capture、RHS表达式及内部guard的multishot。每例要求完成
收集至少5/4/4/8/7次、准确结果42、活动栈0；两类恢复分别20/22，trace123/
1234，串行worker日志28次完成收集。归档5组包含7个成功fixture，源码执行、
build、删除测试创建源码、新进程NSBC准确输出42；3个 malformed/重复/提前
读取负例build失败且无归档。String拼接scrutinee缺括号及边界fixture行末
guard产生分号已纠正，均为输入修正，未称为生产缺陷。

根目录实测日志 `/tmp/nessa-and-is-pattern-root-*.log`，隔离作者日志
`/tmp/nessa-and-is-pattern-behavior-final.log`、
`/tmp/nessa-and-is-pattern-memory-*-final.log` 和 memory-clippy.log。
root全workspace1408通过、0失败、1项已有忽略；fmt、workspace all-targets
check、严格Clippy和diff检查均通过。独立验收者复跑得到相同1408/0/1及全部
质量门通过；另执行7个CLI探针：表达式内部绑定不泄漏、局部变量与RHS同名
绑定不同作用域、左失败跳过副作用、for body边界、计算值准确类型拒绝、not
内部RHS不导出，以及Any动态TypeError。日志为
`/tmp/nessa-and-is-pattern-grader-*.log`。

独立首轮11/11适用项通过，无生产缺陷或评分后修复轮。C4无外部研究不适用，
C1/C2/C7/C8为实时运行记录核对，其余由产物检查或独立复跑证明。实用5个
worker角色、峰值3个并发agent、3波分派、0次重试、深度1、5个可观测worker
turns、1轮评分，分别不超过5/4/4/1/1/8/2的限制。独立评分时911秒，低于
2400秒上限。复用既有代理的独立当前任务上下文，不声称创建了5个新代理，
也没有虚构代币预算。最终diff仅补充报告与进度文档，没有提交或发布代码。

完整Nessa目标仍保持进行，包括其他模式/类型能力、泛型、完整tacit lambda、
Hash、公共trait carrier、跨包稳定身份、原生SP/FP和完整async。专项测试全绿
不能替代全设计目标审计。
