# 代码风格 (Code Style)

## 命名规范

### 蛇形命名法 (`snake_case`)

以下标识符使用蛇形命名法：

- 模块名
- 变量名
- 属性名（struct 字段）
- 函数名
- 参数名
- 代数效应名
- 包名
- 非 Error 用途的 enum variant

### 大驼峰命名法 (`PascalCase`)

以下标识符使用大驼峰命名法：

- 类型名（`struct`、`enum`、`trait`、`newtype`、`typealias`）
- Error 用途的 enum variant

### 示例

```nessa
enum HttpError {
    Timeout(time: Duration),
    ConnectionReseted,
}

enum IpAddr {
    v4(a: u8, b: u8, c: u8, d: u8),
    v6(addr: String),
}
```

Error enum 的 variant 使用 `PascalCase`（如 `Timeout`、`ConnectionReseted`），因为它们代表错误类别，语义上类似类型名。而普通 enum 的 variant 使用 `snake_case`（如 `v4`、`v6`），因为它们更接近值构造器。

### 更多示例

```nessa
-- 模块名：snake_case
mod string_utils { ... }

-- 函数名、参数名：snake_case
fn parse_config(file_path: String) -> Config { ... }

-- 类型名：PascalCase
struct HttpClient { ... }
trait Serializable { ... }
typealias UserId = u64

-- 代数效应名：snake_case
effect read_line() -> String

-- 包名：snake_case（体现在 package.toml 和文件系统中）
-- package.toml: name = "my_web_server"
```

## 缩进与格式

- 使用 4 个空格缩进，不使用 tab
- 花括号 `{` 不换行（K&R 风格）
- 单表达式函数体优先使用 `=` 语法

```nessa
-- 推荐：单表达式用 = 语法
fn double(x: i32) -> i32 = x * 2

-- 推荐：多语句用 block
fn process(data: String) -> Result {
    let parsed = parse(data)
    validate(parsed)
    transform(parsed)
}
```

## 注释

```nessa
-- 单行注释使用双横线

{-
  多行注释使用花括号横线对
  可以嵌套
-}
```

## use 语句组织

`use` 语句放在文件顶部，按以下顺序分组，组间空一行：

1. 标准库导入
2. 外部依赖导入
3. 包内模块导入

```nessa
use std.collections.{Map, Set}
use std.io.{read_file, write_file}

use http_lib.{Client, Request, Response}
use json.parse

use @config.Settings
use .utils.validate
```
