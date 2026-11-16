//! Minimal, self-contained `x86_64` (SysV64) machine-code assembler for Nessa's JIT.
//!
//! Supports 64-bit general-purpose operations, RIP-relative addressing, forward/backward
//! label fixups, conditional jumps/moves, and SSE2 scalar double (`f64`) instructions.

use crate::code_cache::JitError;

/// 64-bit `x86_64` general-purpose register encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum GpReg {
    Rax = 0,
    Rcx = 1,
    Rdx = 2,
    Rbx = 3,
    Rsp = 4,
    Rbp = 5,
    Rsi = 6,
    Rdi = 7,
    R8 = 8,
    R9 = 9,
    R10 = 10,
    R11 = 11,
    R12 = 12,
    R13 = 13,
    R14 = 14,
    R15 = 15,
}

impl GpReg {
    #[inline]
    pub const fn idx(self) -> u8 {
        self as u8
    }
}

/// 128-bit SSE2 XMM register encoding (`xmm0`..`xmm15`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum XmmReg {
    Xmm0 = 0,
    Xmm1 = 1,
    Xmm2 = 2,
    Xmm3 = 3,
    Xmm4 = 4,
    Xmm5 = 5,
    Xmm6 = 6,
    Xmm7 = 7,
    Xmm8 = 8,
    Xmm9 = 9,
    Xmm10 = 10,
    Xmm11 = 11,
    Xmm12 = 12,
    Xmm13 = 13,
    Xmm14 = 14,
    Xmm15 = 15,
}

impl XmmReg {
    #[inline]
    pub const fn idx(self) -> u8 {
        self as u8
    }
}

/// `x86_64` condition codes (`Jcc` / `CMOVcc` / `SETcc`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Cond {
    /// Overflow (`OF = 1`).
    O = 0x0,
    /// Not overflow (`OF = 0`).
    No = 0x1,
    /// Unsigned below / carry (`CF = 1`).
    B = 0x2,
    /// Unsigned above or equal (`CF = 0`).
    Ae = 0x3,
    /// Equal / zero (`ZF = 1`).
    E = 0x4,
    /// Not equal / non-zero (`ZF = 0`).
    Ne = 0x5,
    /// Unsigned below or equal (`CF = 1 || ZF = 1`).
    Be = 0x6,
    /// Unsigned above (`CF = 0 && ZF = 0`).
    A = 0x7,
    /// Sign / negative (`SF = 1`).
    S = 0x8,
    /// Not sign / non-negative (`SF = 0`).
    Ns = 0x9,
    /// Parity / unordered NaN (`PF = 1`).
    P = 0xA,
    /// Not parity / ordered (`PF = 0`).
    Np = 0xB,
    /// Signed less (`SF != OF`).
    L = 0xC,
    /// Signed greater or equal (`SF == OF`).
    Ge = 0xD,
    /// Signed less or equal (`ZF = 1 || SF != OF`).
    Le = 0xE,
    /// Signed greater (`ZF = 0 && SF == OF`).
    G = 0xF,
}

/// Symbolic code label resolved during [`Assembler::finish`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Label(u32);

struct Fixup {
    patch_offset: usize,
    next_ip: usize,
    label: Label,
}

/// Streaming `x86_64` machine-code emitter with 32-bit relative fixups.
pub struct Assembler {
    buf: Vec<u8>,
    labels: Vec<Option<usize>>,
    fixups: Vec<Fixup>,
}

impl Default for Assembler {
    fn default() -> Self {
        Self::new()
    }
}

impl Assembler {
    pub fn new() -> Self {
        Self {
            buf: Vec::with_capacity(1024),
            labels: Vec::new(),
            fixups: Vec::new(),
        }
    }

    pub fn with_capacity(cap: usize) -> Self {
        Self {
            buf: Vec::with_capacity(cap),
            labels: Vec::new(),
            fixups: Vec::new(),
        }
    }

