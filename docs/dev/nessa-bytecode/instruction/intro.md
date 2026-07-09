# 指令集参考

本文档列出 NSBC 所有指令类别及其编码。指令均为 **32-bit** 固定宽度；`[23:22]` 为寻址模式（仅 `Load` 等解释）。

## 寻址模式

| amode | 名称 | 语义 |
|-------|------|------|
| 00 | Imm | 立即数（12-bit，Load 时符号扩展） |
| 01 | Const | 常量池索引 |
| 10 | RegOff | `r[base] + sext(offset)` |
| 11 | Reserved | 非法 |

## 指令表概览

### 算术与位运算 (R-type: 00)

| Opcode | 助记符 | 语义 | 操作 |
|--------|--------|------|------|
| 0x00 | ADD | r[d] = r[s1] + r[s2] | 整数/浮点加法，溢出时自动提升 |
| 0x01 | SUB | r[d] = r[s1] - r[s2] | 减法 |
| 0x02 | MUL | r[d] = r[s1] * r[s2] | 乘法 |
| 0x03 | DIV | r[d] = r[s1] / r[s2] | 除法（整数截断，浮点 IEEE 754） |
| 0x04 | MOD | r[d] = r[s1] % r[s2] | 取模 |
| 0x05 | NEG | r[d] = -r[s1] | 取反 |
| 0x06 | BIT_AND | r[d] = r[s1] & r[s2] | 位与 |
| 0x07 | BIT_OR | r[d] = r[s1] \| r[s2] | 位或 |
| 0x08 | BIT_XOR | r[d] = r[s1] ^ r[s2] | 位异或 |
| 0x09 | BIT_NOT | r[d] = ~r[s1] | 位取反 |
| 0x0A | SHL | r[d] = r[s1] << r[s2] | 左移 |
| 0x0B | SHR | r[d] = r[s1] >> r[s2] | 算术右移 |
| 0x0C | USHR | r[d] = r[s1] >>> r[s2] | 逻辑右移 |

### 比较 (R-type: 00)

| Opcode | 助记符 | 语义 | 操作 |
|--------|--------|------|------|
| 0x10 | CMP_EQ | r[d] = r[s1] == r[s2] | 相等比较，结果为 bool |
| 0x11 | CMP_NE | r[d] = r[s1] != r[s2] | 不等 |
| 0x12 | CMP_LT | r[d] = r[s1] < r[s2] | 小于 |
| 0x13 | CMP_LE | r[d] = r[s1] <= r[s2] | 小于等于 |
| 0x14 | CMP_GT | r[d] = r[s1] > r[s2] | 大于 |
| 0x15 | CMP_GE | r[d] = r[s1] >= r[s2] | 大于等于 |

### 寄存器操作 (R-type: 00)

| Opcode | 助记符 | 语义 | 操作 |
|--------|--------|------|------|
| 0x18 | MOV | r[d] = r[s1] | 寄存器间复制 |
| 0x19 | SWAP | r[d] ↔ r[s1] | 交换两个寄存器 |

### 常量与类型 (A-type: 01)

| Opcode | 助记符 | 语义 | 操作 |
|--------|--------|------|------|
| 0x40 | LOAD | r[d] = operand(amode) | Imm / Const / RegOff |
| 0x41 | LOAD_CONST_WIDE | r[d] = const_pool[idx19] | 宽常量池索引 |
| 0x42 | LOAD_UNIT | r[d] = Unit | 加载 Unit 值 |
| 0x43 | LOAD_TRUE | r[d] = true | 加载 bool true |
| 0x44 | LOAD_FALSE | r[d] = false | 加载 bool false |
| 0x45 | LOAD_NULL | r[d] = null | 加载 ?T 的空值 |
| 0x48 | TYPE_CHECK | r[d] = r[s] is TypeIndex(imm) | 类型检查，结果为 bool |
| 0x49 | TYPE_CAST | r[d] = r[s] as TypeIndex(imm) | 类型转换（失败则 panic） |
| 0x4A | TYPE_CAST_SAFE | r[d] = r[s] as? TypeIndex(imm) | 安全转换（失败返回 null） |

### 内存访问 (A-type: 01，独立索引，不走 amode)

| Opcode | 助记符 | 语义 | 操作 |
|--------|--------|------|------|
| 0x50 | LOAD_FIELD | r[d] = r[s].field[imm] | 读取对象字段 |
| 0x51 | STORE_FIELD | r[s].field[imm] = r[d] | 写入对象字段 |
| 0x52 | LOAD_INDEX | r[d] = r[s1][r[imm]] | 数组/Map 索引读取 |
| 0x53 | STORE_INDEX | r[s1][r[imm]] = r[d] | 数组/Map 索引写入 |
| 0x54 | LOAD_GLOBAL | r[d] = globals[imm] | 读取全局变量 |
| 0x55 | STORE_GLOBAL | globals[imm] = r[s] | 写入全局变量 |
| 0x56 | LOAD_CAPTURE | r[d] = captures[imm] | 读取闭包捕获变量 |
| 0x57 | LOAD_GLOBAL_WIDE | r[d] = globals[idx19] | 宽全局索引 |
| 0x58 | STORE_GLOBAL_WIDE | globals[idx19] = r[d] | 宽全局写入 |

