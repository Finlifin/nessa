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
| 0x4B | TYPE_ASSERT | r[d] = checked r[s] : TypeIndex(imm) | 隐式类型边界检查；允许数值拓宽，禁止显式转换才允许的窄化/截断 |

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
| 0x65 | LOAD_SLOT | r[d] = local_slots[index] | dst:5 \| slot:17 |
| 0x66 | STORE_SLOT | local_slots[index] = r[s] | source:5 \| slot:17 |
| 0x67 | NEW_ENUM | r[d] = enum(descriptor, r[s]) | dst:5 \| Tuple/Unit参数寄存器:5 \| descriptor常量:12 |
| 0x68 | ENUM_IS | r[d] = enum_variant_is(r[s], descriptor) | dst:5 \| value:5 \| descriptor常量:12 |
| 0x69 | ENUM_FIELD | r[d] = enum_payload(r[s], imm) | dst:5 \| value:5 \| field:12，不包含tag |
| 0x6A | TRAIT_PROOF | r[d] = acquire(r[s], view) | dst:5 \| data:5 \| view:12，实际 caller scope |
| 0x6B | TRAIT_ASSERT | r[d] = check(r[d], r[s], view) | data/dst:5 \| proof:5 \| view:12 |
| 0x6C | TRAIT_PROJECT | r[d] = parent_view(r[s], view) | dst:5 \| proof:5 \| view:12，保留原 table 选择 |
| 0x6D | ERROR_OK | r[d] = Ok(r[s], descriptor) | dst:5 \| value:5 \| Type常量索引:12 |
| 0x6E | ERROR_ERR | r[d] = Err(r[s], descriptor) | dst:5 \| value:5 \| Type常量索引:12 |
| 0x6F | ERROR_IS_OK | r[d] = is_ok(r[s]) | dst:5 \| value:5，imm12必须0 |
| 0x70 | ERROR_PAYLOAD | r[d] = checked_payload(r[s]) | dst:5 \| value:5，imm12必须0 |

Error四条指令要求amode为Imm（编码0）且寄存器有效。构造descriptor必须是
受检的Error限定Type常量，索引至多4095，并参与归档常量重定位；不能把
裸TypeIndex误当descriptor。ERROR_OK显式构造成功分支，ERROR_ERR取实际
载荷的完整128位TypeId并检查目标错误集合。分支检查和提取核验对象角色、
布局、标签及载荷；保留位非零、非法目标或未支持旧布局均报错。

NewEnum的descriptor为Enum常量，不是TypeIndex；source为已求值并保存根的
Tuple（无参数可用Unit），元素数须匹配variant，逐字段受检转换后初始化完整
Enum对象。无载荷variant可直接加载Enum常量，不需要heap分配。
EnumIs比较名义Enum身份和tag，对其它正常Enum或非Enum值返回false，损坏值
报错。EnumField核对当前variant、payload/header和字段类型，仅提取载荷字段。
三条A-type指令不解释amode，保留位必须0；descriptor参与归档常量索引搬迁。

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
| 0x8D | CALL_BUILTIN | call builtin(args...) | 注册的 builtin 函数 |
| 0x8E | RETURN_UNIT | return Unit | 返回 Unit |
| 0x8F | RETURN | return r[s] | 函数返回 |
| 0x90 | CALL_FAR | call via const pool | 大 func_id |
| 0x91 | CALL_METHOD_FAR | method via const pool | 大 method_id |
| 0x92 | TRAIT_CALL | call frozen interface slot | physical_count:5 \| proof:5 \| slot:12，r0 为 receiver data |
| 0x93 | CALL_INDIRECT_PROOF | call r[s](physical args...) | physical_count:5 \| callee:5 \| 0:12，不含 captures |

