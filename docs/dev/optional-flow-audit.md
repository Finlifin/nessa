# Optional 传播与非空操作实施记录

完整 Nessa 设计仍未完成。当前范围是传播、非空模式、直接 Optional.unwrap，以及按用户决定删除 null 消除块语法。当前用户明确修订的范围已独立通过。旧消除块范围从未通过。

## 实际基线与核心修复

Root CLI 基线 `fn get()->?i64{42};fn read()->?i64{get()?};fn main(){println(read())}` 错误打印 `()`；`read()->?i64{let missing:?i64=null;missing?;42}` 错误返回42。源码与日志为 `/tmp/nessa-option-propagation-baseline.ns`、`/tmp/nessa-option-propagation-baseline.log`、`/tmp/nessa-option-null-exit-baseline.ns`、`/tmp/nessa-option-null-exit-baseline.log`。不声称完整baseline-red套件已运行。

resolver原来将OptionPropagation透传，NIR缺少分支并返回Unit。当前实现求值并保存操作数一次，非null保留准确inner载荷，null退出当前函数、lambda或handler；返回推断纳入隐式null退出。显式非nullable返回及跨inline默认值边界拒绝且无产物，嵌套callable的退出仍归自身。Any载荷保持Any，透明alias、窄数值、heap/function与具体Self/Item重放遵循既有转换。NoReturn作为底类型不抹去可完成分支的具体结果。

some模式在match/matches/for中先排除null，递归检查inner并保持guard/as/or/not/and-is/List快照作用域。Optional直接unwrap保留原载荷，null走已有panic；普通用户unwrap与Any动态方法保持分派，静态Optional绑定方法函数值明确未支持。本专项不声称Error.unwrap或动态trait carrier已经实现。

## 用户明确删除的语法

原规范只展示 `value? { return null }`，正常完成语义未定。用户回复“把这个删了吧，容易让人误解，且多余”；root确认删除整个 `value? { ... }` 形式，保留单一后缀传播、some模式和unwrap。Optional规范的消除章节删除，普通及括号内消除块明确诊断拒绝，if/while条件的 `optional_bool? { ... }` 仍将花括号解释为控制块。

变更发生在grader第0轮。旧task/rubric保存于graph的prior_scope且从未通过；新C11/C12按明确指令重新冻结。共同实现曾在隔离树通过18行为/4GC/5归档，未合并root且已撤回，patch/log历史保留。原消除块肯定测试仅因用户撤销该能力而移除，改为明确拒绝/noartifact断言；其它core语义断言保留。旧scope行为13/14、GC3/3（18次完成收集）、归档3/4中两项消除块失败的日志仍保留：`/tmp/nessa-optional-flow-behavior-fourth.log`、`/tmp/nessa-optional-flow-gc-first.log`、`/tmp/nessa-optional-flow-archive-second.log`。旧scope或撤回实现的通过结果不计入新scope验收。

## 隔离与验证

使用graph-engineering-workflow，readonly investigator先于设计，root唯一合并，生产/测试隔离，Cargo共享通道单独分配。最终grader只接收冻结rubric、最终文件、基线、scoped diff与实际日志。graphify当前CLI不可用且无既有图，使用rg源码锚点。记录 `/tmp/nessa-optional-flow-graph.json`，基线 `/tmp/nessa-optional-flow-baseline.tar`，地图 `/tmp/nessa-next-design-map.md`。上限5角色、4并发、6波次、2重试、1层、5400秒、12次worker turn、3轮grader。

Root读回并合并12个授权Rust路径，原共同实现的AST/control-flow/NIR类型分支全部未合入。最终生产resolver132、parser73、NIR22项与目标Clippy、格式通过，日志 `/tmp/nessa-optional-final-core-{unit,clippy,format}.log`。测试作者最终source精确刷新后新scope14行为、3GC（18次完成收集）、3删源归档全部通过；日志 `/tmp/nessa-optional-flow-removed-core-first.log`、`/tmp/nessa-optional-flow-removed-archive-first.log`，targetedClippy/格式通过。Root再次核对基线仅12source与3test授权变化，合并3newtests。最终root workspace1552通过/0失败/1已有忽略，fmt、workspace all-targets check、strictClippy及diff均exit0；日志 `/tmp/nessa-optional-flow-root-{workspace,fmt,check,clippy,diff}.log`。独立grader复跑workspace1552/0/1、全部质量门及专项14/3/3一致通过，并检查实际基线、逐字隔离来源和18路径补丁重建。额外探针核对while条件、混合数值推断、inferred lambda、Any some模式、删掉语法的空格/括号形式及正确语法的未使用enum默认值边界；首次enum探针语法写错已保留但不计语义证据。日志 `/tmp/nessa-optional-flow-grader-{workspace,fmt,check,clippy,diff,core,archive,probes,isolation,artifacts}.log`。本scope首轮11/11适用项通过，C4不适用，C1/C2/C7/C8为运行记录核对；历史rubric浅拷贝错误仅修root元数据并保留记录，当前criteria未改。完整项目仍未完成。

Hash公共契约、泛型、Error tagged representation、精确answer/proof carrier、原生SP/FP、async及其它原设计目标继续待落实。
