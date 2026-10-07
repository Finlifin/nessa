# 双变参扩展调用实施记录

完整Nessa目标继续未完成。本阶段按函数设计补齐两个调用者变参槽：List children与Map properties，只有extended application使用这一打包协议。生产、独立测试、全workspace与独立验收均已通过，本专项首轮11/11适用项通过。

## 实际基线与契约

Root实际CLI用 `fn gather(...children:List,...properties:Map)->i64{children(0).as(i64)+properties("answer").as(i64)};fn main(){println(gather{40,answer:2})}`，exit1，报Map参数不符合List-only变参及callee非struct；并非文档预期42。源码/日志为 `/tmp/nessa-dual-variadic-baseline.ns`、`/tmp/nessa-dual-variadic-baseline.log`。只读地图 `/tmp/nessa-dual-variadic-investigation.md`，核心缺口包括注解、layout、ExtendedCall的类型/NIR分派、推断与初始化的Call-only路径及具体默认体重放。没有运行完整baseline-red测试。

用户明确选择重复属性按源码顺序全部求值，最后一个值覆盖前一个。callee/receiver先求值，随后children/property value按原混合顺序各一次并快照，然后创建独立List/Map容器；key是源码标识符对应的字符串，不读取同名变量。隐式self和runtime注入的effect catch沿用既有调用边界，不属于这两个调用者槽。支持已知函数、模块/import别名、literal lambda、静态/实例/trait/具体scoped默认方法及effect调用；仅有FnType的值仍缺packing元数据，不伪造声明身份；普通间接Fn调用仍按物理签名显式提供完整List/Map，不将普通固定两容器参数函数误判为dual。普通括号dual调用及非法/未使用layout明确拒绝，无产物。dual enum不在此函数协议支持范围，既有singleList enum保持。

List/Map角色按canonical metadata识别，透明alias保持；List在前、Map在后，不能有其它调用者参数。Struct的extended构造路径仍依据真正type身份。匿名Object仍是独立缺口，不将嵌套Object冒充Map。trait的MapVariadic已有type_pool variant与NSBC byte4，source producer需要按checked layout生成；无新增tag/opcode/archive revision。capture/resume仍分离/重接delimiter栈段，不复制调用帧。

## 并发与验证计划

使用graph-engineering-workflow。只读investigator先于设计；root读回map、规范和CLI失败后冻结C1-C12，生产/测试写入两个独立/tmp副本，root唯一合并。生产独占共享Cargo，测试只在最终source刷新后取得构建通道。grader仅接收最终artifact/rubric/live log/baseline/scoped diff。graphify当前command-v exit1，无既有图，使用rg锚点。

基线 `/tmp/nessa-dual-variadic-baseline.tar`，运行记录 `/tmp/nessa-dual-variadic-graph.json`。上限5角色、4并发、6波次、2重试、1层、5400秒、12个worker turn、3轮grader；当前调查和两个writer已交付，root已合入，独立grader待最终质量门。C11检查源码协议/实参次序/准确事实与诊断；C12检查Self/Item重放、init/callback依赖、真实GC/multishot、删源新进程NSBC及workspace质量门。C4外部研究不适用，C1/C2/C7/C8运行记录核对。

所有实现、专项测试、全workspace与独立验收均待最终产物和实际日志，不把准备好测试算作通过。完整泛型、Hash公共契约、Error表示、精确answer/proofcarrier、async、跨包身份及其它原目标继续待落实。

初步隔离CLI日志 `/tmp/nessa-dual-variadic-probes.log` 已由root读回：empty/duplicate/lambda/方法/trait默认/effect及packed初始化回调例子得到42；非法ordinary/unused/unknownmetadata有明确诊断。这些并非合入或独立通过。独立作者准备14行为、3GC（minimum5/11/5完成收集）、3归档，尚未Cargo；root已读回具体断言，并要求将深链升级为真实多条ExtendedCall边与extended factory返回receiver的事实，避免将旧普通Call覆盖冒充新路径完成。

## 最终合入与专项实测

Root读回15个生产路径，逐文件核对当前root仍与基线相等后合入；测试作者
刷新同一生产patch，保持其它414个基线文件，独立执行后由root合入3个新测试文件。
生产交付 `/tmp/nessa-dual-variadic-production.patch`，测试交付
`/tmp/nessa-dual-variadic-tests-final.patch`，各自scope/hash记录可复核。

专项行为15/15、GC3/3、归档4/4通过，日志分别为
`/tmp/nessa-dual-variadic-tests-behavior-final.log`、
`/tmp/nessa-dual-variadic-tests-gc-sequential-final.log`、
`/tmp/nessa-dual-variadic-tests-archive-final.log`。串行GC实际完成5/11/5共21次，
各例返回42且active stacks=0。准确参数计划检查children索引[0,2]、重复key
的两个source位置[1,3]及canonical List/Map参数类型；深链有513条真正
函数ExtendedCall边，两种声明顺序都推断Bool并拒绝未使用的i64默认值。
factory源码通过create{}取得P，再调用实例扩展方法，构造与方法结果分开断言。
归档成功例build后删源再用新进程执行；Any显式擦除的scoped trait权限仍由
TypeError拒绝，源执行和归档一致，不声称打包恢复proof carrier。

初始不可变测试14/3/3在生产树只通过13/14行为和2/3GC（归档3/3）；独立
作者的新深链/factory夹具区分Struct ExtendedCall和函数结果；GC夹具将已知
P值从bare Continuation的Any答案显式标注为P，未改收集次数/42/零残留栈断言。
第一失败日志保留 `/tmp/nessa-dual-variadic-initial-integration.log`、
`/tmp/nessa-dual-variadic-tests-first.log`，不能以新通过结果隐去旧失败。

初始化依赖仅精确追踪未写入绑定、静态List索引和无转义String键的打包回调，
重复属性只追踪最终选中回调；所有值表达式本身仍求值。未知/动态/被修改的
容器、selector或逃逸流保留现有runtime global guards；不声称完整aggregate
数据流分析。新职责拆为resolver extended/variadics模块，复用既有call plan、
默认体replay与NIR call降级，不建立旁路权限表。

Root最终全workspace1579通过、0失败、1项已有忽略；fmt、check --all-targets、
strictClippy和git diff --check均exit0。对应日志
`/tmp/nessa-dual-variadic-root-{workspace,fmt,check,clippy,diff}.log`。最终21路径
patch与manifest为 `/tmp/nessa-dual-variadic-integrated.patch`、
`/tmp/nessa-dual-variadic-final-paths.json`。独立grader尚待复跑和逐项验收；
全部Nessa设计目标仍保持未完成。

## 独立验收结果

独立grader复跑workspace1579/0/1、fmt/check-alltargets/strictClippy/diff均exit0；
专项15/3/4通过，GC实际5/11/5，各例42/零活动栈。另九个CLI探针验证
混合求值快照、重复属性全部执行、factory实例调用、合法间接两容器调用、
修改容器别名后的回调不制造假init依赖，以及非法布局/普通dual调用/未知
extended函数值/dual enum的明确拒绝和无归档。独立按基线核对21路径hash、
两个隔离树和patch重建一致。详见
`/tmp/nessa-dual-variadic-grader-verdict.json` 及同前缀grader日志。

Root根据逐项binary verdict计算首轮11/11适用项通过；C4不适用，C1/C2/C7/C8
为运行记录核对，artifact/rerun项独立验证。运行上限内结束，最终记录
`/tmp/nessa-dual-variadic-graph.json`。以上通过只针对本专项；完整项目继续
未完成，前述泛型、Hash/Error、动态carrier、async与原生栈等缺口仍保留。
