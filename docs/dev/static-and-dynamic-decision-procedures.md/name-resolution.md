# 名称解析 (Name Resolution)

名称解析是 Resolution 阶段的第一步，负责将源码中的每个标识符绑定到其定义点。

## Type ↔ Scope 双射

Nessa 的核心设计原则：**作用域与类型是双射关系 (bijection)**。每个命名类型天然拥有一个关联作用域，而每个作用域也对应一个类型。不存在独立于类型的"模块"概念——`mod` 只是一种没有实例化能力的类型，与 `struct`、`enum` 在作用域能力上完全等价。

```
Type ↔ Scope 双射:
  mod math     → math 是类型，math.sin 是其作用域中的函数
  struct Point → Point 是类型，Point.origin 是其作用域中的函数
  enum Color   → Color 是类型，Color.red 是其作用域中的 variant

三者的唯一差异在于实例化能力，作用域能力完全一致：
  mod    → 不可实例化的类型
  struct → 可实例化，拥有字段
  enum   → 可实例化，拥有 variant
```

## 作用域树

名称解析的核心数据结构是作用域树 (Scope Tree)。由于 Type ↔ Scope 双射，作用域树本质上就是类型嵌套树。每个作用域持有一组名称绑定和对父作用域的引用。

```
作用域层次:

  PackageScope                       ← 包级（所有依赖包的导出符号）
  └── TypeScope (root: mod)          ← 根类型 (main.ns / lib.ns 对应的 mod)
      ├── TypeScope (utils: mod)     ← 子类型（文件 utils.ns 对应的 mod）
      │   └── FunctionScope (helper)
      │       └── BlockScope (if)
      │           └── BlockScope (while)
      ├── TypeScope (User: struct)   ← struct 类型 = 作用域
      │   ├── FunctionScope (new)
      │   └── FunctionScope (validate)
      ├── TypeScope (Color: enum)    ← enum 类型 = 作用域
      │   └── FunctionScope (is_primary)
      └── FunctionScope (main)
          └── BlockScope (for)

注意: mod/struct/enum 在树中统一为 TypeScope，
      不存在单独的 ModuleScope。文件系统映射只是
      自动为每个 .ns 文件创建一个 mod 类型。
```

## 作用域节点结构

```
ScopeNode:
  id:           ScopeId
  parent:       Option<ScopeId>
  kind:         ScopeKind           -- Package | Type | Function | Block
  bindings:     HashMap<Name, DefId>
  uses:         Vec<UseDecl>        -- 本作用域的 use 语句
  visibility:   Visibility          -- 本作用域的默认可见性
```

## 查找算法

标识符解析遵循逐层上溯规则：

```
resolve(name, current_scope):
  1. 在 current_scope.bindings 中查找 name
     → 找到则返回 DefId
  2. 在 current_scope.uses 中查找匹配的 use 声明
     → 找到则解析 use 目标路径，返回 DefId
  3. 如果 current_scope.parent 存在:
     → resolve(name, parent)
  4. 否则:
     → 报 "undefined identifier" 错误
```

特殊情况：
- 函数作用域不上溯到同级其他函数（函数间不互相可见局部变量）
- 类型作用域中的 `self` 绑定到当前类型实例（仅 struct/enum，mod 无 self）
- 类型作用域中的 `Self` 绑定到当前类型本身

## Use 语句展开

`use` 语句在名称解析阶段处理，按以下规则展开：

```
use net.http.Client
  → 在当前作用域插入: bindings["Client"] = DefId(net.http.Client)

use net.http.{Client, Server}
  → bindings["Client"] = DefId(net.http.Client)
     bindings["Server"] = DefId(net.http.Server)

use net.http.*
  → 遍历 net.http 的导出符号，逐个插入 bindings

use net.http.Client as HttpClient
  → bindings["HttpClient"] = DefId(net.http.Client)
```

### 路径解析

路径中的每一段按以下规则解析：

