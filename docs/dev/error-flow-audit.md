# 完整 Error 路径实施记录

稳定源码 TypeId、TPOL11 和本阶段完整 Error 路径均已通过独立验收。
本记录保留实施前基线与修复过程；最终结果见末节。整个 Nessa 仍未完成。

实施前根工作区 CLI 的实际基线：`error E.bad` 返回并打印 Unit，成功值42的
`get()!` 同样返回 Unit；`!E E` 成功载荷仍是未包装的 E。精确源码、输出与
当前二进制哈希记录在 `/tmp/nessa-error-flow-current-baseline/results.json`。

当前源码及运行时调查分别为 `/tmp/nessa-error-static-current-map.md`、
`/tmp/nessa-error-runtime-current-map.md`，没有用旧调查中的包依赖循环推断
当前实现。完整验收 C11-C15 在写入实现前冻结于
`/tmp/nessa-error-flow-contract.md`，公共生产者/消费者协议为
`/tmp/nessa-error-flow-api.md`；运行分派记录为 error-flow-graph.json。

三个 writer 使用各自434文件基线副本。静态 writer 独占 parser/AST/
resolution/type_pool；backend writer 独占 runtime/GC/interpreter/NIR/
codegen/NSBC/NSBC_IO；独立测试 writer 只写新的行为、GC、归档测试。root
读回和合并。共享 Cargo 使用锁定自动清理入口和单一所有者，避免重复
编译/清理冲突。

实现目标为真实128位 concrete TypeId 标签与零标签成功分支、稳定错误
集合/推断/转换、构造/传播/消除/模式、GC/continuation/闭包安全及删源
新进程 NSBC 执行。未匹配错误保留为剩余值，隐式成功保留，只有显式
后缀!提前退出；ok!是普通绑定名。Any载荷以实际类型标签并在目标集合
处检查，开放域不能被有限类型处理假装穷尽。

拟实现的三word Error wrapper 使用 VM 所有的 GC payload-role，raw128
标签不是根，仅 payload 一槽可扫描；新 opcode6D..70、writer NSBC4及
受检 legacy3 兼容、TPOL11原字节规则保留。内部 Open 域 errors=[Any]
是显式实现语义，不以 Any 的ID作为载荷标签；详细字段与安全约束见
冻结协议。此时这些仍是目标，尚无本阶段生产或测试通过的声明。

捕获/恢复 continuation 保留 delimiter 独立栈段的拆接；只有建立独立
分支时复制。Error 路径不得改回帧复制，也不得丢掉已有 scope/proof。
完整 Nessa（泛型/源码 newtype/异步/原生栈切换/FFI/包链接等）仍未完成。


旧 size0 ErrorQualified 描述符及身份作为元数据原样保留；旧的未标记载荷
行为是本阶段复现的未完成实现，不另建兼容运行时继续复制该缺陷。旧布局
的可执行使用应在安装前给明确不支持诊断。V3 普通归档仍兼容；降级检查
除了新 opcode，还需覆盖以既有转换指令/函数签名触及新 Error 布局的情况，
避免改 header 就把新能力混入旧版本。上述为验收要求，仍待实际验证。

## 隔离实现的首轮验证

backend 副本的实际 CLI 已能区分成功与错误的同类型载荷，保留未处理错误，
执行 family 模式，并显示 `error 42`。成功传播的 `get()! + 2` 最初发生
TypeError；读回类型事实发现操作数提前 LiftOk，修复 expected payload
上下文后，同一源码实际输出42。该修复仍需静态回归及最终整合检查。

独立行为测试首轮为12通过、9失败，完整日志为
`/tmp/nessa-error-backend-independent-behavior-first.log`。失败已分别交回
测试源码拼写和覆盖修正、qualified 元组载荷转换、handler 返回推断的
责任边界。不能把这些测试当作已通过，GC与删源归档独立测试仍待执行结果。

本阶段生产实现尚未合并到根工作区。最终全仓检查和新上下文独立验收仍未
进行；这些隔离结果不代表完整 Error 或完整 Nessa 已完成。

