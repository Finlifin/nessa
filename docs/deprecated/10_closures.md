# 闭包 (Closures)

闭包在 Nessa 中使用十分自然，尤其配合尾随闭包语法。

## 语法形式

1.  **标准形式**: `|args| expr` 或 `|args| { block }`
2.  **`do` 关键字**:
    ```nessa
    list.map do |x|
        x * x
    ```
3.  **Tacit (点位符) 语法**: `_`
    
    Tacit 语法仅对在函数定义时标记为 `lambda` 的参数有效。

    ```nessa
    -- 定义：注意 lambda 关键字
    fn map(list: List, lambda f: fn(Any) -> Any) -> List: ...

    -- 调用：使用 _ 占位
    list.map(_ * 2) 
    -- 等价于
    list.map(|x| x * 2)
    ```

## 灵活性

以下形式等价：

```nessa
list.map(|x| x * x)
list.map() do |x| x * x
list.map do |x| x * x
```
