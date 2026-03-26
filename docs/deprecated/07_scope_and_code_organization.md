# 作用域与代码组织 (Scope and Code Organization)

Nessa 的作用域和代码组织系统设计简洁而强大，核心理念是"一切皆类型"。

---

## 1. 作用域层次结构

Nessa 的基础作用域单元分为两类：

```
包 (Package)
  ├── 类型作用域 (Type Scope)
  │   ├── struct
  │   ├── enum
  │   ├── trait
  │   └── module (特殊类型)
  │
  └── 块作用域 (Block Scope)
      ├── 函数体
      ├── do 块
      ├── if/while 等控制流
      └── extend 块
```

### 1.1 包 (Package)

**包** 是 Nessa 代码组织的最顶层单位。

- **定义**: 包是一组相关模块的集合，对应文件系统中的一个目录
- **标准包**: `std` - Nessa 标准库
- **用户包**: 默认执行环境在 `main` 包中
- **自动导入**: 所有包默认 `use std.prelude.*`

```nessa
-- 包结构示例
my_project/
├── main/           -- main 包（默认执行环境）
│   ├── main.ns
│   └── utils.ns
└── std/            -- 标准库（内置）
    ├── prelude.ns  -- 自动导入
    ├── io.ns
    └── collections.ns
```

### 1.2 类型作用域 (Type Scope)

**类型作用域**是 Nessa 代码组织的核心。每个类型（struct, enum, trait, module）都拥有自己的作用域。

```nessa
-- 结构体作用域
struct Point {
    x: i32,
    y: i32,
}

impl Point {
    -- Point 的作用域内定义方法
    fn new(x: i32, y: i32) -> Point {
        Point { x, y }
    }
    
    fn distance(self) -> f64 {
        f64.sqrt(f64(self.x * self.x + self.y * self.y))
    }
}

-- 枚举作用域
enum Color {
    Red,
    Green,
    Blue,
}

impl Color {
    -- Color 的作用域内定义方法
    fn to_string(self) -> String {
        match self {
            Red => "red",
            Green => "green",
            Blue => "blue",
        }
    }
}
```

### 1.3 模块 (Module) - 特殊的类型

**关键理念**: 模块是一种普通的类型，专门用于组织代码。

**模块的本质**:
- 模块是一种**类型**，与 struct、enum 地位相同
- 特殊之处：**无法实例化**（不能创建模块对象）
- 用途：组织代码、命名空间、封装

**灵活的模块化**:
如果开发者愿意，可以像 Zig 一样，使用 struct 或 enum 来组织代码：

```nessa
-- 传统模块方式
mod math {
    fn add(a: i32, b: i32) -> i32 { a + b }
    fn sub(a: i32, b: i32) -> i32 { a - b }
}

-- Zig 风格：用 struct 作为命名空间
struct Math {}
impl Math {
    fn add(a: i32, b: i32) -> i32 { a + b }
    fn sub(a: i32, b: i32) -> i32 { a - b }
}

-- 甚至可以用 enum（如果需要）
enum MathOps {}
impl MathOps {
    fn add(a: i32, b: i32) -> i32 { a + b }
}

-- 使用方式相同
let x = math.add(1, 2)      -- 模块
let y = Math.add(1, 2)      -- struct
let z = MathOps.add(1, 2)   -- enum
```

---

## 2. 模块系统

### 2.1 模块定义

Nessa 的模块系统基于文件系统结构：

- **文件模块**: 单个 `.ns` 文件为一个模块
- **目录模块**: 包含 `mod.ns` 的目录为一个模块
- **模块是第一类值** (First-class value): 可以作为参数传递、赋值等

```nessa
-- 文件: utils.ns
mod utils {
    fn helper() { ... }
}

-- 文件: math/mod.ns (目录模块)
mod math {
    use .arithmetic  -- 相对导入
    use .geometry
}

-- 文件: math/arithmetic.ns
mod arithmetic {
    fn add(a, b) { a + b }
}
```

### 2.2 导入 (Use)

```nessa
-- 基本导入
use std.io
use std.collections.HashMap

-- 多重导入
use std.{io, fs, net}

-- 通配符导入
use std.prelude.*

-- 相对导入
use .sibling_mod      -- 同级模块
use ..parent_mod      -- 父级模块
use ...grandparent    -- 祖父级模块

-- 包根导入
use @package_root.mod
use @std.io           -- 显式从 std 包导入

-- 别名导入
use std.collections.HashMap as HMap
```

### 2.3 可见性

```nessa
-- 默认为 public (pub)
fn public_function() { ... }
struct PublicStruct { ... }

-- 显式 private
private fn internal_helper() { ... }
private const SECRET = 42
```

### 2.4 重新导出

```nessa
-- 重新导出子模块
use sub_mod
pub const sub_mod = sub_mod  -- 公开重新导出

-- 选择性重新导出
use internal.{foo, bar}
pub const foo = foo  -- 只导出 foo
```

---

## 3. 块作用域 (Block Scope)

### 3.1 基本块作用域