### 对象操作 (A-type: 01)

| Opcode | 助记符 | 语义 | 操作 |
|--------|--------|------|------|
| 0x60 | NEW_OBJECT | r[d] = new TypeIndex(imm) | 分配堆对象 |
| 0x61 | NEW_LIST | r[d] = List(size=imm) | 创建 List |
| 0x62 | NEW_MAP | r[d] = Map(capacity=imm) | 创建 Map |
| 0x63 | NEW_CLOSURE | r[d] = Closure(func_id, captures) | 创建闭包（func_id:12） |
| 0x64 | NEW_CLOSURE_WIDE | 同上，func_id 来自常量池 | 大 func_id |

### 控制流 (J-type: 10)

| Opcode | 助记符 | 语义 | 操作 |
|--------|--------|------|------|
| 0x80 | JMP | pc += offset | 无条件跳转（17-bit） |
| 0x81 | JMP_IF | if r[cond]: pc += offset | 条件为真时跳转 |
| 0x82 | JMP_IF_NOT | if !r[cond]: pc += offset | 条件为假时跳转 |
| 0x83 | JMP_IF_NULL | if r[cond] == null: pc += offset | 值为 null 时跳转 |
| 0x84 | JMP_IF_NOT_NULL | if r[cond] != null: pc += offset | 值非 null 时跳转 |
| 0x85 | JMP_FAR | pc += offset22 | 远跳转（22-bit） |

### 函数调用 (C-type: 10)

| Opcode | 助记符 | 语义 | 操作 |
|--------|--------|------|------|
| 0x88 | CALL | call func_id(args...) | 静态调用（func_id:14） |
| 0x89 | CALL_INDIRECT | call r[s](args...) | 闭包间接调用 |
| 0x8A | CALL_METHOD | call r[s].method(args...) | 方法调用（method_id:12） |
| 0x8B | CALL_WASM | call wasm_func(args...) | WASM 函数调用 |
| 0x8C | TAIL_CALL | goto func_id(args...) | 尾调用 |
| 0x8D | CALL_INTRINSIC | call intrinsic(args...) | 内建函数 |
| 0x8E | RETURN_UNIT | return Unit | 返回 Unit |
| 0x8F | RETURN | return r[s] | 函数返回 |
| 0x90 | CALL_FAR | call via const pool | 大 func_id |
| 0x91 | CALL_METHOD_FAR | method via const pool | 大 method_id |

### Effect 相关 (E-type: 11)

| Opcode | 助记符 | 语义 | 操作 |
|--------|--------|------|------|
| 0xC0 | EFFECT_CALL | r[d] = evidence.invoke(args) | 静态 evidence 调用 |
| 0xC1 | EFFECT_CALL_DYN | r[d] = dynamic_lookup(effect, args) | 动态 effect 查找+调用 |
| 0xC2 | PUSH_HANDLER | push handler frame | effect_type:11 \| handler:11 |
| 0xC3 | POP_HANDLER | pop handler frame | 卸载 handler |
| 0xC4 | SHIFT | capture continuation to reset | 捕获 delimited continuation |
| 0xC5 | RESET | establish reset prompt | 建立 reset 边界 |
| 0xC6 | RESUME | resume continuation with value | 恢复 continuation |
| 0xC7 | PUSH_HANDLER_WIDE | push via const pool | 宽 handler 元数据 |

### 系统指令 (E-type: 11)

| Opcode | 助记符 | 语义 | 操作 |
|--------|--------|------|------|
| 0xD0 | SAFEPOINT | GC/调度检查点 | 检查 gc_flag 和 preempt_flag |
| 0xD1 | DEBUG_BREAK | 调试断点 | 仅 debug 模式有效 |
| 0xD2 | NOP | 无操作 | 用于对齐或占位 |

## 调用约定

- 参数通过寄存器传递：前 N 个参数依次放入 r0~r(N-1)（最多 8 个）
- 返回值放入 r0
- 调用者保存 r0–r17 中仍需存活的值（caller-saved）
- 被调用者入场时由 VM 保存/恢复 r19–r28（callee-saved）
- Evidence 参数作为隐式额外参数附加在显式参数之后

## 整数溢出处理

算术指令自动处理类型提升：

```
i_small + i_small → 检查溢出 → 溢出则分配堆上 BigInt
u_small + u_small → 检查溢出 → 溢出则分配堆上 BigUint
```

提升发生在指令执行层，对字节码透明。
