# tuple

元组 (tuple) 是 nessa 中的匿名积类型，用于将多个值组合在一起而无需定义命名类型。

## 构造

```nessa
let pair = (1, "hello")
let triple = (true, 42, 3.14)
let unit = ()                    -- 空元组，即 Unit
```

## 元素访问

通过 `.整数索引` 语法访问元组元素（从 0 开始）：

```nessa
let t = ("alice", 30, true)
let name = t.0               -- "alice"
let age = t.1                -- 30
let active = t.2             -- true
```

## 模式匹配解构

```nessa
let (name, age) = ("bob", 25)

(1, "hello") match {
    (0, _) => println("zero"),
    (n, s) => println("{n}: {s}"),
}
```

## 作为函数返回值

元组常用于从函数返回多个值：

```nessa
fn min_max(list: List) -> (i32, i32) {
    let mn = list.fold(i32.MAX, |a, b| if a < b { a } else { b })
    let mx = list.fold(i32.MIN, |a, b| if a > b { a } else { b })
    (mn, mx)
}

let (lo, hi) = min_max([3, 1, 4, 1, 5])
```

## 类型语法

```nessa
-- 元组类型
(i32, String)
(bool, f64, char)
()                  -- Unit
```

## 语法参考

```ebnf
tuple_construction -> (expr, expr*) | ()
tuple_type -> (expr, expr*)
```


## 当前实现与限制（2026-10-07）

构造、数字投影和静态类型已贯穿源码执行及自包含归档。支持 `(a, b)`、
单元素 `(a,)` 与嵌套元组；`()` 保持 Unit。构造元素按源码顺序仅求值一次，
在后续表达式前保存已求出的值，不因可变局部被修改而改变前面的元素。
数字投影从0开始，检查十进制索引范围、静态 Tuple 形状与元素数量。

局部 `let` / `var` 支持嵌套元组绑定及 `_`；绑定必须匹配元素数量和类型。
带显式 Tuple 注解的函数及 lambda 参数也可嵌套解构，返回值/类型别名保留完整
Tuple 形状。`var (a, b)` 使局部绑定可赋值，不表示修改原 Tuple 对象。
Module/全局可以通过单个标识符保存 typed Tuple 值，例如：

```nessa
typealias Pair = (i64, String)
const shared: Pair = (42, "hello")
fn main() { let (answer, text) = shared; println(answer); println(text); }
```

expected Tuple 类型逐元素向构造传递：窄字面量按目标表示生成，Any 元素在
相应边界动态检查。Any 值转入具体 Tuple 时检查对象身份、精确元素数量及
每个实际值的兼容性；需要元素转换时构造目标 Tuple 副本，不改写源对象的
字段或反射类型。失败产生 TypeError，隐式边界不偷偷窄化或截断浮点值。

Tuple 与 List 共用受检显示：括号表示 Tuple，单元素保留尾逗号，字符串元素
带引号/转义；支持二者混合嵌套，递归环显示 `<cycle>`。共同深度上限128层，
展开输出上限1 MiB；超限返回显式显示错误，不无限展开共享子图。
字段赋值如`t.0 = value`按接收者、右值顺序求值并检查字段类型；共享别名
观察同一次写入。同descriptor的参数检查保留身份，需要改变元素表示时才
构造目标Tuple副本。
所有元素是 TaggedValue 槽，进入GC扫描；转换期间源对象与已转换元素保留
临时根。真实收集回归验证仅由Tuple字段保留的字符串，以及暂停continuation
中的Tuple可达性。当前验证不代表完整移动GC与所有collector plan已经完成。

TypePool 使用既有 `TypeKind::Tuple` 与结构驻留，元素alias规范化后复用形状。
NSBC 保存原TypeIndex与结构来源，恢复不重新编号；构造及字段访问复用现有
NewObject/LoadField/StoreField，检查对象类型、字段类型和payload边界。无需
新增Tuple opcode、布局tag或builtin ABI。源码构造仍受4096字段及12-bit
TypeIndex/字段指令编码边界限制，超限明确诊断。

仍明确拒绝以下路径，不能把上文设计示例当作全部已实现：

- 无静态Tuple形状的动态模式分派；静态已知shape的Tuple `match` / `matches`
  已在后续Enum阶段接通，可递归包含Enum、字面量、绑定与guard。
- 没有静态shape的Any直接数字投影或解构；先通过具体Tuple类型边界检查后使用。
- Module/全局声明的Tuple解构；这些作用域目前须用单个标识符保存完整Tuple。
- List.fold等Iterator API尚未实现，上文min_max例子仍是语言设计示例。

携带值enum构造与递归Enum/Tuple模式已在后续阶段接通；复杂Or/AsBind等其余
模式、完整复合类型/trait规则与其余类型系统目标仍是后续工作。
