# Type — 类型的类型

在 nessa 中，类型本身也是值，其类型为 `Type`。`Type`类型不可动态构造。

## 基本概念

```nessa
-- 类型可以作为值使用
let t: Type = i32
let u: Type = String

-- 类型可以作为参数传递
fn size_of(T: Type) -> usize { ... }
size_of(i32)
```

`Type` 是 nessa 类型系统中的一种特殊类型——它是所有类型的类型。当你写下 `i32` 时，它既是一个类型（用于类型标注），也是一个 `Type` 类型的值（用于值上下文）。

## 与 typealias / newtype 的关系

虽然类型是一等公民，但定义类型别名或新类型时应使用专门的语法：

```nessa
-- 推荐
typealias Id = u64
newtype Meters = f64

-- 不推荐（虽然合法）
const Id: Type = u64
```

详见 [newtype 与 typealias](newtype-and-typealias.md)。

## view 语法获取类型

可以通过 `'type` view 在运行时获取值的类型：

```nessa
let x = 42
let t = x'type    -- t: Type, 值为 i32
```

`type_of(x)` 与 `x'type` 使用同一动态查询规则，不读取变量的静态注解。
类型别名返回其目标的类型身份；`i64'type` 的值是 `Type`。

null 是 Optional 的空值，其动态类型规定为 `?NoReturn`，与 `Unit`（`()`）不同。
这是对原先未规定的 null 反射行为的补全；`?NoReturn` 表示只可能包含 null
的 Optional 底类型。

## 编译期结果类型工厂

`IterationStep(Item)` 接受一个位置类型参数，产生具有准确 Item 载荷的
类型值。可执行类型的 Item 必须在编译期已知且具体；运行时 `Type` 变量不能
作为参数。trait 声明可使用关联 Item 或源 `Self` 建立编译期模板，默认类型、
默认方法与关联别名按所选 impl/scope 专化。未专化的模板不能成为运行时 Type
值或可执行签名。工厂本身不是运行时 Type 或函数，不能赋值或传递。

```nessa
typealias Factory = IterationStep
typealias Step = Factory(i64)
let result: Step = Step.yielded(42)
let ended: Step = Step.done
```

工厂别名与导入保持绑定身份；同名用户声明遵循普通绑定规则。
`result'type == IterationStep(i64)`；不同 Item 的结果类型保持独立身份，
普通同名 enum 不获得结构结果身份。`yielded(null)` 是一个元素，
`done` 才是结束；Any Item 的允许值包含 null。
具体结果可构造、匹配、反射和归档。关联 Item 与源 `Self` 模板已支持准确
专化；单步 Iterator、for 与 List 快照迭代已接入，动态关联 carrier 仍待实现，参见
[实施计划](../dev/tagged-iterator-plan.md)。