    /// Current byte offset in the emitted code stream.
    #[inline]
    pub fn offset(&self) -> usize {
        self.buf.len()
    }

    /// Allocate a fresh unbound [`Label`].
    pub fn new_label(&mut self) -> Label {
        let id = self.labels.len() as u32;
        self.labels.push(None);
        Label(id)
    }

    /// Bind `label` to the current emitter offset.
    pub fn bind(&mut self, label: Label) {
        let slot = &mut self.labels[label.0 as usize];
        debug_assert!(slot.is_none(), "label {:?} bound twice", label);
        *slot = Some(self.buf.len());
    }

    /// Query the bound offset of `label`, if already bound.
    pub fn label_offset(&self, label: Label) -> Option<usize> {
        self.labels.get(label.0 as usize).copied().flatten()
    }

    /// Apply all label fixups and return the finalized byte buffer.
    pub fn finish(mut self) -> Result<Vec<u8>, JitError> {
        for fixup in &self.fixups {
            let target = self
                .labels
                .get(fixup.label.0 as usize)
                .copied()
                .flatten()
                .ok_or(JitError::UnsupportedBytecode("unbound assembler label"))?;
            let delta = (target as isize) - (fixup.next_ip as isize);
            let rel32 = i32::try_from(delta)
                .map_err(|_| JitError::UnsupportedBytecode("branch displacement exceeds i32"))?;
            self.buf[fixup.patch_offset..fixup.patch_offset + 4]
                .copy_from_slice(&rel32.to_le_bytes());
        }
        Ok(self.buf)
    }

    // -----------------------------------------------------------------------
    // Encoding helpers
    // -----------------------------------------------------------------------

    #[inline]
    fn emit_u8(&mut self, b: u8) {
        self.buf.push(b);
    }

