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
trait Stream {
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

当前支持用户声明trait的静态关联类型：`assoc Item:Type=Any`给出默认类型，
具体`impl`或局部`extend`可用同名关联声明覆盖。具体方法签名按源Item路径
替换，并检查实现函数；显式Any即使与默认类型相同也不会被替换。绑定按
实现类型、trait、可见作用域及声明owner保存到NSBC，不从其它scope猜测。

```nessa
struct Words { text: String }
trait Stream {
    assoc Item: Type = Any
    def fn next(self) -> ?Item
}
impl Stream for Words {
    assoc Item: Type = String
    pub fn next(self) -> ?String { self.text }
}
fn main() { Words { text: "hello" }.next().as(String) }
```

默认类型可以依赖源Self或同一trait及父trait的其他关联绑定，支持前向引用、
透明alias、Optional、Tuple和Function嵌套。先收集具体impl/extend的显式覆盖，
再求其余默认值；覆盖可以打断默认循环，实际未打断的循环报错。没有具体实现
的循环声明可以保留供后续覆盖。继承值来自准确可见的父实现，不重新求父默认。
源Self与显式trait名称区别保存，显式Any也不会因类型索引相同而被替换。
模板与符号声明叶保存到TPOL7，执行绑定和实际函数签名必须已具体化。

具有关联类型的默认方法按每个具体实现及作用域检查方法体，并生成独立 adapter。
Item 可用于局部类型、转换、Type 值、嵌套闭包及命名函数；字段索引和调用目标也
按具体实现保存。源 Self 参数保留实现证明，Item 参数使用普通值 ABI，即使
Item 恰好绑定为实现类型也不改变参数布局。用户覆盖的方法不实例化旧默认体。
Self 参数证明目前仅支持直接参数；参数内部嵌套的 Self 明确诊断，等待相应
carrier 协议。Item 的结构参数仍使用具体值，不因其绑定含实现类型而受此限制。

关联常量、未提供初始化表达式的声明及未绑定的动态trait视图尚未完成，当前明确
诊断；不以普通常量/typealias替代关联协议。动态限制包括Optional和嵌套函数
中的关联trait视图；关联绑定值自身含trait视图也因返回/存储carrier尚未确定而
明确拒绝。具体类型的关联返回值仍用普通受检函数ABI。
builtin Iterator 采用 `next(self)->IterationStep(Item)`，实现必须显式给出
`assoc Item:Type=...`。IntoIterator 的实现必须给出 `assoc Iter:Type=...`，
`into_iter(self)->Iter`，且 Iter 在实现的作用域满足 Iterator 义务。
声明与实现检查均保留准确类型，旧 has_next/next 源码必须迁移到带标签结果。
已保存的旧归档仍按原签名和双调用指令执行，不补造新关联声明。

```nessa
struct Counter { value: i64, end: i64 }
impl Iterator for Counter {
    assoc Item: Type = i64
    pub fn next(self) -> IterationStep(i64) {
        if self.value < self.end {
            self.value = self.value + 1
            IterationStep(i64).yielded(self.value)
        } else { IterationStep(i64).done }
    }
}
```

循环在 body 检查前绑定准确 Item，冻结词法作用域中选中的具体方法；默认
方法中的循环按各具体实现独立专化。IntoIterator 的具体返回值不携带跨作用域
证明，后续 for 在调用者作用域选择 Iterator；公共动态 carrier 仍未实现。

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


## Ordering与PartialOrd的当前契约

`std.ordering.Ordering`是普通nominal enum，三个公开variant按声明顺序为
`less`、`equal`、`greater`。Ord继承Eq，`cmp(self,other:Self)->Ordering`；
PartialOrd继承PartialEq，`partial_cmp(self,other:Self)->?Ordering`，null表示
不可比较。后者遇NaN返回null，此时`<`、`<=`、`>`、`>=`全部false。
普通运算符只消费真实enum结果，不把Ordering的运行时位模式当整数符号。

Ord的lt/gt/lte/gte普通std源码提供默认body，每个具体实现得到独立adapter；
用户覆盖通过trait槽调用生效，lte/not gt与gte/not lt沿用上例组合。证明在
receiver与另一个Self参数间正确传递，默认方法不按provider scope重选实现。

所有整数、bool、char、String、Unit支持Ord/PartialOrd；f32/f64仅PartialOrd，
不为NaN定义未经选择的总序。Type尚不提供排序，稳定类型身份的顺序仍待设计。
浮点Eq保留既有IEEE相等规则，不因PartialOrd新增而改动。

源代码的旧`cmp->i64`契约已迁移，须以Ordering variant消费结果。已存档的
旧无签名integercmp及其整数指令保持原解释，reader不注入新父接口或新签名。
