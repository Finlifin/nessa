# Trait 定义

Trait 定义了一组类型必须实现的行为接口。

## 基本定义

```nessa
trait Show {
    def fn show(self) -> String
}

trait Eq {
    def fn eq(self, other: Self) -> bool
}
```

`def fn` 声明一个必须由实现者提供的方法。

## 带默认实现的方法

使用 `derive fn` 提供默认实现，实现者可以选择覆盖：

```nessa
trait Eq {
    def fn eq(self, other: Self) -> bool

    derive fn ne(self, other: Self) -> bool = not self.eq(other)
}
```

## Trait 继承

trait 可以声明父 trait 作为前置约束：

```nessa
trait Ord(Eq) {
    def fn cmp(self, other: Self) -> Ordering

    derive fn lt(self, other: Self) -> bool = self.cmp(other) == Ordering.less
    derive fn gt(self, other: Self) -> bool = self.cmp(other) == Ordering.greater
    derive fn lte(self, other: Self) -> bool = not self.gt(other)
    derive fn gte(self, other: Self) -> bool = not self.lt(other)
}
```

实现 `Ord` 的类型必须同时实现 `Eq`。

## 关联声明 (assoc)

trait 内可以定义关联类型和关联常量：

```nessa
trait Iterator {
    assoc Item: Type = Any

    def fn next(self) -> ?Item
}

trait Collection {
    assoc Element: Type = Any

    def fn len(self) -> usize
    def fn get(self, index: usize) -> ?Element
}
```

`assoc` 声明仅允许出现在 `trait`、`impl`、`extend` 的语句体中。

## Trait 内的其他定义

trait 体内也可以包含普通语句（如常量、辅助函数等），它们属于 trait 的关联作用域：

```nessa
trait Numeric {
    def fn zero() -> Self
    def fn one() -> Self
    def fn add(self, other: Self) -> Self

    -- trait 关联作用域中的辅助函数
    fn sum_list(list: List) -> Self {
        list.fold(Self.zero(), |acc, x| acc.add(x.as(Self)))
    }
}
```

## 语法参考

```ebnf
trait_def -> trait id ((expr+))? { (trait_def_fn | trait_derive_fn | statement)* }
trait_def_fn -> def fn id ((param*))? (-> expr)? (handles expr)?
trait_derive_fn -> derive fn id ((param*))? (-> expr)? (handles expr)? ((= expr) | block)
assoc_decl -> assoc id : expr = expr
```
