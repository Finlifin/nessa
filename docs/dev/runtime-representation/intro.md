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
  1000  Type       pool-local u32 TypeIndex
  1001  Enum       pool-local type32 + variant25，无载荷值/metadata
  101x/11xx reserved 保留扩展
```

### 整数溢出策略

- 运算结果超出 57-bit 立即数范围时，自动提升为堆上 BigInt/BigUint 对象
- u128/i128 始终分配在堆上（128 bits 无法放入 57-bit payload）
- 类型信息由编译期确定，运行时通过 TypeIndex 区分 u32 与 i32 等具体类型

### 浮点编码

- f64 有 64 位，无法完整放入 57-bit，通过截断尾数低 7 位来压缩
- 若截断导致精度损失（如 NaN payload、subnormal），回退为堆上 f64 对象
- f32 值在运算时提升为 f64，存储时使用 f64_bits 或堆对象

## 当前实现与验证（2026-10-07）

`TaggedValue::try_from_i64/u64/f64` 只产生无损立即数，不适合编码的值返回 None。
`from_*` 用于已证明能装入立即数的值，越界会报程序不变量错误；普通常量、
运算与 native 返回值均通过 VM numeric factory，自动选择堆表示。

超出57-bit的 i64/u64 使用对应 intrinsic TypeIndex 与一个 u64 payload word；
i128/u128 始终使用两个 u64 words，先低64 bits、后高64 bits，避免要求 payload
具有16-byte对齐。f64 堆值保留完整 bit pattern，包括 subnormal、signed zero
和 NaN payload。GC 只扫描指向对象的根，跳过这些原始标量 payload。

Number 标量快照统一用于 VM 和 native 的运算、比较、转换及打印，不把裸堆指针
交给宿主。分配与初始化之后立即发布到常量池/任务寄存器，builtin 交出的参数
持续保留临时根，不能在下一次 managed allocation 之前丢失根。

当前整数算术实现到128-bit：64-bit结果可提升，超过128-bit返回显式溢出错误。
这不是任意精度 BigInt/BigUint 的完成状态，固定宽语言类型的完整运算/转换语义
仍需落实。源码端到端测试与真实Immix收集测试已经验证上述数值保真路径。

编译期 coercion 为已知数值目标建立实际表示：窄整数/f32 暂时使用带目标
TypeIndex 的堆对象。Any→具体类型使用 TypeAssert，先验证兼容性再转换；
隐式整数拓宽可用，浮点截断只能显式 `.as(T)`。固定参数函数入口先保存所有
参数为根，再检查和装箱，避免分配时丢失尚未处理的参数。

Type 值使用 immediate subtag 8，payload 为当前 TypePool 的 u32 TypeIndex。
构造时检查索引并消除透明 alias，值本身不持有元数据指针或堆引用。其动态类型
为 intrinsic Type；类型名值、`x'type`、native `type_of`、显示与 Any 边界已贯通。
null 的反射返回 canonical `?NoReturn`。结构类型按规范化形状驻留，名义类型
保持独立；effect 声明仍有独立操作身份。带元数据的 closure 反射完整 Function
签名，动态边界精确检查签名并验证 capture 数量；无元数据 closure 返回 intrinsic
Closure。函数 variance/适配、稳定 TypeId 和跨产物 metadata 重定位尚未完成，
不能把 pool-local 索引当成持久身份。

源码函数现在另存 FunctionAbi，明确区分 closure capture 前缀、用户逻辑参数
和物理入口数量。VM 检查环境的实际 capture 数、调用的用户参数数和入口
布局；反射与归档校验复用这一划分，不把数量差当作 captures。旧无描述符
函数保留原解释。裸 trait 用户参数展开为 proof、data；闭包捕获按 data、proof
保存并验证配对。proof 使用独立 immediate subtag 10 的 registry handle，只含
元数据身份，不改变 receiver header，也不是 GC 指针。入口按调用方的冻结证明
检查数据，方法按接口槽执行；转发及父视图投影不重新选择实现。具体默认
方法的源 Self 参数使用 TraitSelf，逻辑签名为具体类型，物理入口仍接原
proof/data；归档以 NSAM5 区分该布局。delimiter 独立
栈段及 continuation 的捕获/恢复仍沿用链接拆接，不复制调用帧。trait 返回、
字段/global/嵌套存储与 Any carrier 尚未完成；显式 Any 绑定继续擦除证明。

## 当前动态 List 表示

List/Buffer 为 TypePool 中使用稳定版本1 TypeId 的 nominal Struct 角色，
不占用新的 Intrinsic，也不靠名字或固定TypeIndex判断。List wrapper payload
固定3个TaggedValue：U64 len、U64 capacity、Any buffer引用；Buffer为私有
GC对象，payload逐槽TaggedValue，未使用元素为Unit。grow只替换buffer，保留
wrapper与别名身份，容量上限65535；pop清除移出槽，空列表返回null。

集合helper核对角色descriptor与header/payload/len/capacity，禁止普通对象
构造和字段操作修改内部布局；native参数/返回值及grow临时对象保留根。
List临时根通过Drop guard恢复，包含unwind路径；pop的heap返回值先交
BuiltinCtx临时根再发布到任务寄存器。当前Immix真实收集验证List-only字符串
引用、pop返回发布前后以及暂停continuation中的List存活，
没有由此完成所有移动GC/collector plan/通用HostRoot协议。

显示嵌套列表时字符串带引号/转义，递归环为`<cycle>`，共享子图正常重复显示。
深度限制128层，展开输出上限1 MiB，超限返回显式显示错误；私有Buffer不显示。
字符串键、Any值Map已使用受GC管理的tagged bucket buffer，支持受检构造、
读写、删除、扩容和显示；泛型List/Map、Hash/Eq键分派、Iterator/IntoIterator
及匿名Object混合容器仍未实现。

## 当前 Enum 表示

Enum无载荷值使用独立immediate subtag9，57-bit data为高32位TypeIndex与低25位
variant tag；旧Symbol subtag4不能当Enum值。同ordinal的不同Enum保留名义身份。
带载荷值为Enum header、同编码tag、最大variant字段数的TaggedValue槽，余槽
初始化Unit；对象头/type/tag/variant和字段读取受检，构造转换期间元素保留根。
Enum/List/Tuple共用128层与1 MiB显示限制，输出Enum名/variant及受检载荷，
递归环为`<cycle>`。真实GC与独立归档验证该表示；payload Eq/derive尚未接通。
完整产物内TypeIndex仍是pool-local引用，不是跨包稳定TypeId。

## 详细文档

- [immediate-values-and-reference-values.md](immediate-values-and-reference-values.md) — 立即值与引用值的完整分类
- [data-type-layout.md](data-type-layout.md) — 堆对象的内存布局设计