    #[inline]
    fn emit_u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    #[inline]
    fn emit_i32(&mut self, v: i32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    #[inline]
    fn emit_u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    #[inline]
    fn emit_rex_w(&mut self, reg: u8, rm: u8) {
        let r = (reg >> 3) & 1;
        let b = (rm >> 3) & 1;
        self.emit_u8(0x48 | (r << 2) | b);
    }

    #[inline]
    fn emit_rex_opt(&mut self, reg: u8, rm: u8) {
        let r = (reg >> 3) & 1;
        let b = (rm >> 3) & 1;
        if r != 0 || b != 0 {
            self.emit_u8(0x40 | (r << 2) | b);
        }
    }

    #[inline]
    fn emit_modrm(&mut self, mode: u8, reg: u8, rm: u8) {
        self.emit_u8(((mode & 0b11) << 6) | ((reg & 7) << 3) | (rm & 7));
    }

    /// Emit ModR/M (+ SIB for `rsp`/`r12` + displacement) for `[base + disp]`.
    fn emit_mem(&mut self, reg: u8, base: GpReg, disp: i32) {
        let base_low = base.idx() & 7;
        if disp == 0 && base_low != 5 {
            self.emit_modrm(0b00, reg, base_low);
            if base_low == 4 {
                self.emit_u8(0x24);
            }
        } else if let Ok(d8) = i8::try_from(disp) {
            self.emit_modrm(0b01, reg, base_low);
            if base_low == 4 {
                self.emit_u8(0x24);
            }
            self.emit_u8(d8 as u8);
        } else {
            self.emit_modrm(0b10, reg, base_low);
            if base_low == 4 {
                self.emit_u8(0x24);
            }
            self.emit_i32(disp);
        }
    }

    fn emit_rel32(&mut self, label: Label) {
        let patch_offset = self.buf.len();
        self.emit_u32(0);
        let next_ip = self.buf.len();
        self.fixups.push(Fixup {
            patch_offset,
            next_ip,
            label,
        });
    }

    // -----------------------------------------------------------------------
    // Stack & control flow
    // -----------------------------------------------------------------------

    pub fn push(&mut self, reg: GpReg) {
        if reg.idx() >= 8 {
            self.emit_u8(0x41);
        }
        self.emit_u8(0x50 + (reg.idx() & 7));
    }

    pub fn pop(&mut self, reg: GpReg) {
        if reg.idx() >= 8 {
            self.emit_u8(0x41);
        }
        self.emit_u8(0x58 + (reg.idx() & 7));
    }

    pub fn ret(&mut self) {
        self.emit_u8(0xC3);
    }

    pub fn jmp(&mut self, label: Label) {
        self.emit_u8(0xE9);
        self.emit_rel32(label);
    }

    pub fn jmp_r(&mut self, reg: GpReg) {
        if reg.idx() >= 8 {
            self.emit_u8(0x41);
        }
        self.emit_u8(0xFF);
        self.emit_modrm(0b11, 4, reg.idx());
    }

    pub fn jcc(&mut self, cond: Cond, label: Label) {
        self.emit_u8(0x0F);
        self.emit_u8(0x80 + (cond as u8));
        self.emit_rel32(label);
    }

    pub fn call_r(&mut self, reg: GpReg) {
        if reg.idx() >= 8 {
            self.emit_u8(0x41);
        }
        self.emit_u8(0xFF);
        self.emit_modrm(0b11, 2, reg.idx());
    }

    // -----------------------------------------------------------------------
    // Moves & loads (never clobber EFLAGS)
    // -----------------------------------------------------------------------

    pub fn mov_rr(&mut self, dst: GpReg, src: GpReg) {
        if dst == src {
            return;
        }
        self.emit_rex_w(src.idx(), dst.idx());
        self.emit_u8(0x89);
        self.emit_modrm(0b11, src.idx(), dst.idx());
    }

    /// Load a 64-bit immediate into `dst` without modifying EFLAGS.
    pub fn mov_ri64(&mut self, dst: GpReg, imm: u64) {
        if imm <= u32::MAX as u64 {
            if dst.idx() >= 8 {
                self.emit_u8(0x41);
            }
            self.emit_u8(0xB8 + (dst.idx() & 7));
            self.emit_u32(imm as u32);
        } else if let Ok(s32) = i32::try_from(imm as i64) {
            self.emit_rex_w(0, dst.idx());
            self.emit_u8(0xC7);
            self.emit_modrm(0b11, 0, dst.idx());
            self.emit_i32(s32);
        } else {
            self.emit_rex_w(0, dst.idx());
            self.emit_u8(0xB8 + (dst.idx() & 7));
            self.emit_u64(imm);
        }
    }

    /// `dst = *(u64*)(base + disp)`
    pub fn mov_rm64(&mut self, dst: GpReg, base: GpReg, disp: i32) {
        self.emit_rex_w(dst.idx(), base.idx());
        self.emit_u8(0x8B);
        self.emit_mem(dst.idx(), base, disp);
    }

    /// `*(u64*)(base + disp) = src`
    pub fn mov_mr64(&mut self, base: GpReg, disp: i32, src: GpReg) {
        self.emit_rex_w(src.idx(), base.idx());
        self.emit_u8(0x89);
        self.emit_mem(src.idx(), base, disp);
    }

    /// `dst = zero_extend(*(u32*)(base + disp))`
    pub fn mov_rm32(&mut self, dst: GpReg, base: GpReg, disp: i32) {
        self.emit_rex_opt(dst.idx(), base.idx());
        self.emit_u8(0x8B);
        self.emit_mem(dst.idx(), base, disp);
    }

    /// `*(u32*)(base + disp) = src as u32`
    pub fn mov_mr32(&mut self, base: GpReg, disp: i32, src: GpReg) {
        self.emit_rex_opt(src.idx(), base.idx());
        self.emit_u8(0x89);
        self.emit_mem(src.idx(), base, disp);
    }

    /// `*(u32*)(base + disp) = imm32`
    pub fn mov_mi32(&mut self, base: GpReg, disp: i32, imm: u32) {
        self.emit_rex_opt(0, base.idx());
        self.emit_u8(0xC7);
        self.emit_mem(0, base, disp);
        self.emit_u32(imm);
    }

    /// `dst = zero_extend(*(u8*)(base + disp))`
    pub fn movzx_rm8(&mut self, dst: GpReg, base: GpReg, disp: i32) {
        self.emit_rex_opt(dst.idx(), base.idx());
        self.emit_u8(0x0F);
        self.emit_u8(0xB6);
        self.emit_mem(dst.idx(), base, disp);
    }

    /// `lea dst, [rip + label]`
    pub fn lea_rip_label(&mut self, dst: GpReg, label: Label) {
        self.emit_rex_w(dst.idx(), 0);
        self.emit_u8(0x8D);
        self.emit_modrm(0b00, dst.idx(), 0b101);
        self.emit_rel32(label);
    }

    /// `cmovcc dst, src`
    pub fn cmovcc(&mut self, cond: Cond, dst: GpReg, src: GpReg) {
        self.emit_rex_w(dst.idx(), src.idx());
        self.emit_u8(0x0F);
        self.emit_u8(0x40 + (cond as u8));
        self.emit_modrm(0b11, dst.idx(), src.idx());
    }

    /// `setcc al` (writes `0` or `1` into low byte of `rax`).
    pub fn setcc_al(&mut self, cond: Cond) {
        self.emit_u8(0x0F);
        self.emit_u8(0x90 + (cond as u8));
        self.emit_modrm(0b11, 0, GpReg::Rax.idx());
    }

    // -----------------------------------------------------------------------
    // 64-bit integer ALU & comparisons
    // -----------------------------------------------------------------------

    fn emit_alu_ri32(&mut self, subop: u8, dst: GpReg, imm: i32) {
        self.emit_rex_w(0, dst.idx());
        if let Ok(i8v) = i8::try_from(imm) {
            self.emit_u8(0x83);
            self.emit_modrm(0b11, subop, dst.idx());
            self.emit_u8(i8v as u8);
        } else {
            self.emit_u8(0x81);
            self.emit_modrm(0b11, subop, dst.idx());
            self.emit_i32(imm);
        }
    }

    pub fn add_rr(&mut self, dst: GpReg, src: GpReg) {
        self.emit_rex_w(src.idx(), dst.idx());
        self.emit_u8(0x01);
        self.emit_modrm(0b11, src.idx(), dst.idx());
    }

    pub fn add_ri32(&mut self, dst: GpReg, imm: i32) {
        self.emit_alu_ri32(0, dst, imm);
    }

    /// `*(u64*)(base + disp) += imm`
    pub fn add_m64_i32(&mut self, base: GpReg, disp: i32, imm: i32) {
        self.emit_rex_w(0, base.idx());
        if let Ok(i8v) = i8::try_from(imm) {
            self.emit_u8(0x83);
            self.emit_mem(0, base, disp);
            self.emit_u8(i8v as u8);
        } else {
            self.emit_u8(0x81);
            self.emit_mem(0, base, disp);
            self.emit_i32(imm);
        }
    }

    pub fn sub_rr(&mut self, dst: GpReg, src: GpReg) {
        self.emit_rex_w(src.idx(), dst.idx());
        self.emit_u8(0x29);
        self.emit_modrm(0b11, src.idx(), dst.idx());
    }

    pub fn sub_ri32(&mut self, dst: GpReg, imm: i32) {
        self.emit_alu_ri32(5, dst, imm);
    }

    pub fn imul_rr(&mut self, dst: GpReg, src: GpReg) {
        self.emit_rex_w(dst.idx(), src.idx());
        self.emit_u8(0x0F);
        self.emit_u8(0xAF);
        self.emit_modrm(0b11, dst.idx(), src.idx());
    }

    /// Sign-extend `rax` into `rdx:rax` (`cqo`).
    pub fn cqo(&mut self) {
        self.emit_u8(0x48);
        self.emit_u8(0x99);
    }

    /// Signed 64-bit divide `rdx:rax` by `divisor`: quotient in `rax`, remainder in `rdx`.
    pub fn idiv_r(&mut self, divisor: GpReg) {
        self.emit_rex_w(0, divisor.idx());
        self.emit_u8(0xF7);
        self.emit_modrm(0b11, 7, divisor.idx());
    }

    pub fn neg_r(&mut self, dst: GpReg) {
        self.emit_rex_w(0, dst.idx());
        self.emit_u8(0xF7);
        self.emit_modrm(0b11, 3, dst.idx());
    }

    pub fn not_r(&mut self, dst: GpReg) {
        self.emit_rex_w(0, dst.idx());
        self.emit_u8(0xF7);
        self.emit_modrm(0b11, 2, dst.idx());
    }

    pub fn and_rr(&mut self, dst: GpReg, src: GpReg) {
        self.emit_rex_w(src.idx(), dst.idx());
        self.emit_u8(0x21);
        self.emit_modrm(0b11, src.idx(), dst.idx());
    }

    pub fn and_ri32(&mut self, dst: GpReg, imm: i32) {
        self.emit_alu_ri32(4, dst, imm);
    }

    pub fn or_rr(&mut self, dst: GpReg, src: GpReg) {
        self.emit_rex_w(src.idx(), dst.idx());
        self.emit_u8(0x09);
        self.emit_modrm(0b11, src.idx(), dst.idx());
    }

    pub fn or_ri32(&mut self, dst: GpReg, imm: i32) {
        self.emit_alu_ri32(1, dst, imm);
    }

    pub fn xor_rr(&mut self, dst: GpReg, src: GpReg) {
        self.emit_rex_w(src.idx(), dst.idx());
        self.emit_u8(0x31);
        self.emit_modrm(0b11, src.idx(), dst.idx());
    }

    pub fn xor_ri32(&mut self, dst: GpReg, imm: i32) {
        self.emit_alu_ri32(6, dst, imm);
    }

    pub fn shl_ri8(&mut self, dst: GpReg, count: u8) {
        self.emit_rex_w(0, dst.idx());
        self.emit_u8(0xC1);
        self.emit_modrm(0b11, 4, dst.idx());
        self.emit_u8(count);
    }

    pub fn shr_ri8(&mut self, dst: GpReg, count: u8) {
        self.emit_rex_w(0, dst.idx());
        self.emit_u8(0xC1);
        self.emit_modrm(0b11, 5, dst.idx());
        self.emit_u8(count);
    }

    pub fn sar_ri8(&mut self, dst: GpReg, count: u8) {
        self.emit_rex_w(0, dst.idx());
        self.emit_u8(0xC1);
        self.emit_modrm(0b11, 7, dst.idx());
        self.emit_u8(count);
    }

    pub fn rol_ri8(&mut self, dst: GpReg, count: u8) {
        self.emit_rex_w(0, dst.idx());
        self.emit_u8(0xC1);
        self.emit_modrm(0b11, 0, dst.idx());
        self.emit_u8(count);
    }

    pub fn ror_ri8(&mut self, dst: GpReg, count: u8) {
        self.emit_rex_w(0, dst.idx());
        self.emit_u8(0xC1);
        self.emit_modrm(0b11, 1, dst.idx());
        self.emit_u8(count);
    }

    pub fn shl_cl(&mut self, dst: GpReg) {
        self.emit_rex_w(0, dst.idx());
        self.emit_u8(0xD3);
        self.emit_modrm(0b11, 4, dst.idx());
    }

    pub fn shr_cl(&mut self, dst: GpReg) {
        self.emit_rex_w(0, dst.idx());
        self.emit_u8(0xD3);
        self.emit_modrm(0b11, 5, dst.idx());
    }

    pub fn sar_cl(&mut self, dst: GpReg) {
        self.emit_rex_w(0, dst.idx());
        self.emit_u8(0xD3);
        self.emit_modrm(0b11, 7, dst.idx());
    }

    /// Compare `lhs` and `rhs` (`EFLAGS` from `lhs - rhs`).
    pub fn cmp_rr(&mut self, lhs: GpReg, rhs: GpReg) {
        self.emit_rex_w(rhs.idx(), lhs.idx());
        self.emit_u8(0x39);
        self.emit_modrm(0b11, rhs.idx(), lhs.idx());
    }

    pub fn cmp_ri32(&mut self, lhs: GpReg, imm: i32) {
        self.emit_alu_ri32(7, lhs, imm);
    }

    pub fn test_rr(&mut self, lhs: GpReg, rhs: GpReg) {
        self.emit_rex_w(rhs.idx(), lhs.idx());
        self.emit_u8(0x85);
        self.emit_modrm(0b11, rhs.idx(), lhs.idx());
    }

    pub fn test_ri32(&mut self, lhs: GpReg, imm: i32) {
        self.emit_rex_w(0, lhs.idx());
        self.emit_u8(0xF7);
        self.emit_modrm(0b11, 0, lhs.idx());
        self.emit_i32(imm);
    }

    // -----------------------------------------------------------------------
    // SSE2 scalar double (`f64`) instructions
    // -----------------------------------------------------------------------

    /// `dst_xmm = bitcast<f64>(src_gp)`
    pub fn movq_xr(&mut self, dst: XmmReg, src: GpReg) {
        self.emit_u8(0x66);
        self.emit_rex_w(dst.idx(), src.idx());
        self.emit_u8(0x0F);
        self.emit_u8(0x6E);
        self.emit_modrm(0b11, dst.idx(), src.idx());
    }

    /// `dst_gp = bitcast<u64>(src_xmm)`
    pub fn movq_rx(&mut self, dst: GpReg, src: XmmReg) {
        self.emit_u8(0x66);
        self.emit_rex_w(src.idx(), dst.idx());
        self.emit_u8(0x0F);
        self.emit_u8(0x7E);
        self.emit_modrm(0b11, src.idx(), dst.idx());
    }

    pub fn movsd_xx(&mut self, dst: XmmReg, src: XmmReg) {
        if dst == src {
            return;
        }
        self.emit_u8(0xF2);
        self.emit_rex_opt(dst.idx(), src.idx());
        self.emit_u8(0x0F);
        self.emit_u8(0x10);
        self.emit_modrm(0b11, dst.idx(), src.idx());
    }

    pub fn movsd_xm(&mut self, dst: XmmReg, base: GpReg, disp: i32) {
        self.emit_u8(0xF2);
        self.emit_rex_opt(dst.idx(), base.idx());
        self.emit_u8(0x0F);
        self.emit_u8(0x10);
        self.emit_mem(dst.idx(), base, disp);
    }

    pub fn movsd_mx(&mut self, base: GpReg, disp: i32, src: XmmReg) {
        self.emit_u8(0xF2);
        self.emit_rex_opt(src.idx(), base.idx());
        self.emit_u8(0x0F);
        self.emit_u8(0x11);
        self.emit_mem(src.idx(), base, disp);
    }

    pub fn addsd(&mut self, dst: XmmReg, src: XmmReg) {
        self.emit_u8(0xF2);
        self.emit_rex_opt(dst.idx(), src.idx());
        self.emit_u8(0x0F);
        self.emit_u8(0x58);
        self.emit_modrm(0b11, dst.idx(), src.idx());
    }

    pub fn subsd(&mut self, dst: XmmReg, src: XmmReg) {
        self.emit_u8(0xF2);
        self.emit_rex_opt(dst.idx(), src.idx());
        self.emit_u8(0x0F);
        self.emit_u8(0x5C);
        self.emit_modrm(0b11, dst.idx(), src.idx());
    }

    pub fn mulsd(&mut self, dst: XmmReg, src: XmmReg) {
        self.emit_u8(0xF2);
        self.emit_rex_opt(dst.idx(), src.idx());
        self.emit_u8(0x0F);
        self.emit_u8(0x59);
        self.emit_modrm(0b11, dst.idx(), src.idx());
    }

    pub fn divsd(&mut self, dst: XmmReg, src: XmmReg) {
        self.emit_u8(0xF2);
        self.emit_rex_opt(dst.idx(), src.idx());
        self.emit_u8(0x0F);
        self.emit_u8(0x5E);
        self.emit_modrm(0b11, dst.idx(), src.idx());
    }

    pub fn xorpd(&mut self, dst: XmmReg, src: XmmReg) {
        self.emit_u8(0x66);
        self.emit_rex_opt(dst.idx(), src.idx());
        self.emit_u8(0x0F);
        self.emit_u8(0x57);
        self.emit_modrm(0b11, dst.idx(), src.idx());
    }

    /// Unordered scalar double compare (`ZF=1, PF=1, CF=1` if either operand is `NaN`).
    pub fn ucomisd(&mut self, lhs: XmmReg, rhs: XmmReg) {
        self.emit_u8(0x66);
        self.emit_rex_opt(lhs.idx(), rhs.idx());
        self.emit_u8(0x0F);
        self.emit_u8(0x2E);
        self.emit_modrm(0b11, lhs.idx(), rhs.idx());
    }

    /// Convert signed 64-bit integer `src` to scalar double `dst` (`dst = src as f64`).
    pub fn cvtsi2sd(&mut self, dst: XmmReg, src: GpReg) {
        self.xorpd(dst, dst);
        self.emit_u8(0xF2);
        self.emit_rex_w(dst.idx(), src.idx());
        self.emit_u8(0x0F);
        self.emit_u8(0x2A);
        self.emit_modrm(0b11, dst.idx(), src.idx());
    }
}

#[cfg(all(test, target_arch = "x86_64", unix))]
mod tests {
    use super::*;
    use crate::code_cache::ExecutableBuffer;

