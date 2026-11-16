# Nessa 编译系统全面增强路线图与设计规范

本文档定义 Nessa 编译系统四大核心增强方向的设计依据、架构边界、分波次（Wave E1–E5）实施计划与发布验收门禁：

1. **各层编译报错全面优化**：跨文件多标签渲染、结构化错误码体系、消除级联误报与重复诊断、完整的声明处溯源标注（secondary labels）。
2. **启发式语法/语义错用报错**：针对其他主流语言（Rust / Python / JS·TS / C·C++ / Go / Lua）肌肉记忆及 Nessa 特有语法契约（无色函数 `fn` vs `async effect`、后缀 `match`、`and`/`or`/`not`、`++` 拼接、`!` 错误传播、`?` 可空传播、`.await`、`const` vs `let`/`var`）提供精准启发式识别与一键可执行的 `help:` 修复建议，并内置全局 Damerau-Levenshtein 相近标识符提示。
3. **类型推理能力大幅加强**：无注解 `null` 局部变量的延迟赋值流推断（`let/var x = null; x = 42` -> `?i64`）、控制流敏感类型窄化（Smart Casts：`x != null`、早退守卫、`when`、`matches` 类型测试与非空赋值窄化）、同构集合元素与键值流向追踪（`List` / `Set` / `Deque` / `Map` / `HashMap` 在 `.map` / `.filter` / 归纳 `.fold` / `for` 中的零 `.as(T)` 自动推断）、局部未标注 Lambda 调用点反向参数推断、`#Effect` 双向期望剥离、以及目标类型驱动的 `.variant` 枚举简写与 `usize` 区间边界转换（无返回注解的递归函数环按既有渐进类型契约保留 `Any`，显式标注环上锚点即可静态收敛）。
4. **AOT 3 级优化级别（`O1` / `O2` / `O3` + `O0` 基线）与现代编译 Pass 管线**：端到端贯通 CLI（`-O0`/`-O1`/`-O2`/`-O3`/`-O`/`--opt-level`）、`Driver`、`ArtifactCache` 与 NIR/Codegen 优化器，涵盖纯度与副作用分析、常量折叠与代数化简、拷贝/常量传播、稀疏条件常量传播（SCCP）、支配树全局值编号与路径条件精化（GVN/CSE）、跨模块词法作用域安全的小函数内联、活跃变量分析与死赋值消除 + 局部槽位着色压缩（DCE + Slot Coloring）、全模块死函数消除（DFE）、自然循环不变量外提（LICM）、自尾递归循环化消除（TRE）及后端指令窥孔、存储转发与紧凑跳转布局。

---

## 一、模块职责与边界约束

| 子系统 | 核心模块 | 职责边界 |
| --- | --- | --- |
| `engine/diagnostic` | `lib.rs`, `emitter.rs`, `suggest.rs` | 跨文件 `ariadne` 源码缓存渲染、结构化错误码分段（`1xxx`–`6xxx`）、Damerau-Levenshtein 相似度搜索与 `help`/`note` 构建器 |
| `engine/lexer` | `lexer.rs`, `error.rs` | 词法错误携带多条 `notes`/`helps`；识别外来注释 `//`、`/*`、单引号多字符字符串 `'abc'`、花括号 Unicode 转义 `\u{...}`、全角标点与缩进混用 |
| `engine/parser` | `parser.rs`, `error.rs`, `heuristics.rs`, `basic.rs`, `expr.rs`, `statement.rs`, `definition.rs`, `pattern.rs` | 语句/块边界恐慌恢复（单文件多语法错误报告）、未闭合括号起点标注、完整跨语言与 Nessa 语法错用启发式检测 |
| `engine/resolution` | `suggestions.rs`, `flow_narrowing.rs`, `collection_inference.rs`, `inference.rs`, `typing.rs`, `name.rs`, `methods.rs`, `structs.rs`, `enums.rs`, `arguments.rs`, `traits.rs` | 消除多分支重复检查与级联报错；全符号/字段/方法/变体/标准库导入建议；语义错用启发式提示；强化双向与数据流类型推断 |
| `engine/nir` | `opt/{mod,purity,cfg,const_fold,copy_prop,sccp,gvn,inline,licm,tailrec,dce,dfe}.rs` | 非破坏性、严格保持求值顺序/除零溢出异常/词法作用域与 GC 安全点的多级中端优化管线 |
| `engine/codegen` | `lib.rs`, `opt.rs`, `validation.rs` | 常量池去重、同基本块 Store-to-Load 转发、落空跳转消除与紧凑条件分支编码，严格维护 `method_call_scopes` 与 `safepoint_pcs` 双射 |
| `engine/driver` & `engine/nessa` | `driver/src/{lib,artifact_cache,package}.rs`, `nessa/src/main.rs` | `OptLevel`（`O0`/`O1`/`O2`/`O3`）配置、CLI 参数解析、构建缓存键隔离与端到端编排 |

---

## 二、分阶段实施波次与状态（Waves E1–E5）

| 波次 | 范围 | 状态 |
| --- | --- | --- |
| **Wave E1** | `diagnostic` 跨文件渲染与相近名推荐 + `lexer` / `parser` 错误恢复与启发式语法错用诊断 | 已完成并验证 |
| **Wave E2** | `resolution` / `nir` / `driver` 全层多 Span 溯源报错、级联错误抑制与启发式语义错用诊断 | 已完成并验证 |
| **Wave E3** | 类型推理能力全面增强（`null` 延迟推断、控制流敏感类型窄化、同构容器元素追踪、局部 Lambda 参数推断、`#Effect` 双向剥离、目标类型 `.variant` 枚举简写、`usize` 区间转换） | 已完成并验证 |
| **Wave E4** | AOT 优化级别（`O0` + `O1`/`O2`/`O3` 三级优化）与完整现代编译 Pass 管线实现 | 已完成并验证 |
| **Wave E5** | 集中端到端集成测试矩阵、文档同步与全仓质量门禁验证 | 已完成并验证 |

---

## 三、发布验收门禁（Release Gate Checklist）

1. **诊断精准度与零级联噪声**：跨文件标签完整展示源码上下文；单文件多处独立语法错误一次性准确报告；未定义符号错误不触发下游 `for`/`match` 级联误报；所有新增启发式报错均给出可执行的正确写法建议。
2. **类型推断完备性与渐进兼容**：`let/var x = null; x = 42`、控制流 `?T -> T` / `Any -> T` 窄化、`[1, 2, 3].filter(|x| x > 1).map(|x| x * 2).fold(0, |a, b| a + b)`、未标注局部闭包、`#Effect` 载荷期望与 `.variant` 枚举简写均精确推断出静态类型，且异构 `Any` 与无注解递归环 100% 保持渐进类型兼容。
3. **AOT 三级优化语义等价与可观测收益**：同一程序在 `-O0`、`-O1`、`-O2`、`-O3` 下运行结果、副作用顺序、错误传播（`!`）、代数效应与延续捕获（`#` / `catch k`）、异步任务取消与异常陷阱（除零、溢出）完全一致；生成的 NSBC 指令数、局部槽位数与冗余函数数随优化等级单调下降且全部通过 `nsbc::validate_artifact` 与 Tier-1/Tier-2 JIT 校验；`-O3` 尾递归消除在深递归输入下保持 $O(1)$ 调用栈深度。
4. **全仓工程门禁**：`fmt --all -- --check`、`check --workspace --all-targets`、`clippy --workspace --all-targets -- -D warnings`、`test --workspace` 与 `git diff --check` 全部零错误零警告通过。
