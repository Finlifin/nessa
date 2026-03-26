# 类型 ID (Type ID)

## 概念

Type ID 是 nessa 中每个类型的稳定唯一标识符，为 128 位 hash 值。它用于运行时的类型识别，特别是在 error qualified type 的 tag 位、动态分派等场景中。

## 计算方式

Type ID 通过对以下信息进行 128 位 hash 计算得出：

- 包的 128 位 identity hash（即 `package.toml` 内容的 Merkle Tree hash）
- 类型的最后稳定版本号
- 类型的 layout 信息（字段布局、大小、对齐等）
- 类型符号在包中的完整路径（如 `net.http.Client`）

```
type_id = hash_128(
    package_identity_hash,
    last_stable_version,
    layout_info,
    symbol_path
)
```

## 设计意图

- 稳定性：相同类型在不同编译中产生相同的 Type ID
- 唯一性：128 位 hash 在实践中足以避免碰撞
- 跨包识别：包含包标识信息，不同包中的同名类型有不同的 Type ID
- 版本感知：版本号参与计算，类型的不兼容变更会产生新的 Type ID

## 使用场景

- Error qualified type 的 tag 位存储 128 位 Type ID，用于区分不同的 error 类型
- 运行时类型检查（`Any` 到具体类型的转换）
- 序列化/反序列化时的类型标识

> TODO: hash 算法待定（package identity hash 的算法也待定）。候选方案包括 SipHash-128、xxHash-128、CityHash-128 等，需要在性能和分布质量之间权衡。
