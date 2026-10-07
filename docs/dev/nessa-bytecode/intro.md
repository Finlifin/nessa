# Nessa 字节码 (NSBC)

Nessa Serialized ByteCode (NSBC) 是 Nessa 的编译产物格式，同时也是 VM 的执行输入。

## 指令集概览

- **指令宽度**: 固定 32 bits
- **通用寄存器**: 32 个 (r0~r31)，每个 64-bit，存储 TaggedValue
- **寻址模式**: 指令内 2-bit `amode`（立即 / 常量池 / 寄存器+有符号偏移 / 保留）
- **Archive 版本**: 3（沿用 v2 的 32-bit 指令，定义新的 continuation 栈切换 ABI；旧版本需重新编译）

## ABI（仿 ARM AAPCS）

| 寄存器 | 角色 |
|--------|------|
| r0 | 第 1 参数 / 返回值 |
| r1–r7 | 第 2–8 参数（caller-saved） |
| r8–r15 | caller-saved 临时 |
| r16–r17 | scratch（类 IP0/IP1） |
| r18 | 保留 |
| r19–r28 | callee-saved（调用时由 VM 保存） |
| r29–r31 | 保留（类 FP/LR/SP） |

## 指令格式族

所有指令 32-bit 固定宽度：

```
[31:24] opcode:8
[23:22] amode:2          // 仅 Load 等解释；其余编码为 00
[21:0]  payload:22
```

Opcode 高 2 位编码格式类别：

```
Format 00: R-type (寄存器-寄存器)
┌────────┬──────┬───────┬───────┬───────┬─────┐
│ Opcode │ amode│  Dst  │ Src1  │ Src2  │  0  │
│ 8 bits │  2   │ 5 bit │ 5 bit │ 5 bit │ 7   │
└────────┴──────┴───────┴───────┴───────┴─────┘
用于: 算术运算、比较、寄存器间移动

Format 01: A-type (寻址操作数 / 独立索引)
┌────────┬──────┬───────┬───────┬────────────┐
│ Opcode │ amode│  Dst  │ Base  │   Imm12    │
│ 8 bits │  2   │ 5 bit │ 5 bit │  12 bits   │
└────────┴──────┴───────┴───────┴────────────┘
用于: Load(amode)、字段/全局/捕获、对象创建、类型检查

Format 10 / J: 跳转
┌────────┬──────┬───────┬────────────────────┐
│ Opcode │ amode│ Cond  │   Offset (17-bit)  │
│ 8 bits │  2   │ 5 bit │     signed         │
└────────┴──────┴───────┴────────────────────┘

Format 10 / C: 调用与返回
┌────────┬──────┬────────────────────────────┐
│ Opcode │ amode│     Call payload (22)      │
│ 8 bits │  2   │  arg_count + id / regs     │
└────────┴──────┴────────────────────────────┘

Format 11: E-type (扩展)
┌────────┬──────┬────────────────────────────┐
│ Opcode │ amode│     Payload (22 bits)      │
│ 8 bits │  2   │                            │
└────────┴──────┴────────────────────────────┘
用于: effect、safepoint、调试
```

### 寻址模式 (`amode`)

| 编码 | 名称 | 有效操作数 |
|------|------|------------|
| `00` | Imm | `sext(imm12)` |
| `01` | Const | `const_pool[imm12]` |
| `10` | RegOff | `r[base] + sext(imm12)`（SCALE=0） |
| `11` | Reserved | 解码失败 |

超出三种模式的操作数来源（字段索引、全局、捕获、宽 func_id 等）使用**独立 opcode**，不挤进 amode。

## 指令分类

Continuation 指令与栈段所有权见
[continuation-stack-abi.md](../continuation-stack-abi.md)。

详见 [instruction/](instruction/) 目录。

- [intro.md](intro.md) — 本文档
- [archive.md](archive.md) — NSBC Archive 文件格式
- [package-and-module.md](package-and-module.md) — 包与模块在字节码中的组织
- [safe-point.md](safe-point.md) — Safe-point 机制
- [wasm-in-archive.md](wasm-in-archive.md) — WASM 模块嵌入
