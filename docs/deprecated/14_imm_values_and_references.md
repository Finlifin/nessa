# Imm 值与引用 (Imm Values and References)

## 变量绑定

*   **`let`**: 定义不可变绑定 (Immutable binding)。通常意味着变量指向的值不可被重新赋值（Rebinding），且对于值类型，其内容不可变。
    ```nessa
    let a = 10
    -- a = 20 -- Error
    ```
*   **`var`**: 定义可变绑定 (Mutable binding)。
    ```nessa
    var b = 10
    b = 20 -- OK
    ```
*   **`const`**: 

## 结构体与可变性