修正测试词法和普通 match 的覆盖后，行为测试为18通过、3失败；GC为6通过、
2失败；归档为6通过、2失败。行为中的外部 `matches` 绑定与既定规范冲突，
已将测试改成模式内 guard 的正例和外部引用的拒绝例。实现仍需修复元组
载荷转换与 handler 上下文推断。归档两项失败是无效输入构造的测试前提，
已修正但尚未复跑。

GC 的嵌套 delimiter 测试实际暴露 InvalidContinuation：单次使用优化只
数源码引用，漏掉 handler 被内层 effect 捕获后产生独立分支的可能性。
修复须以不发生捕获的证明为前提，不能改回 capture/resume 复制栈帧。
物理移动测试将用新进程启用 MMTk 的强制 defrag 选项，继续断言实际地址
变化、完整标签和载荷、完成的 GC epoch，以及最终栈段释放；尚无通过证据。

后续实际复验：静态 v4 的5/75/177/84项测试、check和严格Clippy通过。
独立归档8项全部通过，原始嵌套 continuation 的GC测试也已通过；物理移动
仍未通过。读回GC实现发现所有普通根都进入 pinning roots work，因此即使
强制defrag也不会移动直接根对象。需要迁移到可更新槽位，并同步修复枚举、
字段、集合、效应调用等分配路径跨GC保留旧副本的问题。

独立内存审查同时发现根回调的共享引用写入来源，以及 native `arg()` 的
值保留契约。可移动根必须提供有效的写回来源；宿主无法更新的复制引用须
有明确的固定或根句柄策略，不能仅改扫描入口就声称支持移动GC。上述修复
仍在隔离副本进行，最终全仓和独立验收未完成。

## 整合与最终验证

静态28个文件与后端39个文件已合入根工作区。合并前逐文件检查434文件
基线，三个责任范围互不重叠；生产实现应用后67个SHA256均与封存产物相同。
独立测试作者已在刷新依赖后的隔离树实际复跑行为21项、GC10项、归档8项，
全部通过；根工作区全仓检查与新上下文验收仍待完成。

普通托管根现在以独占可写槽位交给collector，native无法更新的复制引用只在
builtin生命周期内固定。`BuiltinRoot`提供可移动槽位，旧token及跨调用句柄
拒绝。日志记录Error wrapper的4次真实地址变化，以及native句柄8次移动和
7次失效拒绝。分配压力测试在集合增长、枚举与字段转换调用内部记录8次完成
收集；这不等于每一个分配点都发生过收集，具体边界由独立审查记录。

单次continuation优化现在要求handler体不发生其他调用或隐式分派，避免内层
effect捕获handler后重复使用已消费句柄。capture/resume仍只拆接栈段；clone
承担独立分支复制。完整项目仍未完成，本节不作为最终验收结论。

## 独立验收结果

最终77个阶段文件的补丁在新434文件基线应用、逐文件读回与反向检查通过。
新上下文验收者独立执行全workspace格式、编译、测试与严格Clippy，全部通过：
1753项通过、0失败、1项已有ignored。行为21项、删源归档8项、强制移动GC
10项，以及分配内部收集、栈段、continuation回收与单次优化专项均通过。

控制者检查逐项证据后计算14/14适用项通过；C4因没有外部研究而不适用，
C11-C15全部通过。C1、C2、C7、C8属于运行记录证据，其余由产物检查或实际
独立执行支持。证据为 `/tmp/nessa-error-grader-result.json`、
`/tmp/nessa-error-grader-evidence.json` 与对应grader日志，根质量记录为
`/tmp/nessa-error-root-quality.json`。原失败与较早审查结论均保留为历史。

实际验证支持移动后的标签、载荷、guard顺序和多次恢复，不声称每一次分配
都触发了收集。解释器capture/resume只拆接已有栈段，fork复制独立分支。
Native硬件SP/FP切换、泛型、源码newtype、异步、FFI与跨包链接仍未完成。
