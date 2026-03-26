# Nessa 字节码 (NSBC)

Nessa Serialized ByteCode (NSBC) 是 Nessa 的编译产物格式，同时也是 VM 的执行输入。

## 指令集概览

- **指令宽度**: 固定 64 bits
- **通用寄存器**: 20 个 (r0~r19)，每个 64-bit，存储 TaggedValue
- **寻址空间**: 指令内可编码 5-bit 寄存器号 (0~31，后 12 个保留)，41-bit 立即数

## 指令格式族

所有指令 64-bit 固定宽度，Opcode 高 2 位编码格式类别：

```
Format 00: R-type (寄存器-寄存器)
┌────────┬────────┬────────┬────────┬────────────────────────────────┐
│ Opcode │  Dst   │  Src1  │  Src2  │         Unused/Aux             │
│ 8 bits │ 5 bits │ 5 bits │ 5 bits │         41 bits                │
└────────┴────────┴────────┴────────┴────────────────────────────────┘
用于: 算术运算、比较、寄存器间移动

Format 01: I-type (寄存器-立即数)
┌────────┬────────┬────────┬─────────────────────────────────────────┐
│ Opcode │  Dst   │  Src   │              Immediate                  │
│ 8 bits │ 5 bits │ 5 bits │              46 bits                    │
└────────┴────────┴────────┴─────────────────────────────────────────┘
用于: 加载常量、带立即数运算、类型检查

Format 10: J-type (跳转)
┌────────┬────────┬──────────────────────────────────────────────────┐
│ Opcode │  Cond  │                   Offset                         │
│ 8 bits │ 5 bits │                   51 bits                        │
└────────┴────────┴──────────────────────────────────────────────────┘
用于: 无条件跳转、条件分支、函数调用

Format 11: E-type (扩展)
┌────────┬───────────────────────────────────────────────────────────┐
│ Opcode │                     Extended Payload                      │
│ 8 bits │                         56 bits                           │
└────────┴───────────────────────────────────────────────────────────┘
用于: 宽常量加载、effect 调用、调试指令
```

## 指令分类

详见 [instruction/](instruction/) 目录。

- [intro.md](intro.md) — 本文档
- [archive.md](archive.md) — NSBC Archive 文件格式
- [package-and-module.md](package-and-module.md) — 包与模块在字节码中的组织
- [safe-point.md](safe-point.md) — Safe-point 机制
- [wasm-in-archive.md](wasm-in-archive.md) — WASM 模块嵌入