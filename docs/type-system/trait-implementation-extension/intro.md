# Trait、实现与扩展

nessa 的多态系统围绕 trait、impl、extend 和 derive 四个机制展开。

## 概览

| 机制 | 关键字 | 作用 |
|------|--------|------|
| Trait 定义 | `trait` | 定义一组行为接口 |
| 实现 | `impl ... for ...` | 为类型实现 trait |
| 直属实现 | `impl` | 向类型的关联作用域追加符号 |
| 扩展 | `extend` | 在当前作用域内为类型添加方法（局部有效） |
| 派生 | `derive` | 自动生成 trait 实现 |

## Trait 继承

trait 可以声明父 trait，要求实现者同时满足父 trait 的约束：

```nessa
trait Animal { ... }
trait Bird(Animal) { ... }    -- Bird 继承 Animal
```

实现 `Bird` 的类型必须同时实现 `Animal`。

## 本节内容

- [Trait 定义](trait-definition.md)
- [实现 (impl)](implementation.md)
- [扩展 (extend)](extension.md)
- [派生 (derive)](derivation.md)
- [动态分派](dynamic-dispatching.md)

## 语法参考

```ebnf
trait_def -> trait id ((expr+))? { (trait_def_fn | trait_derive_fn | statement)* }
trait_def_fn -> def fn id ((param*))? (-> expr)? (handles expr)?
trait_derive_fn -> derive fn id ((param*))? (-> expr)? (handles expr)? ((= expr) | block)

impl_def -> impl expr { statement* }
impl_trait_def -> impl expr for expr { statement* }
extend_def -> extend expr { statement* }
extend_trait_def -> extend expr for expr { statement* }
derive_def -> derive expr* for expr

assoc_decl -> assoc id : expr = expr
```