TraitCall 的 proof 寄存器在参数区之后；receiver 的 proof 不重复加入参数区。
TraitProof、TraitCall、CallIndirectProof 属于 Calls scope coverage；TraitAssert
属于 CallsAndTypes；TraitProject 不查询词法 scope。旧 EffectCallDyn 没有该
证明取得上下文契约，带裸 trait 输入明确拒绝，零输入 effect 捕获 proof 可执行。

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
| 0xC8 | CLONE_CONTINUATION | branch suspended computation | 为多次恢复建立独立分支 |
| 0xC9 | DROP_CONTINUATION | discard suspended computation | 归还捕获的整个栈段链 |
| 0xCA | PUSH_HANDLER_CLOSURE | install in-place closure | closure 寄存器:5 \| effect:17 |
| 0xCB | PUSH_CAPTURING_HANDLER | install capturing closure | closure 寄存器:5 \| metadata 常量:17 |
| 0xCC | RESET_CLOSURE | enter closure on delimiter stack | body 寄存器:5 \| handler 数量:5 \| 0:12 |
| 0xCD | RESUME_CONTINUATION | invoke multi-shot language value | continuation 寄存器:5 \| value 寄存器:5 \| 0:12 |
| 0xCE | RESUME_CONTINUATION_ONCE | consume proven single use | 同上；直接转移原栈段链 |

`RESET`、`SHIFT`、`RESUME` 与 continuation 生命周期指令的 v3 编码及执行协议见
[continuation-stack-abi.md](../../continuation-stack-abi.md)。捕获和恢复均转移栈段链接，
`CLONE_CONTINUATION` 与多次调用的语言指令建立独立分支时复制状态，
`RESUME_CONTINUATION_ONCE` 经编译器证明单次使用后直接恢复原链。

### 系统指令 (E-type: 11)

| Opcode | 助记符 | 语义 | 操作 |
|--------|--------|------|------|
| 0xD0 | SAFEPOINT | GC/调度检查点 | 检查 gc_flag 和 preempt_flag |
| 0xD1 | DEBUG_BREAK | 调试断点 | 仅 debug 模式有效 |
| 0xD2 | NOP | 无操作 | 用于对齐或占位 |
| 0xD3 | ALLOCATE_SLOTS | allocate function local value slots | count:22，当前最大 131072 |
| 0xD4 | MATCH_FAIL | 返回NoMatchingCase错误 | payload必须0，无后继控制流 |

## 调用约定

- 参数通过寄存器传递：前 N 个参数依次放入 r0~r(N-1)（最多 8 个）
- 返回值放入 r0
- 调用者保存 r0–r17 中仍需存活的值（caller-saved）
- 被调用者入场时由 VM 保存/恢复 r19–r28（callee-saved）
- trait 用户参数按 proof、data 展开；具体默认方法的 TraitSelf 同样展开
- closure capture 按 data、proof 保存
- effect handler chain 是独立机制，不是 trait 参数证明表

当前解释器编译器采用保守的 frame slots 布局：每个 NIR local 对应一个
TaggedValue 槽，计算临时值用 r16/r17，结果用 r8。函数进入时先把全部参数写入
槽；普通调用移动槽的所有权到 CallFrame，返回时移回，避免局部值依赖参数
寄存器或永久取模分配。当前 VM 的参数窗口可用 32 个寄存器，间接调用保留一个
寄存器存 receiver，最多 31 个显式参数；这是解释器实现状态。以上设计的 8 个
参数寄存器与其余参数的栈传递仍须统一，不能将当前窗口当作完成了原生 ABI。

slot 索引在 A-type 的 base:5 与 imm12:12 中保存高/低位，不表示寻址寄存器。
越界访问返回 VM 错误，GC 枚举当前函数以及所有保存 CallFrame 的槽。

## 整数溢出处理

算术指令自动处理类型提升：

```
i_small + i_small → 检查溢出 → 溢出则分配堆上 BigInt
u_small + u_small → 检查溢出 → 溢出则分配堆上 BigUint
```

提升发生在指令执行层，对字节码透明。
