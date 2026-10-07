# Receiver 别名初始化依赖验收

本阶段修复默认方法中的简单 receiver 别名被误当成独立 Self 证明的问题。
`let copy=self;copy.next()` 原先可能加载未调用的 scoped provider，形成假初始化
循环。现在按具体 body 保存原 receiver 与未写入的简单复制链，沿冻结 root
选择真实依赖。receiver 自身或别名被写入时不作该推断，包括闭包内的写入。
复制边线性传播，不反复扫描逆序别名链。

## 实际执行与审查

使用 graph-engineering-workflow：独立只读调查与单一 root writer 并行，完成后
交给 fresh verifier。第一轮实际复现 `self=other` 后别名仍误标原证明的遗漏；
问题只路由到 root 修复，第二个 fresh verifier 重验整个固定 rubric。graphify
作为可选调查工具未执行：本机没有 CLI 或已有 graph，直接读源码，没有安装。
没有外部研究节点或未锚定的“安全”结论。

生产修改在 `engine/nir/src/initialization.rs`；源码回归在
`engine/test/tests/initialization_proof_tests.rs`，归档回归在
`engine/nessa/tests/archive_execution.rs`。现有 worktree 改动均保留。

全 workspace 为1229 passed、0 failed、1既有ignored。check all-targets、严格
Clippy、fmt 和 diff 空白检查通过。独立 verifier 重跑源码18项、指定归档1项
及全 workspace check/Clippy/fmt；另实际执行继承 root/body 切换、symbol
shadowing、闭包内写入和旧 mutated-self fixture。源码及删除源码后的归档都
返回42；写入后的变量和独立 Self 继续保守收集目标，并在真实循环负例中拒绝
生成产物。完整测试数量来自 root 全仓日志，不冒称 verifier 全量重跑。

固定 C1–C12 中11项适用，C4无外部研究不适用。首轮10/11，修复后第二轮
11/11，gate=passed，修复循环确实执行。C1、C2、C7、C8 使用运行记录证明，
其余适用项以产物或独立重跑为证据。第二轮没有剩余或不可达缺陷。

| 限制 | 实际 / 上限 |
| --- | --- |
| Workers（含 root） | 4 / 4 |
| 并发 | 2 / 2 |
| Waves | 3 / 3 |
| Retries | 0 / 1 |
| Nested depth | 0 / 1 |
| Worker turns（可观察预算） | 4 / 6 |
| Grader rounds | 2 / 2 |
| 时间 | 少于15分钟 / 45分钟 |

本阶段没有请求或执行 commit、push、部署、发布、支付或外部发送；测试只删除
自己的临时 fixture 源码。没有并发仓库 writer 或需要隔离后合并的产物。

日志：`/tmp/nessa-init-proof-workspace-final.log`、`check-final.log`、
`clippy-final.log`、`fmt-final.log`（后三者同 `nessa-init-proof-` 前缀）。独立
证据为 `/tmp/nessa-init-proof-recheck-source.log`、`recheck-archive.log`、
`recheck-check.log`、`recheck-clippy.log`、`recheck-fmt.log`（后四者同前缀），
额外 fixtures 结果在 `/tmp/nessa-init-proof-recheck-xh5bgi7u/results.json`。
完整现场记录为 `/tmp/nessa-init-proof-graph.json`。

## 继续工作

这个阶段没有实现精确的独立 Self 参数证明数据流、Tuple/分支/函数返回的复杂
provenance；这些情况仍保守规划，可能多计依赖。同模块初始化依旧按源码顺序。
用户已决定 Iterator 使用单次 next 与带标签结果，要求与迁移缺口另见
[迭代协议计划](tagged-iterator-plan.md)。项目总目标保持 active，未声称完成。
