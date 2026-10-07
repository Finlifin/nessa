# 数据类型内存布局 (Data Type Layout)

## struct 布局

struct 实例在堆上分配，字段按声明顺序排列（不自动重排）：

```
struct Point { x: f64, y: f64 }

堆布局:
┌──────────────────────────────────────┐
│ Header (16 bytes)                     │
│   TypeIndex: Point 的 TypeIndex       │
│   GC Meta / Identity Hash            │
├──────────────────────────────────────┤
│ field[0]: x  (8 bytes, TaggedValue)   │
│ field[1]: y  (8 bytes, TaggedValue)   │
└──────────────────────────────────────┘

总大小 = 16 + N × 8 bytes（N = 字段数）
```

每个字段固定 8 字节（一个 TaggedValue），无论字段的 Nessa 类型是什么。这简化了 GC 扫描——每个 slot 都可能是引用。

## enum 布局（当前已实现）

无载荷variant使用独立`IMM_ENUM = 9`，57-bit data编码为
`(u64(TypeIndex) << 25) | variant_tag`：类型索引32位、tag25位。INVALID索引
或tag超出25位拒绝；subtag4的legacy Symbol保留原解释，不能作为Enum值。
类型及tag受当前TypePool检查，透明alias规范化后使用实际Enum身份。

携带值variant使用固定最大variant大小的GC对象：

```
Header (16 bytes): Enum TypeIndex, payload_words = 1 + max_variant_fields
payload[0]: TaggedValue enum tag (同独立subtag9/type32/tag25编码)
payload[1..]: 当前variant声明顺序的TaggedValue字段
剩余槽: Unit padding
```

header类型必须与tag的Enum身份相同，payload长度必须与该Enum最大字段数一致。
读取只允许当前variant的实际字段；EnumField索引0指第一载荷字段，不包含tag。
总payload最多65535个word，因此最大variant至多65534字段；源码及字段指令还
受当前12-bit编码容量约束。构造时先检查/转换所有字段并保留根，再初始化tag、
字段及Unit padding后发布对象。所有槽按TaggedValue扫描，tag与padding不是裸
ordinal或任意字节，不会被当作堆引用。

Enum/List/Tuple共用显示深度128层和展开1 MiB限制，字符串字段带引号/转义，
递归环显示`<cycle>`；超限错误明确返回。相同Enum/tag的payload equality仍
为UnsupportedEnumEquality，尚未完成Eq/derive分派。

## Trait 参数证明布局（当前已实现）

独立 immediate subtag 10 的 57-bit payload 是 VM 私有不可变 registry 的
受检 handle。进程内单调 ID 防止跨 VM 碰撞；安装新 TypePool 后旧 handle
失效。registry 保存 root vtable、concrete type、当前 trait view 和冻结槽，
不保存对象地址；按 (root vtable, canonical view) 缓存，取得新证明仍检查
调用位置的 scope、权限及签名，不混用不同作用域的 table。

裸 trait 用户参数的物理顺序为 proof、data；闭包 capture 的顺序为 data、
proof，并由 FunctionAbi 描述。GC 扫描原始 data 根，handle 不含堆引用；
continuation clone 复制 handle，捕获/恢复仍拆接栈段。handle 不写入归档常量，
归档只保存 schema、指令和入口布局。返回值与嵌套存储 carrier 尚未实现。

## Error 限定值布局（当前已实现）

```text
Header (16 bytes): qualified TypeIndex, payload_words=3, ErrorEnvelope role
payload[0]: raw u64 stable concrete TypeId high half
payload[1]: raw u64 stable concrete TypeId low half
payload[2]: TaggedValue payload
```

两半标签均为0表示成功，否则必须等于实际错误载荷具体类型的稳定TypeId，
并属于目标错误集合。成功和错误载荷即使同类型也不会混淆。payload为一个
普通托管值槽，collector仅扫描该槽，两个裸标签word绝不当作指针扫描。
对象角色由VM分配器在post_alloc前设置，不能由源码或归档原始对象指定。

限定描述符为size24/align8，总分配包含16字节头。旧size0描述符仍可作为
历史元数据保存；其可执行使用明确拒绝，不把旧未标记载荷解释成新wrapper。
转换保留成功/错误分支，错误标签按具体类型身份检查；成功载荷可受检拓宽。
移动后的正确性由可更新根槽及跨分配重载保证，continuation捕获/恢复仍只拆接栈段。

## String 布局

```
┌──────────────────────────────────────┐
│ Header (16 bytes)                     │
├──────────────────────────────────────┤
│ len: usize     (字节数)               │ 8 bytes
│ cap: usize     (容量)                 │ 8 bytes
│ data: *u8      (UTF-8 字节指针)       │ 8 bytes → 堆上连续 u8 数组
└──────────────────────────────────────┘
```

短字符串优化 (SSO) 可选：当字符串 ≤ 22 字节时，直接内联在 data 区，避免二次分配。

## List 布局（当前已实现）

```
List wrapper                          opaque List buffer
Header: List role TypeIndex           Header: Buffer role TypeIndex
payload[0]: TaggedUInt len             payload[0..capacity]: TaggedValue
payload[1]: TaggedUInt capacity         unused slots: Unit
payload[2]: TaggedValue buffer
```

