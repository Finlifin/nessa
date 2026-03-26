# 运行时表示 (Runtime Representation)

本章描述 Nessa 值在运行时的内存表示方式。

## 核心设计原则

1. **统一 64-bit 值**: 所有值在 VM 中统一为 64-bit `TaggedValue`，无论是立即数还是堆指针
2. **零开销立即数**: 小整数、bool、char、null、unit、symbol 直接编码在 64-bit 内，无需堆分配
3. **类型信息外置**: 堆对象的 128-bit TypeID 不存储在对象内部，通过 32-bit TypeIndex 间接查询 TypePool

## TaggedValue 总览

```
64-bit TaggedValue
┌────────────────────────────────────────────────────────────────┬───┐
│                      Payload (61 bits)                         │Tag│
│                                                                │3b │
└────────────────────────────────────────────────────────────────┴───┘

Tag = 000: HeapObject → payload 是 8 字节对齐的堆指针（低 3 位天然为 0）
Tag = 001: Immediate  → payload 内部再编码区分具体值类型
Tag = 01x: Reserved
Tag = 1xx: Reserved
```

仅用 1 bit（是否为 001）即可区分堆指针和立即数，GC 扫描时只需 check `tag == 000`。

## Immediate 子编码

当 tag = 001 时，payload 的高 4 bits 作为子类型 tag，剩余 57 bits 作为数据：

```
Payload (61 bits)
┌────┬──────────────────────────────────────────────────────────┐
│Sub │                    Data (57 bits)                         │
│4b  │                                                          │
└────┴──────────────────────────────────────────────────────────┘

Sub-tag 编码:
  0000  i_small    57-bit 有符号整数（覆盖 i8~i64/isize 常见值域）
  0001  u_small    57-bit 无符号整数（覆盖 u8~u64/usize 常见值域）
  0010  f64_bits   57-bit 压缩浮点（截断 mantissa 低位）
  0011  bool       bit 0 = true/false
  0100  Symbol     内部符号值（enum variant tag、.ok/.err 等）
  0101  null       ?T 的空值（optional 的 null 状态）
  0110  unit       Unit 值 ()
  0111  char       Unicode 码点（21 bits 即够用）
  1xxx  reserved   保留扩展
```

### 整数溢出策略

- 运算结果超出 57-bit 立即数范围时，自动提升为堆上 BigInt/BigUint 对象
- u128/i128 始终分配在堆上（128 bits 无法放入 57-bit payload）
- 类型信息由编译期确定，运行时通过 TypeIndex 区分 u32 与 i32 等具体类型

### 浮点编码

- f64 有 64 位，无法完整放入 57-bit，通过截断尾数低 7 位来压缩
- 若截断导致精度损失（如 NaN payload、subnormal），回退为堆上 f64 对象
- f32 值在运算时提升为 f64，存储时使用 f64_bits 或堆对象

## 详细文档

- [immediate-values-and-reference-values.md](immediate-values-and-reference-values.md) — 立即值与引用值的完整分类
- [data-type-layout.md](data-type-layout.md) — 堆对象的内存布局设计