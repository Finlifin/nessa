# 函数返回类型依赖推断记录

完整 Nessa 设计仍在实施。本阶段修复 Effect 参数验收中两层无注解函数留下临时 Any 的根因，独立首轮验收11/11适用项通过。

## 实现与边界

一次收集参数和返回值注解，随后按函数与变量初始化式的静态依赖迭代排序，先检查依赖，再检查消费者。模块值、函数别名、返回函数的 factory、静态投影及已知 receiver 的方法使用完整结果。词法块在外层参数和模式绑定类型建立后单独准备依赖；运行时求值顺序仍由原 NIR 控制。

函数体与值初始化式在同一事实上下文只检查一次。Trait 默认体的具体重放保存并恢复推断状态，清除重放节点原有类型、coercion 和 lowering facts，避免继承模板中的 Any assertion。

用户明确选择块内具名函数整个块前向可见。较晚的同名函数会遮蔽块内较早使用的外层函数；局部变量仍按源码顺序进入作用域。具名函数捕获外层局部值仍由既有 lowering 诊断拒绝；可执行捕获使用 lambda。Resolver 的局部绑定推断证据不能替代运行时捕获支持。

显式返回值和变量注解可截断依赖环。无注解循环函数保留现有渐进 Any；本阶段不求解循环结果方程，也不宣称根据循环内字面量得到精确返回类型。显式 Any 与真实异构结果继续保留运行时断言。

没有修改 opcode、NSBC 格式、continuation 捕获或恢复 ABI。

## 当前证据

生产隔离树 resolver 124/124，针对 resolution 的 all-target strict Clippy 通过；root 读取补丁和日志后唯一合入。独立行为14/14、归档3/3及其针对性 strict Clippy 通过，覆盖513函数三种声明顺序的签名/Call facts、关联类型与局部绑定、错误默认值无产物、真实 Any 断言、4次完成GC和删源归档。首轮测试 fixture 的 f32 类型名碰撞与无注解 global 已修正，失败日志保留；未执行 baseline-red。

Root 全 workspace 首轮失败于既有 `archive_specializes_associated_default_bodies_without_changing_explicit_any`：默认方法的内部具名函数 `identity(Item)->Item` 在 P/Q 重放时仍保留抽象签名。失败日志 `/tmp/nessa-function-inference-workspace.log` 已保留，窄修复交回原生产作者。全质量检查和最终独立验收仍待完成，尚未声称专项通过。

独立只读审查另有复现：`factory().m0()` 的 factory 返回值未标注，初始 receiver 尚未具体化，长方法链会走递归 demand 推断而栈溢出。深度64/128/256正确拒绝，512边时进程 exit -6；日志 `/tmp/nessa-function-inference-grader-deep-probe.log`。同一窄修复已要求将这种依赖也纳入迭代处理，不能增加栈限制或缩短测试掩盖问题。

修复后，内部函数头在词法/重放上下文中准备；factory结果建立后重新发现 receiver 方法依赖，并加入显式工作栈，方法 fallback 同样进入该调度器。块内扩展方法保留在其所属词法块。原实现作者实际通过 resolver127/127、原有归档104/104、扩展7/7和针对性严格lint；日志 `/tmp/nessa-function-inference-repair-{resolution,archives,extensions,clippy}.log`。独立作者补至行为16/16、归档5/5，刷新精确修复源码后首次运行通过；日志 `/tmp/nessa-function-inference-behavior-repair-first.log`、`/tmp/nessa-function-inference-archive-repair-first.log`。三种错误长方法链均检查正常exit1、诊断和无归档，正例删源后42；P/Q各自签名与显式Any均保留。

Root修复后全workspace1525通过、0失败、1项已有忽略，fmt、all-targets check、strictClippy及diff检查exit0；日志 `/tmp/nessa-function-inference-{workspace,fmt,check,clippy,diff}-repaired.log`。当时等待最终独立复跑，未提前声称验收通过。

最终独立复跑同为1525/0/1及全部质量门exit0；推断行为16、归档5、事实11组，以及原参数行为13、GC4、归档9组均通过。原始错误链和factory方法链均正常诊断拒绝且无归档；原P/Q重放回归原样通过。日志 `/tmp/nessa-function-inference-grader-{workspace,fmt,check,clippy,diff,behavior,archive,facts,probes}-final.log`。

用户授权 cargo clean，释放共享构建通道后清理 18,368 个文件共 10.7 GiB；没有清理源码或测试日志。

## 图与验收

使用 graph-engineering-workflow：只读调查先于设计，生产和测试作者隔离，root 唯一合并，独立 grader 仅接收最终文件、差异、冻结 C1-C12、日志和运行记录。graphify CLI 不可用且没有图，使用源码锚点读取。

记录 `/tmp/nessa-function-inference-graph.json`，基线 `/tmp/nessa-function-inference-baseline.tar`，生产和名称可见性补丁已保留。最多5个角色、4个并发、6个波次、1次重试、1层深度、5400秒、12次worker turn、3轮独立验收。C11 检查无环静态结果依赖与无效默认值的无产物拒绝；C12 检查事实一致性、上下文重放、cycle边界、NSBC/GC/ABI及质量检查。C4 无外部研究不适用；完整目标仍未完成。

独立首轮C1-C12中11/11适用项通过，gate accepted；C4不适用，C1/C2/C7/C8为记录核对。实际5/5角色、4/4峰值并发、5/6波次、1/1重试、1/1深度、6/12worker turn、1/3grader轮，grader实测2637/5400秒。root唯一合并，两个writer隔离；生产副本的14/3测试仅由root授权复制用于验证，与旧独立测试补丁逐字节一致；最终16/5测试与测试作者副本一致。

旧Effect参数阶段的capped记录保留，但其C11无注解函数链根因已在本阶段修复并独立复跑参数专项。无注解循环方程、泛型、精确answer类型、async、完整proof ABI、源码精确GC生命周期及其它原设计目标继续待完成；不将本专项通过替代整个项目完成。
