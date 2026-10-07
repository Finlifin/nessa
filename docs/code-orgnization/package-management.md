# 包管理 (Package Management)

## package.toml

每个 nessa 包由项目根目录下的 `package.toml` 文件定义。

### 基本字段

```toml
[package]
name = "my_server"
domain = "com.example"
version = "0.3.1"
type = "exe"                    # "exe"， "lib" 或 "tmp"
description = "A web server"
license = "MIT"
authors = ["[name] <[email]>"]
repository = "https://example.com/my_server"
readme = "README.md"
keywords = ["web", "server"]
min_nessa_version = "1.0.0"     # 最低 nessa 版本要求
```

### 包类型

| 类型 | 入口文件 | 说明 |
|------|----------|------|
| `exe` | `src/main.ns` | 可执行程序，入口文件必须定义 `main` 函数 |
| `lib` | `src/lib.ns` | 库，供其他包依赖使用 |

## Domain 机制

nessa 包必须定义 `domain` 字段。在声明依赖时，包路径采用 `domain/package_name` 的形式：

```toml
[dependencies]
"com.example/utils" = "^1.0.0"
"org.nessa/std_collections" = "~2.1.0"
```

这种设计的优势：
- 有效降低包名冲突概率——不同组织可以拥有同名包而不冲突
- 降低 domain 身份仲裁成本——域名所有权本身就是天然的身份证明

## 包唯一性与版本管理

### 唯一性标识

nessa 通过对 `package.toml` 的内容（不包括版本号字段）及其所有依赖（版本号作为描述版本约束的 ADT）进行 Merkle Tree 128 位 hash 来确定包的唯一性标识。

这意味着：
- 两个内容相同但版本号不同的包共享同一个唯一性标识
- 任何对包结构、依赖关系的修改都会产生新的标识
- 128 位 hash 在实践中足以避免碰撞

### 版本锁定

具体的依赖版本解析结果由 `package.lock` 文件管理，确保构建的可复现性。`package.lock` 应当纳入版本控制。

## 版本约束语法

依赖版本号是一个描述版本约束的 ADT，支持常见的语义化版本约束：

```toml
[dependencies]
"com.example/foo" = "1.2.3"      # 精确版本
"com.example/bar" = "^1.2.0"     # 兼容更新：>=1.2.0, <2.0.0
"com.example/baz" = "~1.2.0"     # 近似版本：>=1.2.0, <1.3.0
"com.example/qux" = ">=1.0, <3.0" # 范围约束
```

## 包身份协议与当前实现范围

包身份采用版本1协议：域分离 SHA256 的前128位，输出32位小写十六进制。
清单按 TOML 语义值编码，table键排序、array保持顺序，字符串/数字/日期等
类型分别编码；包括描述性和未知字段。只排除 package.version，依赖约束
另按规范化 ADT 编码；inline dependency table 的其它属性仍参与身份。
递归选中子包的身份按限定包名排序参与 Merkle 节点，具体选中版本由 lock固定。

版本约束匹配保留精确、caret、tilde、range的semver与预发布规则。没有任何
预发布比较器的合取可规范化等价的省略零分量；若有显式预发布比较器，则
保留分量精度，不将 `<3.0` 和 `<3.0.0` 等可能不同的边界混为同一身份。
规范化不改变匹配规则，也不尝试将所有逻辑等价区间化为同一表达式。

公开 API 由 ManifestDocument 保留完整清单，PackageManifest仍是既有五字段
投影。PackageResolver对整个图共同求解，优先选择最高兼容版本，必要时回溯；
缺依赖、冲突、循环和同名同版本内容冲突均明确报告。锁定图通过
ResolvedPackageGraph.to_lock生成，resolve_document_locked只使用锁定的精确
版本并复核约束、边、身份、root和所有可达项，拒绝未知格式/字段和多余/缺失项。

lock schema1包括 schema_version、identity_schema、root表和packages数组；
root含qualified_name/version/identity，package项另有dependencies限定名数组。
这种包身份和lock API尚不等于源码包编译、包下载、跨包链接或源码TypeId赋值。
确切字节编码、资源预算和验证结果见[实施记录](../dev/package-identity-audit.md)。
