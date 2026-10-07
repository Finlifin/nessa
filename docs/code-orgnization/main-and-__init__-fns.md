# main 与 \_\_init\_\_ 函数

## main 函数

对于 `exe` 类型的包，入口文件 `src/main.ns` 中必须定义一个 `main` 函数作为程序入口：

```nessa
fn main() {
    println("hello, nessa")
}
```

目录编译要求入口文件中的本地 `main` 为零参数函数；嵌套模块的函数、导入的
同名函数和非函数绑定不能代替它。当前启动 ABI 不传实参，也不求值默认实参，
所以含默认参数或变参的 `main` 同样被拒绝。返回类型不作额外限制。

`lib` 和 `tmp` 包执行加载所需的初始化，不调用声明的 `main`。没有初始化工作的
库可以没有启动入口，直接运行或运行其归档均正常结束。

`main` 函数是程序执行的起点。在 `main` 被调用之前，所有被引用到的模块的 `__init__` 函数已经按依赖顺序执行完毕。

## \_\_init\_\_ 函数

每个类型（包括模块）都可以定义一个 `__init__` 函数，用于在关联作用域加载完成后执行初始化逻辑：

```nessa
mod config {
    global settings: Map = Map.new()

    fn __init__() {
        settings.insert("log_level", "info")
        settings.insert("max_connections", "100")
    }
}
```

### 签名约束

`__init__` 的签名固定为 `fn()`——无参数、无返回值：

```nessa
fn __init__() {
    -- 初始化逻辑
}
```

### 执行时机

`__init__` 在其所属类型的关联作用域加载完成后自动执行。执行顺序由依赖关系决定：如果模块 A 依赖模块 B，则 B 的 `__init__` 先于 A 的 `__init__` 执行。

### 典型用途

- 初始化全局状态或配置
- 注册工厂函数或插件
- 执行运行时检查或断言

```nessa
struct Logger {
    global instance: ?Logger = null

    fn __init__() {
        instance = Logger.with_level("info")
    }

    fn with_level(level: String) -> Logger { ... }
}
```
