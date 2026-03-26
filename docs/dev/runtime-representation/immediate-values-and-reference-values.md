# 立即值与引用值 (Immediate Values & Reference Values)

## 分类总则

Nessa 的每个运行时值都是一个 64-bit `TaggedValue`，分为两大类：

| 类别 | Tag | 特征 | GC 参与 |
|------|-----|------|---------|
| **立即值 (Immediate)** | 001 | 数据直接编码在 64-bit 内，无需堆分配 | 否 |
| **引用值 (Reference)** | 000 | payload 是堆对象指针，数据在堆上 | 是 |

## 立即值清单

以下类型的值在大小允许时编码为立即数：

| Nessa 类型 | 子编码 | 值域 | 溢出行为 |
|-----------|--------|------|---------|
| `i8`~`i64`, `isize` | i_small (0000) | -2^56 ~ 2^56-1 | 提升为堆上带类型 BigInt |
| `u8`~`u64`, `usize` | u_small (0001) | 0 ~ 2^57-1 | 提升为堆上带类型 BigUint |
| `f64` | f64_bits (0010) | 双精度浮点（截断低 7 位尾数） | 回退为堆上 f64 object |
| `bool` | bool (0011) | true / false | — |
| Symbol | symbol (0100) | enum variant tag 等内部标识 | — |
| `?T` 的 None | null (0101) | 唯一值 | — |
| `Unit` | unit (0110) | 唯一值 `()` | — |
| `char` | char (0111) | Unicode 码点 (U+0000 ~ U+10FFFF) | — |

## 引用值清单

以下类型始终分配在堆上：

| Nessa 类型 | 说明 |
|-----------|------|
| `i128`, `u128` | 128-bit 整数，无法放入 57-bit payload |
| `BigInt` | 任意精度整数 |
| `f64`（精度不足时） | 需要完整 64-bit IEEE 754 表示的浮点 |
| `f32`（作为独立存储时） | 保存为堆上 f32 object；参与运算时提升为 f64 |
| `String` | UTF-8 字符串，堆分配 |
| `List` | 动态数组 |
| `Map` | 哈希表 |
| `Set` | 哈希集合 |
| struct 实例 | 用户定义的结构体 |
| enum 实例（带 payload） | enum variant 携带数据时 |
| 闭包 | 捕获环境的 lambda |
| Continuation | delimited continuation 捕获的栈帧 |

## 堆对象头 (Object Header)

所有堆对象共享统一的 16 字节头：

```
┌──────────────────────────────────────────────┐
│ Object Header (16 bytes)                      │
│  ┌──────────────┬────────────────────────┐   │
│  │ TypeIndex u32 │  GC Metadata u32       │   │
│  ├──────────────┴────────────────────────┤   │
│  │ Identity Hash / Forwarding Ptr (u64)   │   │
│  └────────────────────────────────────────┘   │
└──────────────────────────────────────────────┘
```

- **TypeIndex (u32)**: 索引到 TypePool，获取 128-bit TypeID 和完整类型元数据
- **GC Metadata (u32)**: GC 标记位、年龄、锁状态等
- **Identity Hash / Forwarding Pointer (u64)**: 正常时为对象的身份哈希值；GC 期间复用为转发指针

## 类型判断

```
运行时类型判断流程:

1. 检查 tag == 000 ?
   → 是 HeapObject: 读取 header.TypeIndex → 查 TypePool 获取 TypeID
   → 否 (tag == 001): 读取 sub-tag 确定立即值类型

2. 立即值的具体 Nessa 类型由编译期信息确定
   例如 i_small 可表示 i8/i16/i32/i64/isize，具体是哪个由函数签名/局部类型推断决定
   运行时只关心它是"小整数"，操作统一
```

## 值的相等性

- 立即值: 按 64-bit 位相等比较（同 tag + 同 payload = 相等）
- 引用值: 默认按身份比较（指针相等），可通过实现 `Eq` trait 定义结构相等