wrapper固定3个word；空列表buffer为null，len/capacity均0。buffer是受GC管理
的对象引用，不能放Rust Vec或外部裸指针。每个slot按TaggedValue扫描，元数据
也必须tagged；单buffer容量最多65535。grow先初始化/复制新buffer再发布引用，
保持wrapper身份并保留跨分配根；pop清空移出的slot。

角色使用稳定版本1TypeId：List `(0x4e455353434f4c4c,0x0000000100000001)`，
Buffer同namespace低半部`0x0000000100000002`。TypePool/List header均受检，
普通NewObject/字段访问不能创建或修改内部角色。泛型元素约束尚未实现。

## Map 布局（String 键、Any 值）

Map 使用独立、受检的 GC wrapper 与 tagged bucket buffer：

```text
Map wrapper: [len: TaggedUInt, capacity: TaggedUInt, buffer: TaggedValue]
Map buffer:  capacity × [state: TaggedUInt, hash: TaggedUInt, key, value]
```

wrapper 固定3个word；空表capacity/len均0、buffer为null。state为empty0、
occupied1、deleted2；empty/deleted的hash为0且key/value清为Unit。哈希按字符串
UTF-8内容计算并限57位，所有元数据保持tagged，key/value均由GC逐槽扫描。
开放寻址处理碰撞与删除标记；扩容构造完整新buffer后发布，保持wrapper身份。

容量为2的幂，上限8192桶（32768 payload words），不能超过现有u16对象计数。
普通NewObject、LoadField/StoreField不能创建或修改集合内部布局。Map与MapBuffer
稳定TypeId沿用List的namespace，低半部分别为`0x0000000100000003`和
`0x0000000100000004`，现有intrinsic、trait、null与List身份均不重排。

字符串以内容识别键；缺失get/remove返回null，contains可区分存储null与缺失。
此路径尚不支持泛型键值约束、任意Hash/Eq键、Iterator或外部Swiss Table缓冲；
`{ property | expr }`仍属于下面的匿名Object设计，不解释为Map字面量。

## Object 布局（尚未实现的设计）

以下混合容器语义仍未贯通运行时，不能由parser接受语法推断为可执行。

Object 是 Nessa 的匿名结构容器，通过 `{ property | expr }` 语法构造。一个 Object 可同时包含键值对（Map 部分）和普通元素（List 部分），类似 Lua Table 或 XML 属性+子节点的统一设计。

```nessa
-- 纯 Map 形式
{ name: "Nessa", version: 1.0 }

-- 纯 List 形式
{ 1, 2, 3 }

-- 混合形式
{
    1, 2,
    name: "Mixed",
    3,
    active: true
}
```

堆布局：

```
┌──────────────────────────────────────┐
│ Header (16 bytes)                     │
│   TypeIndex: Object 的 TypeIndex      │
├──────────────────────────────────────┤
│ list_len:  usize                      │ 8 bytes   List 部分元素数
│ list_cap:  usize                      │ 8 bytes
│ list_data: *TaggedValue              │ 8 bytes → 堆上连续 TaggedValue 数组
│ map_len:   usize                      │ 8 bytes   Map 部分键值对数
│ map_cap:   usize                      │ 8 bytes
│ map_ctrl:  *u8                        │ 8 bytes   控制字节数组
│ map_entries: *(Key, Value)            │ 8 bytes → 堆上 key-value 对数组
└──────────────────────────────────────┘
```

Object 内部分为两个独立存储区：

- **List 区**: 按插入顺序存储无键表达式（普通 `expr`），与 List 布局一致
- **Map 区**: 存储 `id: expr` 形式的键值对，与 Map 布局一致，key 为 Symbol 或 String

当 Object 仅包含 Map 部分时，list_len = 0，list_data = null；仅包含 List 部分时反之。

## 闭包布局

闭包按值捕获环境变量，生成一个携带环境的对象：

```
lambda |x| x + offset + base

闭包对象布局:
┌──────────────────────────────────────┐
│ Header (16 bytes)                     │
│   TypeIndex: Closure_xxx              │
├──────────────────────────────────────┤
│ func_ptr: FuncId    (函数代码指针)     │ 8 bytes
│ capture[0]: offset  (TaggedValue)     │ 8 bytes
│ capture[1]: base    (TaggedValue)     │ 8 bytes
└──────────────────────────────────────┘
```

## BigInt 布局

```
┌──────────────────────────────────────┐
│ Header (16 bytes)                     │
├──────────────────────────────────────┤
│ sign: u8          (0=正, 1=负)        │
│ len: usize        (digit 数量)        │
│ digits: *u64      (大端序 u64 数组)    │
└──────────────────────────────────────┘
```

## Continuation 布局

delimited continuation 捕获的栈帧序列：

```
┌──────────────────────────────────────┐
│ Header (16 bytes)                     │
├──────────────────────────────────────┤
│ frame_count: usize                    │ 8 bytes
│ frames: *CapturedFrame               │ 8 bytes → 堆上帧数组
│ one_shot: bool                        │ 8 bytes (标记是否已消费)
└──────────────────────────────────────┘

CapturedFrame:
  return_pc, base_reg, func_id, evidence, saved_regs...
  （与 CallFrame 布局一致，但保存在堆上）
```

## GC 扫描规则

- 每个 TaggedValue slot 检查 tag: 只有 `000` (HeapObject) 需要追踪
- 对象的字段数量由 TypePool 中的类型元数据决定
- List/Map 的 managed buffer 逐 TaggedValue 扫描；String 原始字节不扫描。Object 外部缓冲仍属待实现设计
