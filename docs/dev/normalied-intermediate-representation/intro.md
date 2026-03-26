# 规范化中间表示 (NIR)

NIR (Normalized Intermediate Representation) 是 Nessa 编译管线中 Resolution 与 Codegen 之间的中间表示。NIR 的核心目标是将高层语法糖和复杂控制流标准化为简单、均匀的形式，使后续 codegen 只需处理一种简化的语义。

## 设计原则

1. **语法糖消除**: 所有高层语法结构脱糖为基本形式
2. **控制流标准化**: 所有分支和循环统一为基本块 + 条件跳转
3. **显式化**: 隐式行为变为显式（如 evidence 参数、?/! 展开）
4. **接近字节码**: NIR 节点与 NSBC 指令基本一一对应

## 数据结构

```
NIR 由函数为单位组织：

NirFunction:
  func_id:     FuncId
  params:      Vec<NirParam>        -- 参数列表（含隐式 evidence 参数）
  return_type: TypeIndex
  blocks:      Vec<BasicBlock>
  entry_block: BlockId

BasicBlock:
  id:          BlockId
  stmts:       Vec<NirStmt>         -- 顺序语句
  terminator:  Terminator           -- 基本块终结指令

Terminator:
  | Goto(BlockId)                   -- 无条件跳转
  | Branch(NirValue, BlockId, BlockId)  -- 条件跳转 (cond, then, else)
  | Return(NirValue)                -- 函数返回
  | Unreachable                     -- 不可达（NoReturn 路径）

NirStmt:
  | Assign(NirLocal, NirExpr)       -- 赋值
  | Drop(NirLocal)                  -- 显式释放（用于 move 语义）
  | EffectCall(...)                  -- effect 调用
  | Nop                             -- 占位

NirExpr:
  | Literal(TaggedValue)            -- 字面量
  | Local(NirLocal)                 -- 局部变量读取
  | BinOp(Op, NirValue, NirValue)   -- 二元运算
  | UnaryOp(Op, NirValue)           -- 一元运算
  | Call(FuncId, Vec<NirValue>)     -- 函数调用
  | MethodCall(NirValue, MethodId, Vec<NirValue>)
  | FieldAccess(NirValue, FieldIdx)
  | IndexAccess(NirValue, NirValue)
  | NewObject(TypeIndex, Vec<NirValue>)
  | NewClosure(FuncId, Vec<NirValue>)
  | TypeCheck(NirValue, TypeIndex)
  | TypeCast(NirValue, TypeIndex)
```

## 脱糖变换

### for 循环 → while + iter

```nessa
-- 源码:
for item in collection {
    process(item)
}

-- NIR 脱糖:
let _iter = collection.iter()
while _iter.has_next():
    let item = _iter.next()
    process(item)
```

### ?T 传播 → match

```nessa
-- 源码:
let value = get_value()?

-- NIR 脱糖:
let _tmp = get_value()
_tmp match {
    null => return null,
    _v => let value = _v,
}
```

### !E 传播 → match

```nessa
-- 源码:
let data = read_file(path)!

-- NIR 脱糖:
let _tmp = read_file(path)
_tmp match {
    .err(e) => return .err(e),
    .ok(v) => let data = v,
}
```

### 字符串插值 → concat

```nessa
-- 源码:
let msg = "hello, {name}! you are {age} years old."

-- NIR 脱糖:
let msg = String.concat("hello, ", name.to_string(), "! you are ", age.to_string(), " years old.")
```

### Effect 调用 → evidence passing

```nessa
-- 源码:
fn greet():
    let name = read_line()#
    print("hello, {name}")

-- NIR（静态路径）:
fn greet(_ev_readline: &Handler_read_line):
    let name = _ev_readline.invoke()
    print(String.concat("hello, ", name))
```

### when → if/else 链

```nessa
-- 源码:
when {
    x > 0 => "positive",
    x < 0 => "negative",
    else => "zero",
}

-- NIR:
if x > 0:
    "positive"
else if x < 0:
    "negative"
else:
    "zero"
```

### 条件后缀 → if

```nessa
-- 源码:
return null if x < 0

-- NIR:
if x < 0:
    return null
```

## 模式匹配编译

`match` 表达式编译为决策树 (decision tree)，将嵌套的模式匹配转换为一系列条件跳转的基本块序列。

```
match value {
    Point(0, y)   => f(y),
    Point(x, 0)   => g(x),
    Point(x, y)   => h(x, y),
}

编译为决策树:

  Block 0: 检查 value.field[0] == 0 ?
    true  → Block 1: let y = value.field[1]; f(y)
    false → Block 2: 检查 value.field[1] == 0 ?
      true  → Block 3: let x = value.field[0]; g(x)
      false → Block 4: let x = value.field[0]; let y = value.field[1]; h(x, y)
```

编译策略：
- 按列选择区分度最高的字段优先检查
- 生成的决策树无冗余检查
- 穷尽性检查在 Resolution 阶段完成

## 闭包转换

Lambda 表达式中的自由变量分析在 Resolution 阶段完成。NIR 阶段将闭包转换为显式的 `NewClosure` 操作 + 一个顶级函数定义：

```nessa
-- 源码:
fn make_adder(x: i32) -> fn(i32) -> i32:
    fn(y) => x + y

-- NIR:
fn _lambda_0(captures: &[TaggedValue], y: i32) -> i32:
    let x = captures[0]   -- 从捕获数组读取
    x + y

fn make_adder(x: i32) -> Closure:
    NewClosure(_lambda_0, [x])
```

捕获规则（见 lambda-capturing-rules.md）决定变量是 copy 还是 move 到捕获数组中。

## 与前后阶段的关系

```
Resolution                    NIR                          Codegen
┌──────────────┐         ┌──────────────┐         ┌──────────────────┐
│ Resolved AST │────────▶│ NirFunction  │────────▶│ NSBC Instructions│
│ + SymbolTable│  lower  │ + BasicBlock │  emit   │ + StackMaps      │
│ + TypePool   │         │ + Terminator │         │ + ConstPool      │
└──────────────┘         └──────────────┘         └──────────────────┘

Resolution 保证:
  - 所有名称已解析
  - 类型检查通过
  - Effect 证据链已确定

NIR 保证:
  - 无语法糖
  - 控制流为基本块图
  - 所有隐式参数显式化
  - 模式匹配已编译为跳转

Codegen 消费:
  - 遍历基本块图
  - 线性扫描分配寄存器
  - 发射 64-bit 指令
```