    #[test]
    fn encode_and_execute_gp_osr_and_small_int_overflow() {
        // Test function `extern "sysv64" fn(a: u64, b: u64, mode: usize) -> u64`:
        // mode == 0: 57-bit tagged Add fast path (`(a - 1) + b`, returns 0xDEAD on JO).
        // mode != 0: jumps via `lea rax, [rip + base]; add rax, rdx; jmp rax` to `alt_entry`.
        let mut asm = Assembler::new();
        let base_lbl = asm.new_label();
        let mode0_lbl = asm.new_label();
        let overflow_lbl = asm.new_label();
        let alt_lbl = asm.new_label();

        asm.bind(base_lbl);
        asm.push(GpReg::Rbp);
        asm.mov_rr(GpReg::Rbp, GpReg::Rsp);
        asm.push(GpReg::R12);
        asm.push(GpReg::R13);

        asm.test_rr(GpReg::Rdx, GpReg::Rdx);
        asm.jcc(Cond::E, mode0_lbl);
        asm.lea_rip_label(GpReg::Rax, base_lbl);
        asm.add_rr(GpReg::Rax, GpReg::Rdx);
        asm.jmp_r(GpReg::Rax);

        asm.bind(mode0_lbl);
        asm.mov_rr(GpReg::Rax, GpReg::Rdi);
        asm.sub_ri32(GpReg::Rax, 1);
        asm.add_rr(GpReg::Rax, GpReg::Rsi);
        asm.jcc(Cond::O, overflow_lbl);
        asm.pop(GpReg::R13);
        asm.pop(GpReg::R12);
        asm.pop(GpReg::Rbp);
        asm.ret();

        asm.bind(overflow_lbl);
        asm.mov_ri64(GpReg::Rax, 0xDEAD);
        asm.pop(GpReg::R13);
        asm.pop(GpReg::R12);
        asm.pop(GpReg::Rbp);
        asm.ret();

        asm.bind(alt_lbl);
        let alt_off = asm.label_offset(alt_lbl).unwrap();
        // Verify SIB / disp=0 addressing through R12 and R13.
        asm.mov_rr(GpReg::R12, GpReg::Rdi);
        asm.mov_rr(GpReg::R13, GpReg::Rsi);
        asm.mov_rm64(GpReg::Rax, GpReg::R12, 0);
        asm.mov_rm64(GpReg::Rcx, GpReg::R13, 0);
        asm.imul_rr(GpReg::Rax, GpReg::Rcx);
        asm.pop(GpReg::R13);
        asm.pop(GpReg::R12);
        asm.pop(GpReg::Rbp);
        asm.ret();

        let bytes = asm.finish().unwrap();
        let buf = ExecutableBuffer::from_code(&bytes).unwrap();
        // SAFETY: `buf` contains valid SysV64 machine code matching the signature below.
        let func: unsafe extern "sysv64" fn(u64, u64, usize) -> u64 =
            unsafe { std::mem::transmute(buf.as_ptr()) };

        let tag_small = |v: i64| ((v as u64) << 7) | 1;
        // 10 + 32 = 42
        let res = unsafe { func(tag_small(10), tag_small(32), 0) };
        assert_eq!(res, tag_small(42));

        // (2^56 - 1) + 1 overflows 57-bit signed!
        let max57 = (1_i64 << 56) - 1;
        let ovf = unsafe { func(tag_small(max57), tag_small(1), 0) };
        assert_eq!(ovf, 0xDEAD);

        // OSR jump to `alt_off` reading `[r12 + 0] * [r13 + 0]`.
        let x: u64 = 6;
        let y: u64 = 7;
        let prod = unsafe {
            func(
                (&x as *const u64) as u64,
                (&y as *const u64) as u64,
                alt_off,
            )
        };
        assert_eq!(prod, 42);
    }