| 前缀 | 起始作用域 |
|------|-----------|
| (无) | 当前作用域 |
| `.` | 父作用域 (类似 super) |
| `@` | 包根作用域 |

```
use @db.driver.postgres
  → 从包根 → 解析 db 模块 → driver 子模块 → postgres 子模块

use .sibling.helper
  → 从父作用域 → 解析 sibling → helper
```

## 可见性检查

每次名称解析成功后，检查可见性约束：

```
check_visibility(def, access_site):
  match def.visibility:
    pub      → 通过
    default  → 检查 def 和 access_site 是否在同一个包内
    private  → 检查 access_site 是否在 def 的定义作用域内
```

违反可见性约束时报诊断错误。

## 投影解析 (Projection Resolution)

由于 Type ↔ Scope 双射，`.` 投影运算符统一处理所有成员访问——无论目标是 mod、struct 还是 enum，都是同一个操作：进入类型的作用域查找符号。

```
Point.origin
  → 解析 Point → TypeDef(Point, struct)
  → 进入 Point 的作用域，查找 origin
  → 返回 DefId(Point::origin)

math.sin(x)
  → 解析 math → TypeDef(math, mod)
  → 进入 math 的作用域，查找 sin
  → 返回 DefId(math::sin)
  → 与 Point.origin 的查找过程完全一致

net.http.Client.new(url)
  → 解析 net → TypeDef(net, mod)
  → 进入 net 的作用域 → 查找 http → TypeDef(http, mod)
  → 进入 http 的作用域 → 查找 Client → TypeDef(Client, struct)
  → 进入 Client 的作用域 → 查找 new
  → 每一步的 . 都是同一个操作: 进入类型作用域

value.distance(other)
  → 推断 value 的类型为 Point
  → 进入 Point 的作用域，查找 distance
  → 绑定自动附加 self 参数
```

### impl 和 extend

`impl` 块向类型的作用域追加符号，与类型定义体内的符号地位完全相同：

```
impl Point { fn scale(...) }
  → 将 scale 插入 Point 的作用域 bindings
  → 从此 Point.scale 可解析
```

`extend` 块也向类型作用域追加符号，但**仅在当前作用域内有效**，不全局修改目标类型：

```
extend String { fn is_blank(self) -> bool { ... } }
  → 仅在当前作用域及子作用域内，String.is_blank 可解析
  → 离开当前作用域后不可见
  → 这保证类型的作用域不被远处代码污染
```

## 遮蔽 (Shadowing)

同一作用域内不允许重复定义。子作用域可以遮蔽父作用域的绑定：

```nessa
let x = 1
do {
    let x = 2     -- 遮蔽外层 x
    print(x)      -- 输出 2
}
print(x)          -- 输出 1
```

局部类型名遮蔽同名外部依赖包（因为 mod 也是类型，所以本质上是类型名遮蔽包名）。

## 文件系统映射与 Type ↔ Scope

文件系统模块发现（见 fs-module-discovering.md）的本质是：每个 `.ns` 文件自动定义一个 `mod` 类型。目录结构映射为类型嵌套关系。

```
src/main.ns         → 自动定义 root mod 类型
src/utils.ns        → 自动定义 mod utils 类型，作为 root 的子类型
src/net/mod.ns      → 自动定义 mod net 类型
src/net/http.ns     → 自动定义 mod http 类型，嵌套在 net 作用域内

等价于手写:
mod root {              -- main.ns
    mod utils { ... }   -- utils.ns
    mod net {           -- net/mod.ns
        mod http { ... } -- net/http.ns
    }
}
```

## 产出

名称解析完成后产出：
- **SymbolTable**: 所有标识符到 DefId 的映射
- **ScopeTree**: 完整的类型嵌套层次结构（= 作用域树，因为 Type ↔ Scope 双射）
- **UseResolutions**: 所有 use 语句的解析结果
- **VisibilityErrors**: 可见性违规的诊断信息

这些数据作为后续类型解析和 effect 解析的输入。