# use 语句与路径 (Use Statement & Paths)

`use` 语句用于将其他作用域中的符号引入当前作用域。nessa 的路径语法完全基于投影运算符 `.`，与类型成员访问语法一致。

## 基本语法

```nessa
use net.http.Client
use net.http.{Client, Server, Status}
use net.http.*
```

## 路径解析规则

### 解析优先级

当写下 `use foo.bar` 时，nessa 按以下顺序解析 `foo`：

1. 在当前作用域中查找名为 `foo` 的符号（局部模块、类型等）
2. 若未找到，尝试从当前包的依赖中查找名为 `foo` 的包

这意味着本地定义的模块名会遮蔽同名的外部依赖。

### 特殊路径前缀

| 前缀 | 含义 | 示例 |
|------|------|------|
| （无） | 从当前作用域开始解析 | `use utils.helper` |
| `.` | 从父作用域开始（类似 `super`） | `use .sibling_module.foo` |
| `@` | 从当前包根开始 | `use @net.http.Client` |

### 示例

```nessa
-- 从当前作用域解析
use utils.string_helpers.trim

-- 从父模块引用兄弟模块
use .sibling.some_fn

-- 从包根开始的绝对路径
use @db.driver.postgres.connect

-- 引用外部依赖包中的符号
-- 假设 package.toml 中声明了依赖 "com.example/http_lib"
use http_lib.Client
```

## 批量导入与通配符

```nessa
-- 导入多个符号
use net.http.{Client, Server, Request, Response}

-- 导入模块下所有公开符号
use net.http.*
```

## 重命名导入 (as)

```nessa
use net.http.Client as HttpClient
use db.driver.postgres as pg
```

## 重导出 (pub use)

`pub use` 将导入的符号重新导出，使其成为当前模块公开 API 的一部分：

```nessa
-- 在 net/mod.ns 中
pub use .http.Client
pub use .http.Server

-- 外部使用者可以直接：
-- use net.Client
```

这在构建库的公开接口时非常有用，可以将深层嵌套的符号提升到更方便的路径上。

## 路径语法总结

```ebnf
path ->
    id |                        -- 简单标识符
    path . path |               -- 投影
    path . { path* } |          -- 批量投影
    path . * |                  -- 通配符投影
    . path |                    -- 父作用域（super）
    @ path |                    -- 包根
    id as id                    -- 重命名绑定
```
