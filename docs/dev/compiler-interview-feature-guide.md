# 编译岗位面试：Nessa 语言特性讲解指南

这份文档用于把 Nessa 现有设计文档整理成一套更适合编译工程岗位面试的讲法。核心原则不是“列出所有亮点”，而是优先挑那些能够把**语言设计**、**Resolution 决策**、**IR lowering** 和 **运行时语义** 串起来的特性。

## 最推荐主讲的 3 个特性

### 1. 渐进类型系统（`Any` / Gradual Typing）

- 对应文档：
  - [Any 与渐进类型](../type-system/any-the-type.md)
  - [类型系统总览](../type-system/intro.md)
  - [静态与动态决策过程](static-and-dynamic-decision-procedures.md/intro.md)
- 为什么优先讲：
  - 最容易建立“静态路径 + 动态回退路径”的总体世界观
  - 能自然带出类型格、类型检查、运行时类型检查和动态分派
  - 这是 Nessa 区别于纯静态 toy language 的关键点
- 面试时应该强调：
  - `Any` 不是“万能类型”，而是“类型信息未知”
  - 编译器在类型已知时走静态路径，在涉及 `Any` 时保留动态回退路径
  - 这种设计把 FFI、插件化、渐进迁移和类型安全放进同一个框架里

### 2. 代数效应（Algebraic Effects）与 evidence passing

- 对应文档：
  - [代数效应总览](../algebraic-effect-and-multi-prompt-delimited-continuation/intro.md)
  - [Evidence-Passing 编译策略](../algebraic-effect-and-multi-prompt-delimited-continuation/evidence-passing-compilation.md)
  - [动态效应调用](../algebraic-effect-and-multi-prompt-delimited-continuation/dynamic-effect-call.md)
- 为什么优先讲：
  - 这个主题辨识度高，能体现 Nessa 不只是语法实验
  - 能展示“源码层 effect 调用”如何在编译阶段被显式化
  - 能和渐进类型自然连接：静态可知时走 evidence passing，涉及 `Any` 时退化为动态查找
- 面试时应该强调：
  - effect 的“声明”和“处理”解耦
  - Resolution 阶段决定一条调用是静态 handler 传递还是动态查找
  - lowering 之后，隐式能力会变成显式 evidence 参数

### 3. NIR（Normalized Intermediate Representation）与 lowering

- 对应文档：
  - [NIR 设计](normalied-intermediate-representation/intro.md)
  - [系统架构设计](architecture.md)
- 为什么优先讲：
  - 这是最容易被面试官顺着深挖编译工程能力的部分
  - 能把前两个特性统一收束到 compiler pipeline
  - 能清楚说明“高层语义如何被标准化成简单 IR”
- 面试时应该强调：
  - NIR 位于 Resolution 与 Codegen 之间
  - NIR 的工作是脱糖、控制流标准化、隐式行为显式化
  - `for`、`when`、`?/!`、effect call、lambda、pattern matching 都会在这一层被统一处理

## 可作为第四顺位补充的特性

### 模式匹配 / 控制流脱糖

- 对应文档：
  - [控制流与模式匹配](../control-flow-and-pattern-matching/intro.md)
  - [NIR 设计中的模式匹配编译](normalied-intermediate-representation/intro.md)
- 适合作为补充而不是主线的原因：
  - 容易讲明白，也能体现 lowering 思维
  - 但如果时间有限，它更适合作为 NIR 的一个代表性案例，而不是独立主轴

## 不建议作为第一顺位主讲的主题

- GC / scheduler / safe-point
  - 很亮眼，但容易被继续追问运行时实现细节
  - 更适合作为“我还有 runtime 设计储备”的扩展点
- 字节码格式 / VM 指令
  - 工程味很强，但更偏后端实现
  - 单独拿出来讲，不如前 3 个特性更能体现“语言设计 + 编译落地”的闭环
- 结构化并发
  - 主题吸引人，但如果面试重点是编译实现，这个话题容易把重心带到调度器和运行时

## 推荐讲解顺序

1. 先讲渐进类型
2. 再讲代数效应与 evidence passing
3. 再讲 NIR / lowering
4. 最后用一句话补 runtime（如 TaggedValue、VM、safe-point）

这个顺序的好处是：先建立“静态 vs 动态”的决策框架，再展示 effect 如何被编译器显式化，最后把这些特性统一收束到 IR 设计。

## 一个安全的讲法模板

每个特性都按同样的 4 个问题回答：

1. 为什么需要这个特性
2. 它在语言层怎么表现
3. 编译器在哪个阶段处理它
4. 当前实现到了什么程度，哪些还在演进

这样讲既不会停留在语法层，也不会一上来就陷入实现细节。

## 三个主讲特性的 2 分钟回答模板

### 渐进类型

> 我会先讲 Nessa 的渐进类型系统。它不是把 `Any` 当成一个随便塞值的动态桶，而是把 `Any` 视为“类型信息未知”的上界类型。这样一来，编译器在大多数已知类型的路径上仍然可以做静态解析，但只要跨到 `Any` 边界，就自动切到动态回退路径，比如运行时 `TypeCheck`、动态字段访问或者动态 effect dispatch。这个设计对我来说很关键，因为它把静态语言的性能和可维护性，跟 FFI、插件系统、渐进迁移这些动态需求放进了同一个类型框架里。实现上，这个问题主要落在 Resolution 阶段的静态/动态判定，以及 NIR 中把这些隐含分支显式化。

### 代数效应与 evidence passing

> 我第二个会讲代数效应。因为它最能体现我不是只设计语法，而是在设计一套可编译的语义模型。Nessa 里 effect 的声明和处理是解耦的，源码上看是 `#` 传播和 handler 消除，但编译器不会把它一直保留成高层抽象。对于静态已知的 effect，Resolution 会给函数注入 evidence 参数，把 handler 传递变成普通参数传递；只有在遇到 `Any` 或无法静态确定签名的时候，才退化成运行时动态查找。这样讲的重点不是“effect 很酷”，而是“高级控制流怎样落到一个成本可控的编译策略上”。

### NIR / lowering

> 我第三个会讲 NIR，因为这是最能体现完整编译器思维的部分。NIR 位于 Resolution 和 Codegen 之间，它的目标不是做炫技优化，而是把高层语法统一成简单、规则的语义单元。比如 `for` 会脱糖成迭代器循环，`?/!` 会展开成匹配分支，effect 调用会显式化为 evidence 传递，模式匹配会编译成 decision tree，lambda 会做闭包转换。这样后面的 codegen 不需要理解所有表面语法，只要消费一个规范化 IR 即可。对面试官来说，这通常也是最容易继续追问 lowering、控制流图和后端设计的入口。

## 面试时的收束句

如果只能用一句话总结，最值得讲的不是“语法有多丰富”，而是：

- 渐进类型负责建立静态与动态的边界
- 代数效应负责展示高级语义怎样被编译成显式机制
- NIR 负责把整个语言设计收束成统一的编译管线

这三个点最能体现 Nessa 的语言设计价值和编译工程含量。
