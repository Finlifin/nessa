# 代数效应 (Algebraic Effects)

代数效应提供了一种结构化的方式来处理副作用和控制流。

## 定义 Effect

```nessa
effect yield(value: Any)
effect repository(key: String) -> Repository
```

## 发出 Effect

使用 `#` 符号发出 effect 或传播 effect。

*   `effect_call(...)#`: 发出并等待处理。
*   `fn name(...) -> #Effect T`: 函数签名声明可能会发出 Effect。

```nessa
fn iter_num(range: Range) -> #yield i32:
    for i in range:
        yield(i)#
```

## 处理 Effect (Handlers)

Handlers 动态地在调用栈中查找。

1.  **就地处理**: `expr #: ...`
    ```nessa
    iter_num(1..10)#:
        println("value: {}", _)
    ```

2.  **Lexical Handler**: `let handles ...`
    ```nessa
    fn main():
        let handles repo = Repo() -- Repo implementation handles 'repository' effect
        render()#
    ```

3.  **块级 Handler**:
    ```nessa
    handles yield(value):
        println("Got: {}", value)
    
    computation()
    ```

4.  **Type-bound Handler**:
    ```nessa
    struct Repo:
        fn provide(self, key: String) -> Repository handles repository:
             ...
    ```

注意：Nessa 目前没有 Effect 多态 (Effect Polymorphism)，Effect 的检查是局部的或动态的。
