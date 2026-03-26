# 扩展 (extend)

`extend` 允许在当前作用域内为类型添加方法或实现 trait，但其效果仅限于当前作用域及其子作用域。

## 基本用法

```nessa
extend String {
    fn is_blank(self) -> bool = self.trim().len() == 0
    fn repeat_n(self, n: u32) -> String { ... }
}

-- is_blank 和 repeat_n 仅在当前作用域内可用
"  ".is_blank()    -- true
```

## 为外部类型实现外部 Trait

`extend` 最常见的用途是规避孤儿规则——当你既不拥有类型也不拥有 trait 时：

```nessa
-- 假设 Show 来自外部包，Vec2 也来自外部包
-- impl Show for Vec2 { ... }  -- 编译错误：违反孤儿规则

-- 使用 extend 在本地作用域内实现
extend Show for Vec2 {
    fn show(self) -> String = "Vec2({self.x}, {self.y})"
}

-- 仅在当前作用域内，Vec2 拥有 Show 实现
println(my_vec.show())
```

## 作用域限制

`extend` 的效果严格限定在当前作用域内，不会全局地修改目标类型的关联作用域。这保证了：

- 类型的行为不会被远处的代码意外改变
- 不同模块可以为同一类型提供不同的 `extend`，互不冲突
- 代码的可预测性和可维护性

```nessa
mod a {
    extend i32 {
        fn double(self) -> i32 = self * 2
    }
    -- 这里可以用 42.double()
}

mod b {
    -- 这里不能用 42.double()，因为 extend 在 mod a 的作用域内
}
```

## extend vs impl

| 特性 | `impl` | `extend` |
|------|--------|----------|
| 作用范围 | 全局（整个程序） | 仅当前作用域 |
| 孤儿规则 | 受限 | 不受限 |
| 适用场景 | 类型或 trait 的所有者 | 为外部类型添加本地便捷方法 |

## 语法参考

```ebnf
extend_def -> extend expr { statement* }
extend_trait_def -> extend expr for expr { statement* }
```
