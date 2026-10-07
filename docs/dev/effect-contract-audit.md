# Effect 恢复输入与 handler 退出契约

本阶段修复 effect 的恢复输入和普通 handler 的退出检查；完整 Nessa 目标仍未完成。
不引入新的 Continuation 类型语法，不强制异构 continuation answer 统一。

## 实现与边界

声明的 effect 返回类型决定 continuation 恢复输入。前端对无写入的 catch 绑定、
稳定别名和 clone 传播输入期望，包括闭包捕获。任何位置写入该绑定都会保守取消
此静态精化；普通 Continuation 参数、函数返回、字段和 Any 保留运行时检查。
这不是完整的流敏感 continuation 类型系统。

每个 handler 有独立的 return/resume 推断与检查边界，普通 handler 的所有提前
退出、guarded 退出和尾值按 effect 返回类型检查。嵌套 lambda/handler 保持自己的
返回上下文，外层函数推断/转换不再遍历独立 handler 的退出。捕获 handler 的
结果继续按已有 join 规则推断，保留返回 continuation、再次捕获和多次恢复。

语言 wrapper 的弱所有者登记附带输入 TypeIndex 和捕获点 scope；clone 复制
该契约。所有语言恢复先在冻结 scope 下检查/转换输入，再 fork 或消费；错误
不改变模板或栈池，且恢复调用点的查询 scope。临时根保护 wrapper、输入及可能
分配的转换。原始线性 ABI 保持显式无类型契约的内部路径。

输入来自已有 TypeKind::Effect.ret，在源/归档安装后捕获时重新建立，无新 opcode、
wrapper payload、类型元数据格式或 NSBC 版本。标准库 Continuation 的透明别名
现在也能用于字段取值后的 clone。

精确 answer 类型精化、完整 effect 传播义务、effect 参数默认/具名/变参协议和
其它原始设计目标继续待完成；未用本专项代替它们。

## 验证

- Runtime 隔离作者7组：生产 dispatch 从 Effect.ret 取得输入，无效 multishot/once
  不增长或消费链，随后有效恢复；clone/未知句柄、heap/null/Unit、窄数字转换、
  冻结 trait scope 成功/失败/clone 后恢复调用点 scope。实际完成 GC 有 epoch 断言。
  `/tmp/nessa-effect-contract-runtime-contract-tests-final.log`、`runtime-clippy-final.log`。
- 独立测试作者9组：known catch/alias/clone/captured closure 静态拒绝、普通
  handler 各退出、nested 返回边界、有效 narrow/optional/Unit、擦除/可变/返回值
  及字段 runtime TypeError、已有 Any 参数与 tail/guard 检查、heap delayed clone、
  再次捕获和 i64 输入/String 分支/i64 handler answer。15次实际完成 GC、结果42、
  活动栈0。`/tmp/nessa-effect-contract-behavior-final.log`。
- 归档3组：4个成功 fixture 删源后新进程执行42，5个静态错误无产物，5个
  动态错误在源码和删源 NSBC 执行中均为 TypeError。
  `/tmp/nessa-effect-contract-archive-final.log`。
- Root 全 workspace 1465通过、0失败、1项已有忽略，fmt、all-targets check、
  strict Clippy、diff whitespace 通过。日志
  `/tmp/nessa-effect-contract-{workspace,fmt,check,clippy,diff}.log`。

首轮测试发现 clone provenance 使用了错误 AST kind Property（实际为 Projection），
以及字段 std Continuation 别名的 clone 被 exact TypeIndex 检查误拒。均由 root
修复，原测试不变，失败日志保留。最初 module 顺序 fmt 失败已修复。

上轮 actual-movement 测试因进程级 MMTk 和并行测试的堆/钉住状态波动而失败；
精确单测与全套重跑成功。root 将该测试隔离到 fresh child process，仍要求 wrapper
和 String 确实移动，并要求子进程实际跑了1项通过测试。保留原失败证据，未弱化
移动断言。未执行 baseline-red。

## 并行与验收

使用 graph-engineering-workflow，先只读 investigator 后冻结实现 rubric。
runtime 与 behavior/archive 两个 writer 在隔离副本并行，root 独占 resolver
实现并唯一合并；独立 grader 只接收最终产物、基线、diff、rubric、运行记录、日志。
记录 `/tmp/nessa-effect-contract-graph.json`，基线 `baseline.tar`，差异 `integrated.patch`。
已启动独立验收与修复重评循环，最多2轮；首轮11/11适用项通过，无未解决缺陷。
C11固定为静态输入与 handler 退出边界，C12固定为全语言恢复、失败所有权、
GC/异构结果/归档与质量门。C1/C2/C7/C8为运行记录，C4无外部研究不适用。

独立复跑全 workspace 同为1465/0/1，runtime7/7、behavior9/9（15次完成GC）、
archive3/3，所有质量门 exit 0；日志
`/tmp/nessa-effect-contract-grader-{workspace,fmt,check,clippy,diff,runtime,behavior,archive,artifacts}.log`。
附加 CLI probe 验证 mutable alias 从 i64 输入替换为 bool 输入后能正确返回42、
链式 clone 和嵌套 handler 错误拒绝、shadowed lambda 参数与 guarded 窄返回值成功。
closure 内赋值并不改写外部捕获绑定，验收 probe 的初始相反预期已按独立 scalar
捕获实验纠正，失败记录保留。

本阶段4/5角色、峰值3/4并发、2/4波次、0/1重试、1/1深度、3/9派发 worker
turn、1/2验收轮，最终未超过3600秒限制。C1/C2/C7/C8为记录核对，其余为
产物检查或独立复跑。
