# or / as 组合模式验收

当前实现接通 match、matches 与 for 的组合模式，而非在 lowering 临时拼接绑定。
pattern_bindings 收集成功路径的名字，检查 or 两边相同名称集合、分支内部重复绑定
及 as 的标识符要求。备选分支在名称阶段共用 SymbolId，但遇到绑定前不会发布到
作用域。左右分支之间恢复原绑定表，guard 不会读取另一分支或后面字段的未赋值槽。

共享 enums::pattern 逐分支检查并保存每个 binding occurrence 的准确类型，然后
比较 canonical type；不能依靠被后分支覆写的 symbol.type_index 推断类型一致。
as 使用当前位置整个输入的类型，Any 输入保持 Any。名称作用域沿用原 arm、matches
临时作用域和 for 作用域。声明或参数的组合模式明确诊断，避免进入仅支持不可失败
绑定的 tuples::bind_pattern。函数参数及声明的完整模式目标仍需继续实现。

NIR branch 以左失败块作为右入口，共用成功出口；as 仅在子模式成功后赋 alias。
外层 guard 失败不重试已成功的备选模式，分支内部 guard 失败可以继续右分支。
原 scrutinee snapshot、Enum/Tuple 检查后提取、单次 next 和 for 跳过失败均复用。
局部类型与 trait proof 绑定仍走 tuples::bind_pattern，无新增 NIR、指令或归档布局。

隔离作者只新增 composite_pattern_tests.rs，8组验证标量/Unicode/null/Unit、嵌套
Enum/Tuple、部分失败重绑定、准确 alias/窄整数、源码求值一次、guard trace、闭包、
matches 作用域、for 跳过及错误类型/绑定不产物。root 读回文件，比较隔离基线 tar
确认原文件未改，然后复制该新文件并复跑。作者首次未设置共享 target 导致生成隔离
目录自己的 target，后改用共享 target；没有将构建输出当作源码改动或成功证据。

root 另有5组边界回归：同名绑定与 alias、错误集合/类型/上下文和 guard 可见性、
i64/String 关联默认 adapter 与 Fn 返回反射、跨 scope trait receiver alias 冻结
证明，以及真实 GC/guard 内 continuation。GC 至少5次，保存 alias Enum 与 String
字段，左右恢复分支各21，合计42；最终活动栈0。CLI测试先运行源、build、删源、
新进程执行，检查左右捕获、for、单次输入及 matches。错误输入 CLI build 失败
且没有创建 nsbc。共享 ABI/TPOL/NSAM/NSBC revision 和旧字节码解释没有改变。

本轮使用 graph-engineering-workflow：只读调查与 root 源码阅读并行，隔离测试
作者与 root GC/归档测试并行，root 唯一合并者；graphify 无 CLI/现成图未运行。
运行中追加记录为 /tmp/nessa-composite-pattern-graph.json，root日志使用
/tmp/nessa-composite-pattern-前缀。root 全仓1313通过、0失败、1项已有忽略；fmt、all-targets check、严格Clippy及
diff检查均通过。独立验收第1轮12/12通过，gate为passed；另行复跑全仓和全部质量门结果一致，
没有产品修复轮或重试。独立日志使用/tmp/nessa-composite-grader-前缀，focused
日志另记录额外CLI负例、嵌套失败重绑定和guard闭包。自定义Iterator筛选1/3，
产出合计4，4次yielded及1次done恰好next5次。检查fixture曾使用有歧义的直接
struct iterable和当前不支持的字段compound assignment；改为提前绑定iterable
与普通字段赋值后通过，没有修改源码或降低断言。
未请求或执行提交、推送、部署、发布、删除项目内容、付款或对外发送。

本轮接通 or/as 静态模式路径。not、List/struct 模式、穷尽性分析、公共动态 trait
carrier、泛型、跨包身份、continuation 原生 SP/FP 与其它项目目标仍未全部完成。

验收C1/C2/C7/C8根据运行中追加记录核对，属于record/attested；其余项由独立
读取或重跑验证。参与者4/4、并发峰值2/4、3波/4、重试0/1、深度1/1、worker
turn4/6、验收轮1/2，耗时未超过2400秒；预算单位可直接观测。独立比较365个
隔离原文件均未改，新测试与集成文件字节一致。没有未通过、无法达到或不适用
的标准；此次验收仅覆盖所列组合模式目标，整个Nessa目标保持active。
