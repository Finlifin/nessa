# Any 与渐进类型 (Gradual Typing)

## 类型格上界 — Any

`Any` 是 nessa 类型格的上界，是所有类型的超类型。它用于支持渐进类型 (gradual typing) 场景——当类型信息不完整或需要与动态类型代码交互时。

```nessa
let x: Any = 42
let y: Any = "hello"
let z: Any = [1, 2, 3]
```

## Gradual Consistency

nessa 的渐进类型基于 gradual consistency 关系而非传统的子类型关系。两个类型是 gradually consistent 的，当且仅当：

- 它们相同
- 其中一个是 `Any`
- 它们的结构在已知部分一致（未知部分用 `Any` 填充）

这意味着 `Any` 不是简单的"万能类型"——它表示"类型信息未知"，编译器会在 `Any` 与具体类型之间插入运行时检查。

```nessa
fn process(x: Any) {
    -- 需要通过模式匹配或 as cast 来恢复具体类型
    x match {
        i: i32 => println("int: {i}"),
        s: String => println("str: {s}"),
        _ => println("unknown"),
    }
}
```

## 当渐进类型函数发出效应调用时

当一个参数或返回值涉及 `Any` 的函数发出代数效应调用时，效应的类型签名中的 `Any` 部分同样遵循 gradual consistency 规则。效应处理器需要能够处理运行时的实际类型。

```nessa
-- 效应定义
effect log(msg: Any) -> Unit

-- 渐进类型函数中使用效应
fn do_work(data: Any) -> #[log] Unit {
    log(data)    -- data 的实际类型在运行时确定
}
```

## Any 与类型安全

使用 `Any` 会放弃部分编译期类型安全保证，应谨慎使用。典型的合理场景包括：

- FFI 边界
- 反序列化未知结构的数据
- 插件系统等需要动态类型的场景
- 渐进迁移无类型代码到有类型代码
