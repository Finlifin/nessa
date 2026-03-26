# 动态分派 (Dynamic Dispatching)

nessa 支持通过 trait 类型参数进行动态分派，使用虚表 (vtable) 传递的方式实现。

## 基本用法

当函数参数的类型标注为 trait 而非具体类型时，调用方会自动传递虚表：

```nessa
trait Animal {
    def fn name(self) -> String
    def fn speak(self) -> String
}

trait Bird(Animal) {
    def fn fly(self) -> String
}

struct Cuckoo { ... }

impl Animal for Cuckoo {
    fn name(self) -> String = "cuckoo"
    fn speak(self) -> String = "cuckoo!"
}

impl Bird for Cuckoo {
    fn fly(self) -> String = "flap flap"
}
```

## 虚表传递

当参数类型为 trait 时，编译器自动在调用处插入虚表参数：

```nessa
fn inspect_animal(animal: Bird) = ...
-- 编译器展开为类似：
-- fn inspect_animal(animal_vtable: ..., animal: Bird) = ...
```

调用时：

```nessa
let c = Cuckoo { ... }
inspect_animal(c)
-- 编译器自动传递 Cuckoo 的 Bird 虚表
```

## Trait 继承与虚表

当 trait 有继承关系时（如 `Bird(Animal)`），虚表中包含父 trait 的方法表。传递 `Bird` 虚表时，`Animal` 的方法也可用。

```nessa
fn greet(a: Animal) {
    println("{a.name()} says {a.speak()}")
}

fn show_bird(b: Bird) {
    greet(b)              -- Bird 虚表中包含 Animal 方法，可以隐式转换
    println("{b.fly()}")
}
```
（TODO：返回类型处写trait怎么办）