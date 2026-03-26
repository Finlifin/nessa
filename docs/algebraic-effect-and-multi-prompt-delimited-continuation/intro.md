# 代数效应与多提示分界续延 (Algebraic Effects & Multi-Prompt Delimited Continuations)

代数效应 (Algebraic Effects) 是 nessa 中处理副作用的核心机制。与传统的 monad 或 async/await 不同，代数效应将**效应的声明**与**效应的处理**彻底解耦，使得代码可以在不关心具体实现的情况下声明"我需要某种能力"，而由调用者决定如何提供这种能力。

## 设计哲学

nessa 的代数效应设计遵循以下原则：

1. **与错误处理的对称性**：效应传播 (`#`) 与错误传播 (`!`) 在语法和语义上高度对称，降低学习曲线
2. **效率优先**：默认采用 in-place handler application 策略，避免不必要的 continuation 捕获开销
3. **渐进的控制能力**：简单场景下开箱即用，需要精细控制时可显式捕获 continuation
4. **与并发的自然整合**：async effect 是 nessa 并发模型的唯一入口，取代了传统的 async/await fn

## 核心概念

```
效应声明 ──→ 效应调用 ──→ 效应传播 ──→ 效应消除
 effect      expr#       fn -> #eff T   expr# { ... }
                                        expr.use(handler)
```

- **效应声明** (`effect`)：定义一种副作用的签名
- **效应调用与传播** (`#`)：发出或向上传播一个效应
- **效应消除**：通过消除块 `# { ... }` 或 handler application `.use()` 处理效应
- **capability**：类型实例作为 handler 的能力
- **evidence passing**：编译期 handler 传递优化
- **multi-prompt delimited continuation**：显式捕获 continuation 的高级控制流

## 本章内容

- [代数效应](algebraic-effect.md)：效应的声明、调用、传播与消除语法
- [Capability](capability.md)：类型实例作为 handler 的能力模式
- [Evidence-Passing 编译](evidence-passing-compilation.md)：编译器的 evidence 参数传递策略
- [动态效应调用](dynamic-effect-call.md)：渐进类型下的动态 handler 查找
- [In-Place Handler Application](in-place-handler-application.md)：默认的高效 handler 执行策略
- [Multi-Prompt Delimited Continuation](multi-prompt-delimited-continuation.md)：显式 continuation 捕获与高级控制流

## 与其他机制的关系

| 机制 | 传播符号 | 消除方式 | 类型限定 |
|------|---------|---------|---------|
| Error | `!` | `! { ... }` | `!ErrType T` |
| Effect | `#` | `# { ... }` / `.use()` | `#EffType T` |
| Optional | `?` | `match` / `else` | `?T` |

代数效应与错误处理共享相同的结构——它们都是限定类型 (qualified type)，都使用后缀运算符传播，都使用消除块来处理。这种对称设计使得 nessa 的控制流机制保持高度一致。
