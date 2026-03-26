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

## enum 布局

enum 使用 tag + payload 布局。tag 是一个 symbol 立即值，payload 根据携带数据决定：

```
enum Shape {
    circle(radius: f64),
    rect(w: f64, h: f64),
    point,
}

堆布局 (以最大 variant 对齐):
┌──────────────────────────────────────┐
│ Header (16 bytes)                     │
│   TypeIndex: Shape 的 TypeIndex       │
├──────────────────────────────────────┤
│ variant_tag: TaggedValue (symbol)     │ 8 bytes
│ field[0]:    TaggedValue              │ 8 bytes  (radius 或 w)
│ field[1]:    TaggedValue              │ 8 bytes  (h 或 padding)
└──────────────────────────────────────┘

总大小 = 16 + 8 + max_variant_fields × 8 bytes
```

无 payload 的 variant（如 `point`）不需要堆分配——直接表示为 symbol 立即值。

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

## List 布局

```
┌──────────────────────────────────────┐
│ Header (16 bytes)                     │
├──────────────────────────────────────┤
│ len: usize     (元素数)               │ 8 bytes
│ cap: usize     (容量)                 │ 8 bytes
│ data: *TaggedValue (元素指针)          │ 8 bytes → 堆上连续 TaggedValue 数组
└──────────────────────────────────────┘
```

## Map 布局

采用 Robin Hood 哈希表或 Swiss Table 实现：

```
┌──────────────────────────────────────┐
│ Header (16 bytes)                     │
├──────────────────────────────────────┤
│ len: usize                            │ 8 bytes
│ cap: usize                            │ 8 bytes
│ ctrl: *u8       (控制字节数组)         │ 8 bytes
│ entries: *(K,V) (key-value 对数组)     │ 8 bytes
└──────────────────────────────────────┘
```

## Object 布局

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
- 变长对象（String/List/Map）的 data 指针指向的内部缓冲区也需要扫描（如果元素是 TaggedValue）