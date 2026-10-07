# List 模式与 rest 验收

完成文档的 `[patterns]` 和 `...id`，用于 match/matches/for。静态 List 或
Any 输入，运行时检查实际 List 身份及准确/最小长度；固定元素 Any、具名
rest List。rest 单个、任意位置，前缀/后缀从同一快照提取，空中间段合法。
用户已明确确认浅快照和单个 rest：槽位独立，引用对象共享，rest 创建新 List。
每次 List 节点尝试独立建快照，先类型/长度检查，再复制槽位，最后执行字段
guard；原输入 alias 保留原wrapper，失败后下个 arm/备选观察当前原List。

parser原rest错误消费两个点和任意模式，修为文档三个点+标识符。名称收集
包含List固定字段与rest，复用or预分配SymbolId，准确类型和重复绑定检查。
rest为显式binder，独立于同名外部变量。不可提前读后面的rest，not/matches
的不导出规则、and/is按遇到顺序绑定均保留。匿名rest、多个rest、List以外
位置和声明/参数解构明确拒绝，静态错误无执行函数或artifact。

新增 resolution/list_patterns.rs 专管List模式类型/shape，新增
nir/list_patterns.rs 专管 List 身份/长度、槽位快照和rest范围复制。复用既有
TypeCheck、List长度/索引/push和NewList，范围cursor在帧内，无新增builtin、
VM opcode、continuation ABI或NSBC格式。旧“List模式未实现”单元测试改为
准确的不兼容输入诊断，仍验证静态i64不能用List模式。

运行中记录 `/tmp/nessa-list-pattern-graph.json`，基线
`/tmp/nessa-list-pattern-baseline.tar`。调查先于图设计，C1–C10固定技能rubric
和C11/C12语义/GC归档质量要求先于编辑冻结。两个隔离测试作者与root生产
实现并行；root唯一合并。逐文件比较副本，只允许9个授权生产路径（2个新
模块）和各自新测试；3个worker测试读回后复制。可选baseline-red未执行。

行为10组覆盖所有rest位置/空段/长度边界、动态List身份与嵌套List/Enum/
null/Unit/Unicode、缩短/扩展/覆盖、下一arm新快照、rest独立槽位及Cell共享、
组合模式、trace12342单次求值、matches/for及返回捕获。20个静态负例断言
明确诊断、无生成函数及无法生成artifact。root边界4组覆盖真实rest AST与
准确Any/List类型、透明List别名、默认Item i64/String、冻结Self guard proof
40+2和实际回调初始化软依赖。

GC5组最低完成收集5/4/4/9/9次，全部结果42、活动栈0，worker串行日志31次
完成收集。覆盖嵌套heap前后缀/rest捕获、独立slots及Cell共享、失败guard
旧rest捕获与下个arm新快照。归档6组含10个成功fixture：源码执行、build、
删除测试创建源码、新进程NSBC准确输出42；4个静态负例build失败无归档。
包括上述捕获、null、类型、关联Item、scoped Self guard、初始化和multishot。

首次for multishot fixture错误假设堆迭代器也被分叉复制，实际诊断20/0、
visits2/bodies2/trace11。continuation-stack-abi.md:36明确堆引用保持共享，
root按此证据判定为输入假设修正，保留失败/debug日志；现case严格断言共享
游标20/0和恢复后head/rest检查2次，另加fold帧内cursor控制20/22、visits3/
bodies3/trace111。没有将两种状态混为一谈或深复制堆改变ABI语义。
root raw resolver AST测试移除无源码std时不存在的len方法，准确类型断言
保持；driver层guard/len仍被其他测试覆盖。另修正一个算术续行fixture语法。

专项与质量日志在 `/tmp/nessa-list-pattern-root-*.log`，隔离作者最终日志在
`/tmp/nessa-list-pattern-behavior-refreshed.log` 和
`/tmp/nessa-list-pattern-memory-*-final.log`。root全workspace1433通过、0失败、
1项已有忽略；fmt、workspace all-targets check、严格Clippy和diff检查均通过。
独立首轮11/11适用项通过，无产品修复轮。C4无外部研究不适用，C1/C2/C7/C8
为实时运行记录核对，其余来自产物检查或独立复跑。独立评分时2061秒，低于
2400秒上限；5个worker角色、峰值3个并发、3波、0次worker重试、深度1、
1轮评分，分别不超过5/4/4/1/1/2限制。中断后恢复验收额外计一次可观测turn，
最终6/8；验收者读取较早5/8记录时的结论保留，该差异未隐藏或改变预算。
未提交或发布代码。

首次独立workspace运行因写入配额耗尽退出，分离日志确认 QuotaExceeded，
随后连沙箱只读命令也无法建立mount。失败日志保留；用户明确授权cargo clean，
清理仅 `/tmp/nessa-not-pattern-memory-baseline-target` 的可再生构建缓存，
移除3567个文件、1.3GiB，源码/测试/日志/基线均保留。恢复后独立workspace
1433/0/1和List归档6/6通过，日志
`/tmp/nessa-list-pattern-grader-recovered-{workspace,archive}.log`；并非将失败
重标为成功。其它独立质量门在源码未变时复用，最终diff另复查通过。

整项目目标保持进行。声明/参数模式、匿名rest、其余模式/类型设计、泛型、
完整tacit lambda、Hash、公共trait carrier、跨包稳定身份、原生SP/FP和完整
async未因本轮完成而视为实现，不把专项测试代替全设计完成审计。