    #[test]
    fn encode_and_execute_sse2_float_loop() {
        // Compute sum_{i=0}^{n-1} (i as f64) * step
        let mut asm = Assembler::new();
        let loop_lbl = asm.new_label();
        let done_lbl = asm.new_label();

        // rdi = n (i64), rsi = step_bits (u64) -> returns sum_bits (u64)
        asm.xorpd(XmmReg::Xmm0, XmmReg::Xmm0); // sum = 0.0
        asm.movq_xr(XmmReg::Xmm1, GpReg::Rsi); // step
        asm.mov_ri64(GpReg::Rcx, 0); // i = 0

        asm.bind(loop_lbl);
        asm.cmp_rr(GpReg::Rcx, GpReg::Rdi);
        asm.jcc(Cond::Ge, done_lbl);
        asm.cvtsi2sd(XmmReg::Xmm2, GpReg::Rcx);
        asm.mulsd(XmmReg::Xmm2, XmmReg::Xmm1);
        asm.addsd(XmmReg::Xmm0, XmmReg::Xmm2);
        asm.add_ri32(GpReg::Rcx, 1);
        asm.jmp(loop_lbl);

        asm.bind(done_lbl);
        asm.movq_rx(GpReg::Rax, XmmReg::Xmm0);
        asm.ret();

        let bytes = asm.finish().unwrap();
        let buf = ExecutableBuffer::from_code(&bytes).unwrap();
        // SAFETY: `buf` contains valid SysV64 machine code matching `(i64, u64) -> u64`.
        let func: unsafe extern "sysv64" fn(i64, u64) -> u64 =
            unsafe { std::mem::transmute(buf.as_ptr()) };

        let out_bits = unsafe { func(100, 0.1_f64.to_bits()) };
        let out = f64::from_bits(out_bits);
        let mut expected = 0.0_f64;
        for i in 0..100_i64 {
            expected += (i as f64) * 0.1_f64;
        }
        assert_eq!(out.to_bits(), expected.to_bits());
    }
}