```nessa
-- 函数作用域
fn example() {
    let x = 10  -- 局部变量
    var y = 20  -- 可变局部变量
}

-- do 块作用域
do {
    let temp = compute()
    process(temp)
}  -- temp 在此处销毁

-- 控制流作用域
if condition {
    let result = expensive_computation()
    use_result(result)
}  -- result 仅在 if 块内有效
```

### 3.2 extend 作用域 (临时扩展)

`extend` 块允许在**当前作用域**临时扩展类型方法：

```nessa
struct Dog {
    name: String,
}

fn example() {
    let dog = Dog { name: "Buddy" }
    
    do {
        -- 在这个块内临时扩展 Dog
        extend Dog {
            fn fetch(self, item: String) -> String {
                "\{self.name} fetches \{item}"
            }
        }
        
        dog.fetch("ball")  -- ✅ OK，在 extend 作用域内
    }
    
    dog.fetch("ball")  -- ❌ Error: method not found
}
```

---

## 4. std.prelude - 自动导入

所有 Nessa 包默认自动执行：

```nessa
use std.prelude.*
```

`std.prelude` 包含：

```nessa
-- std/prelude.ns
mod prelude {
    -- 基础类型
    pub const i8 = builtin.i8
    pub const i16 = builtin.i16
    pub const i32 = builtin.i32
    pub const i58 = builtin.i58  -- Nessa 特有
    pub const i64 = builtin.i64
    pub const u8 = builtin.u8
    pub const u16 = builtin.u16
    pub const u32 = builtin.u32
    pub const u58 = builtin.u58  -- Nessa 特有
    pub const u64 = builtin.u64
    pub const f32 = builtin.f32
    pub const f64 = builtin.f64
    pub const bool = builtin.bool
    pub const char = builtin.char
    pub const String = builtin.String
    
    -- 常用函数
    pub const println = builtin.println
    pub const print = builtin.print
    pub const dbg = builtin.dbg
    
    -- 常用 trait
    pub const Eq = builtin.Eq
    pub const Ord = builtin.Ord
    pub const Clone = builtin.Clone
    pub const ToString = builtin.ToString
}
```

---

## 5. 执行环境

### 5.1 main 包

Nessa 程序默认在 `main` 包中执行：

```nessa
-- 文件: main.ns (在 main 包中)

-- 自动导入 std.prelude.*
-- 所以可以直接使用 println, i32, String 等

fn main() {
    println("Hello, Nessa!")
}
```

### 5.2 包路径解析

```nessa
-- 当前在 main 包中

use std.io           -- 从 std 包导入
use @main.utils      -- 显式从 main 包导入
use .utils           -- 相对导入（当前包内）
```

---

## 6. 示例：完整项目结构

```
my_nessa_project/
├── main/                    -- main 包（执行环境）
│   ├── main.ns              -- 入口文件
│   ├── config.ns            -- 配置模块
│   └── handlers/            -- 处理器目录模块
│       ├── mod.ns           -- handlers 模块入口
│       ├── http.ns
│       └── websocket.ns
│
├── mylib/                   -- 自定义包
│   ├── core.ns
│   └── utils.ns
│
└── std/                     -- 标准库（内置）
    ├── prelude.ns           -- 自动导入
    ├── io.ns
    ├── collections/
    │   ├── mod.ns
    │   ├── vec.ns
    │   └── hashmap.ns
    └── net.ns
```

使用示例：

```nessa
-- 文件: main/main.ns

-- 自动导入 std.prelude.*

use std.io
use std.collections.HashMap
use @mylib.core
use .config
use .handlers

fn main() {
    -- 使用 prelude 中的类型和函数
    let x: i32 = 42
    println("Value: \{x}")
    
    -- 使用导入的模块
    let map = HashMap.new()
    io.read_file("config.toml")
    handlers.http.start()
}
```

---

## 7. 设计哲学

1. **一切皆类型**: 模块也是类型，与 struct、enum 地位平等
2. **灵活的组织**: 可以用 struct/enum 替代 module 进行代码组织
3. **清晰的作用域**: 包、类型作用域、块作用域层次分明
4. **便利的预设**: 自动导入 std.prelude，减少样板代码
5. **First-class 模块**: 模块是值，可以传递、存储

---

## 8. 与其他语言对比

| 特性 | Nessa | Rust | Zig | Python |
|------|-------|------|-----|--------|
| 模块是类型 | ✅ | ❌ | ⚠️ (struct命名空间) | ❌ |
| 自动导入 prelude | ✅ | ✅ | ❌ | ❌ |
| struct/enum 作命名空间 | ✅ | ✅ | ✅ | ❌ |
| 包系统 | ✅ (包级) | ✅ (crate) | ✅ (包作根) | ✅ |
| First-class 模块 | ✅ | ❌ | ❌ | ✅ |

---

## 待补充

- [ ] 包管理器 (Package Manager)
- [ ] 包版本控制
- [ ] 私有作用域的更多细节
- [ ] 循环依赖处理

