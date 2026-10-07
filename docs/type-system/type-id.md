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

## 版本 1 编码与身份来源

类型身份采用域分离 SHA256 的前128位（按大端拆为两个u64）。编码域为
`nessa.type.identity\0`，随后是schema版本、根类型term，以及按anchor字节
排序的有限可达名义类型记录。anchor包含包身份、有效稳定版本和带标签的
完整路径；layout记录包含真实大小、对齐、字段、枚举tag和trait接口。
递归边引用anchor，不展开无限递归；引用类型布局变化传播，无关声明不进入
本类型hash。详细字节规范见[身份协议](../dev/stable-type-id-schema.md)。

最后稳定版本允许调用者显式提供，缺省使用包版本；不自动追踪逐类型历史。
`Driver::compile_with_identity` 接受包上下文和全局/逐路径版本覆盖；重复、
未使用或无效覆盖产生诊断。包上下文可以从受检的已解析包图构造。

没有package.toml的临时源码按受检token流规范化：忽略注释与token间空白，
保留有意义的布局token、literal与macro内容。摘要写入真实临时清单metadata，
然后通过[包管理身份协议](../code-orgnization/package-management.md)计算包身份，
不把token摘要冒充清单Merkle身份。std使用独立真实清单，不随用户源码身份
或用户类型版本覆盖而变化。

透明alias共享目标完整ID，逆向查询返回canonical目标。抽象关联类型、模板
和未专化复合类型仍为ZERO，ZERO没有逆向条目或可执行稳定身份；Trait参数
视图可以执行。原生角色使用受检保留ID，bootstrap trait改为版本化ordinal，
不依赖StrId。发布前检查完整候选池，失败不改动原身份/索引。

TPOL11保存包/原始声明路径/稳定版本，并从实际描述符重计算ID。读取拒绝
不匹配的高低word、layout和输入，即使外层checksum已重算。旧TPOL1–10
保留原索引、ID和执行语义，不补造稳定来源。验证只证明描述符与给定包身份
一致，不认证外部manifest，不代表跨包linker已经完成。

本轮源码接入、归档端到端和独立验收的实际进度见[实施记录](../dev/stable-type-id-audit.md)。
完整Error的128位tag运行时布局、源码newtype语义、泛型和多文件包编排仍需
后续实现；稳定TypeId基础不能代替这些项目目标。
