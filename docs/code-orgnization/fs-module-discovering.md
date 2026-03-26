# 文件系统模块发现 (Filesystem Module Discovering)

nessa 采用类似 Rust 的文件系统模块映射方式，源码文件和目录结构直接决定模块树的形状。

## 入口文件

每个包有一个根入口文件，取决于包的类型：

| 包类型 | 入口文件 |
|--------|----------|
| 可执行程序 (`exe`) | `src/main.ns` |
| 库 (`lib`) | `src/lib.ns` |

入口文件定义了包的根模块，所有其他模块都是根模块的子模块。

## 模块映射规则

从入口文件所在目录开始，递归地：

- 每个 `.ns` 源码文件被视为一个子模块，模块名为文件名（去掉 `.ns` 后缀）
- 每个子目录被视为一个子模块，模块名为目录名，其入口文件为目录内的 `mod.ns`

## 示例

假设有如下文件结构：

```
my_package/
├── package.toml
└── src/
    ├── main.ns            -- 根模块（包入口）
    ├── utils.ns           -- 模块 utils
    ├── net/
    │   ├── mod.ns         -- 模块 net
    │   ├── http.ns        -- 模块 net.http
    │   └── tcp.ns         -- 模块 net.tcp
    └── db/
        ├── mod.ns         -- 模块 db
        └── driver/
            ├── mod.ns     -- 模块 db.driver
            └── postgres.ns -- 模块 db.driver.postgres
```

对应的模块树为：

```
root (main.ns)
├── utils
├── net
│   ├── http
│   └── tcp
└── db
    └── driver
        └── postgres
```

## 命名约束

模块名遵循蛇形命名法（`snake_case`），因为模块名直接来源于文件名和目录名，所以文件和目录也应使用蛇形命名。

## 与内联模块的关系

除了文件系统映射，nessa 也支持在源码中直接定义内联模块：

```nessa
mod inline_module {
    fn helper() -> i32 = 42
}
```

内联模块和文件系统模块在语义上完全等价，都是关联作用域的实例。文件系统映射只是一种便捷的隐式声明方式。
