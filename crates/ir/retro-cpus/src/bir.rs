//! Compile generated CPU tables into Boolar IR.
//!
//! Produces one oblivious single-block circuit per CPU step:
//!
//! - entry params are the machine state bits (A, X, Y, S, PCL, PCH,
//!   C Z I D V N — 54 bits);
//! - opcode bytes are fetched through `StorageRead` at symbolic addresses;
//! - every documented pattern's decode path becomes an indicator bit
//!   (OR of ANDs of byte predicates); indicators are mutually exclusive by
//!   construction of the decode tree;
//! - every pattern's semantics program is evaluated obliviously on gate
//!   values starting from the same entry state; memory effects go through
//!   `StorageRead`/`StorageWrite` with indicator-guarded addresses
//!   (non-taken stores are redirected to a scratch region so they cannot
//!   corrupt RAM);
//! - next-state bits are XOR-selected across patterns; when no pattern
//!   matches (undocumented opcode), an explicit fallback branch passes the
//!   state through unchanged and advances PC by one;
//! - the terminator jumps to `Return` with the next-state bits.
//!
//! The result feeds directly into the standard pipeline (`volar-ir-opt`,
//! `movfuscate`, `lower_to_circuit`) and can be evaluated concretely with
//! `volar_fuzz::interpreter::biir::eval_biir`.

use volar_ir::boolar::{
    BIrBlock, BIrBlocks, BIrPreInitSegment, BIrStmt, BIrTarget, BIrTerminator, LaneId,
};
use volar_ir::ir::{IRBlockTargetId, IRVarId, StorageId};
use volar_ir_common::Node;

use crate::generated::m6502::{DECODE_PATHS, PATTERNS, PROGRAMS};
use crate::table::{Bin, Ea, ExprId, Flag, Pred, R8, Step, VNode};

/// Storage space holding retro RAM (one bit per cell).
pub const RETRO_RAM: StorageId = StorageId(5);
/// Lane for [`RETRO_RAM`].
pub const LANE_RAM: LaneId = LaneId(0);

/// Number of entry/exit state bits: A X Y S PCL PCH + C Z I D V N.
pub const STATE_BITS: usize = 6 * 8 + 6;

/// Scratch region for guarded stores; far above any real flat cell address
/// (`addr + bit<<16` tops out under 0x80000).
const SCRATCH_BASE: u64 = 0x100000;

type Byte = [IRVarId; 8];
type Word = [IRVarId; 16];

// ---------------------------------------------------------------------------
// Gate builder
// ---------------------------------------------------------------------------

struct Bir {
    stmts: Vec<Node<BIrStmt, ()>>,
    zero: IRVarId,
    one: IRVarId,
}

impl Bir {
    fn new() -> Self {
        // Params occupy ids 0..STATE_BITS; statements start right after.
        let mut b = Bir { stmts: Vec::new(), zero: IRVarId(0), one: IRVarId(0) };
        b.zero = b.push(BIrStmt::Zero);
        b.one = b.push(BIrStmt::One);
        b
    }

    fn push(&mut self, stmt: BIrStmt) -> IRVarId {
        let id = IRVarId((STATE_BITS + self.stmts.len()) as u32);
        self.stmts.push(Node::new(stmt, (), None));
        id
    }

    fn konst(&self, v: bool) -> IRVarId {
        if v { self.one } else { self.zero }
    }

    fn kbyte(&self, v: u8) -> Byte {
        core::array::from_fn(|i| self.konst((v >> i) & 1 == 1))
    }

    fn and(&mut self, a: IRVarId, b: IRVarId) -> IRVarId {
        self.push(BIrStmt::And(a, b))
    }
    fn or(&mut self, a: IRVarId, b: IRVarId) -> IRVarId {
        self.push(BIrStmt::Or(a, b))
    }
    fn xor(&mut self, a: IRVarId, b: IRVarId) -> IRVarId {
        self.push(BIrStmt::Xor(a, b))
    }
    fn not(&mut self, a: IRVarId) -> IRVarId {
        self.push(BIrStmt::Not(a))
    }
    /// Mux from the AND/OR/NOT basis (Boolar has no Select primitive):
    /// `(c & t) | (!c & f)`.
    fn select(&mut self, c: IRVarId, t: IRVarId, f: IRVarId) -> IRVarId {
        // (c & t) | (!c & f)
        let nt = self.and(c, t);
        let nc = self.not(c);
        let nf = self.and(nc, f);
        self.or(nt, nf)
    }

    fn band(&mut self, a: &Byte, b: &Byte) -> Byte {
        core::array::from_fn(|i| self.and(a[i], b[i]))
    }
    fn bor(&mut self, a: &Byte, b: &Byte) -> Byte {
        core::array::from_fn(|i| self.or(a[i], b[i]))
    }
    fn bxor(&mut self, a: &Byte, b: &Byte) -> Byte {
        core::array::from_fn(|i| self.xor(a[i], b[i]))
    }
    fn bnot(&mut self, a: &Byte) -> Byte {
        core::array::from_fn(|i| self.not(a[i]))
    }
    fn bsel(&mut self, c: IRVarId, t: &Byte, f: &Byte) -> Byte {
        core::array::from_fn(|i| self.select(c, t[i], f[i]))
    }

    fn eq0(&mut self, a: &Byte) -> IRVarId {
        let mut acc = self.konst(true);
        for bit in a {
            let nb = self.not(*bit);
            acc = self.and(acc, nb);
        }
        acc
    }
    fn neq0(&mut self, a: &Byte) -> IRVarId {
        let z = self.eq0(a);
        self.not(z)
    }

    /// Shift left one with carry-in; returns (result, old bit 7).
    fn shl(&self, v: &Byte, cin: IRVarId) -> (Byte, IRVarId) {
        let cout = v[7];
        let mut bits = [cin; 8];
        for i in 1..8usize {
            bits[i] = v[i - 1];
        }
        (bits, cout)
    }

    /// Shift right one with carry-in; returns (result, old bit 0).
    fn shr(&self, v: &Byte, cin: IRVarId) -> (Byte, IRVarId) {
        let cout = v[0];
        let mut bits = [cin; 8];
        for i in 0..7usize {
            bits[i] = v[i + 1];
        }
        (bits, cout)
    }

    /// Ripple-carry add; returns (sum, carry-out, signed overflow).
    fn add8(&mut self, a: &Byte, b: &Byte, cin: IRVarId) -> (Byte, IRVarId, IRVarId) {
        let mut sum = [self.zero; 8];
        let mut c = cin;
        let mut c6 = self.zero;
        for i in 0..8usize {
            if i == 6 {
                c6 = c;
            }
            let axb = self.xor(a[i], b[i]);
            let s = self.xor(axb, c);
            let ab = self.and(a[i], b[i]);
            let xc = self.and(axb, c);
            let maj = self.or(ab, xc);
            sum[i] = s;
            c = maj;
        }
        let ovf = self.xor(c, c6);
        (sum, c, ovf)
    }

    /// 16-bit increment-free add of two (lo, hi) byte pairs.
    fn add16(&mut self, alo: &Byte, ahi: &Byte, blo: &Byte, bhi: &Byte) -> (Byte, Byte) {
        let (lo, c, _) = self.add8(alo, blo, self.zero);
        let (hi, _, _) = self.add8(ahi, bhi, c);
        (lo, hi)
    }

    /// `(lo, hi) + n` where n is a public constant.
    fn inc_word(&mut self, lo: &Byte, hi: &Byte, n: u16) -> (Byte, Byte) {
        let nlo = self.kbyte(n as u8);
        let nhi = self.kbyte((n >> 8) as u8);
        self.add16(lo, hi, &nlo, &nhi)
    }

    /// Read one byte value at a symbolic address (flat cell =
    /// `addr + (bit_index << 16)`).
    fn mem_read(&mut self, addr: &Word) -> Byte {
        core::array::from_fn(|i| {
            let mut av = Vec::with_capacity(19);
            av.extend_from_slice(addr);
            av.push(self.konst(i & 1 == 1));
            av.push(self.konst((i >> 1) & 1 == 1));
            av.push(self.konst((i >> 2) & 1 == 1));
            self.push(BIrStmt::StorageRead { storage: RETRO_RAM, lane: LANE_RAM, addr: av })
        })
    }

    /// Guarded memory write: when `guard` is low, the store lands in the
    /// shared scratch region instead of RAM.
    fn mem_write_guarded(&mut self, guard: IRVarId, addr: &Word, val: &Byte) {
        for (i, bit) in val.iter().enumerate() {
            let cell = SCRATCH_BASE + i as u64;
            let mut av = Vec::with_capacity(19);
            for k in 0..16usize {
                let scratch_bit = self.konst((cell >> k) & 1 == 1);
                av.push(self.select(guard, addr[k], scratch_bit));
            }
            for k in 0..3usize {
                // taken branch carries the bit index of the stored byte
                let idx_bit = self.konst((i >> k) & 1 == 1);
                let scratch_bit = self.konst((cell >> (16 + k)) & 1 == 1);
                av.push(self.select(guard, idx_bit, scratch_bit));
            }
            self.push(BIrStmt::StorageWrite {
                storage: RETRO_RAM,
                lane: LANE_RAM,
                src: *bit,
                addr: av,
            });
        }
    }
}

fn ridx(r: R8) -> usize {
    match r {
        R8::A => 0,
        R8::X => 1,
        R8::Y => 2,
        R8::S => 3,
    }
}

fn fidx(f: Flag) -> usize {
    f as usize
}

fn concat(lo: &Byte, hi: &Byte) -> Word {
    let mut out = [IRVarId(0); 16];
    out[..8].copy_from_slice(lo);
    out[8..].copy_from_slice(hi);
    out
}

fn split(w: &Word) -> (Byte, Byte) {
    (w[..8].try_into().unwrap(), w[8..].try_into().unwrap())
}

// ---------------------------------------------------------------------------
// Oblivious per-pattern evaluation
// ---------------------------------------------------------------------------

struct SymState<'b> {
    b: &'b mut Bir,
    regs: [Byte; 4],
    pc: Word,
    f: [IRVarId; 6],
    imm: Byte,
    imm2: Byte,
    temps: [Byte; 8],
    len: u16,
    /// Indicator under which this pattern's stores may touch RAM.
    guard: Option<IRVarId>,
}

impl SymState<'_> {
    fn ev(&mut self, exprs: &[VNode], e: ExprId) -> Byte {
        match exprs[e.0 as usize] {
            VNode::K(k) => self.b.kbyte(k),
            VNode::Reg(r) => self.regs[ridx(r)],
            VNode::Flg(f) => {
                let bit = self.f[fidx(f)];
                let mut arr = [self.b.zero; 8];
                arr[0] = bit;
                arr
            }
            VNode::T(i) => self.temps[i as usize],
            VNode::Imm => self.imm,
            VNode::Imm2 => self.imm2,
            VNode::Bin(op, a, b2) => {
                let av = self.ev(exprs, a);
                let bv = self.ev(exprs, b2);
                self.alu_raw(op, &av, &bv).0
            }
            VNode::Sel(c, t, f) => {
                let cv = self.ev(exprs, c);
                let tv = self.ev(exprs, t);
                let fv = self.ev(exprs, f);
                let cond = self.b.neq0(&cv);
                self.b.bsel(cond, &tv, &fv)
            }
            VNode::Eq0(inner) => {
                let x = self.ev(exprs, inner);
                let z = self.b.eq0(&x);
                let mut arr = [self.b.zero; 8];
                arr[0] = z;
                arr
            }
            VNode::Load(ea) => {
                let addr = self.ea(ea);
                self.b.mem_read(&addr)
            }
        }
    }

    fn ea(&mut self, ea: Ea) -> Word {
        match ea {
            Ea::Abs => concat(&self.imm, &self.imm2),
            Ea::Zp => {
                let mut out = concat(&self.imm, &self.imm2);
                for bit in out.iter_mut().skip(8) {
                    *bit = self.b.zero;
                }
                out
            }
            Ea::ZpX | Ea::ZpY => {
                let idx = if matches!(ea, Ea::ZpX) {
                    self.regs[ridx(R8::X)]
                } else {
                    self.regs[ridx(R8::Y)]
                };
                let imm = self.imm;
                // page-zero wrap: drop the carry
                let (lo, _, _) = self.b.add8(&imm, &idx, self.b.zero);
                let mut out = [self.b.zero; 16];
                out[..8].copy_from_slice(&lo);
                out
            }
            Ea::AbsX | Ea::AbsY => {
                let base = concat(&self.imm, &self.imm2);
                let idx = if matches!(ea, Ea::AbsX) {
                    self.regs[ridx(R8::X)]
                } else {
                    self.regs[ridx(R8::Y)]
                };
                let (blo, bhi) = split(&base);
                let (lo, c, _) = self.b.add8(&blo, &idx, self.b.zero);
                let zero = self.b.kbyte(0);
                let (hi, _, _) = self.b.add8(&bhi, &zero, c);
                concat(&lo, &hi)
            }
            Ea::IndZpX | Ea::IndZpY => {
                let p = if matches!(ea, Ea::IndZpX) {
                    let imm = self.imm;
                    let x = self.regs[ridx(R8::X)];
                    let (p, _, _) = self.b.add8(&imm, &x, self.b.zero);
                    p
                } else {
                    self.imm
                };
                let one = self.b.kbyte(1);
                let mut paddr = [self.b.zero; 16];
                paddr[..8].copy_from_slice(&p);
                let plo = self.b.mem_read(&paddr);
                // wrapped pointer increment
                let (q, _, _) = self.b.add8(&p, &one, self.b.zero);
                let mut qaddr = [self.b.zero; 16];
                qaddr[..8].copy_from_slice(&q);
                let phi = self.b.mem_read(&qaddr);
                let ptr = concat(&plo, &phi);
                if matches!(ea, Ea::IndZpY) {
                    let y = self.regs[ridx(R8::Y)];
                    let (blo, bhi) = split(&ptr);
                    let (lo, c, _) = self.b.add8(&blo, &y, self.b.zero);
                    let zero = self.b.kbyte(0);
                    let (hi, _, _) = self.b.add8(&bhi, &zero, c);
                    concat(&lo, &hi)
                } else {
                    ptr
                }
            }
            Ea::IndAbsX => {
                let base = concat(&self.imm, &self.imm2);
                let x = self.regs[ridx(R8::X)];
                let (blo, bhi) = split(&base);
                let (ptr_lo, c, _) = self.b.add8(&blo, &x, self.b.zero);
                let zero = self.b.kbyte(0);
                let (ptr_hi, _, _) = self.b.add8(&bhi, &zero, c);
                let ptr = concat(&ptr_lo, &ptr_hi);
                let plo = self.b.mem_read(&ptr);
                // pointer increment WITHOUT wrap (65C02)
                let one = self.b.kbyte(1);
                let (nlo, c2, _) = self.b.add8(&ptr_lo, &one, self.b.zero);
                let (nhi, _, _) = self.b.add8(&ptr_hi, &zero, c2);
                let nxt = concat(&nlo, &nhi);
                let phi = self.b.mem_read(&nxt);
                concat(&plo, &phi)
            }
        }
    }

    fn alu_raw(&mut self, op: Bin, a: &Byte, b2: &Byte) -> (Byte, IRVarId, IRVarId) {
        match op {
            Bin::Add => self.b.add8(a, b2, self.b.zero),
            Bin::Sub => {
                let nb = self.b.bnot(b2);
                let cb = self.f[fidx(Flag::C)];
                self.b.add8(a, &nb, cb)
            }
            Bin::And => (self.b.band(a, b2), self.b.zero, self.b.zero),
            Bin::Ora => (self.b.bor(a, b2), self.b.zero, self.b.zero),
            Bin::Eor => (self.b.bxor(a, b2), self.b.zero, self.b.zero),
            Bin::Asl => {
                let cin = self.b.zero;
                let (r, c) = self.b.shl(a, cin);
                (r, c, self.b.zero)
            }
            Bin::Lsr => {
                let cin = self.b.zero;
                let (r, c) = self.b.shr(a, cin);
                (r, c, self.b.zero)
            }
            Bin::Rol => {
                let cb = self.f[fidx(Flag::C)];
                let (r, c) = self.b.shl(a, cb);
                (r, c, self.b.zero)
            }
            Bin::Ror => {
                let cb = self.f[fidx(Flag::C)];
                let (r, c) = self.b.shr(a, cb);
                (r, c, self.b.zero)
            }
        }
    }

    fn set_nz(&mut self, res: &Byte) {
        self.f[fidx(Flag::N)] = res[7];
        self.f[fidx(Flag::Z)] = self.b.eq0(res);
    }

    fn push_raw(&mut self, v: &Byte) {
        // mem[$0100 + S] <- v ; S -= 1
        let s = self.regs[ridx(R8::S)];
        let hi = self.b.kbyte(0x01);
        let addr = concat(&s, &hi);
        let guard = self.guard.expect("push outside guarded context");
        self.b.mem_write_guarded(guard, &addr, v);
        // S -= 1 == S + 0xFF (mod 256)
        let ff = self.b.kbyte(0xFF);
        let (ns, _, _) = self.b.add8(&s, &ff, self.b.zero);
        self.regs[ridx(R8::S)] = ns;
    }

    fn pull_raw(&mut self) -> Byte {
        // S += 1 ; T <- mem[$0100 + S]
        let one = self.b.kbyte(1);
        let s = self.regs[ridx(R8::S)];
        let (ns, _, _) = self.b.add8(&s, &one, self.b.zero);
        self.regs[ridx(R8::S)] = ns;
        let hi = self.b.kbyte(0x01);
        let addr = concat(&ns, &hi);
        self.b.mem_read(&addr)
    }

    /// Execute one program step obliviously (mirror of cpu.rs's exec_step).
    fn exec_step(&mut self, exprs: &[VNode], st: &Step) {
        match st {
            Step::Let(i, v) => {
                let val = self.ev(exprs, *v);
                self.temps[*i as usize] = val;
            }
            Step::Store(ea, v) => {
                let val = self.ev(exprs, *v);
                let addr = self.ea(*ea);
                let guard = self.guard.expect("store outside guarded context");
                self.b.mem_write_guarded(guard, &addr, &val);
            }
            Step::SetR(r, v) => {
                let val = self.ev(exprs, *v);
                self.regs[ridx(*r)] = val;
            }
            Step::SetF(f, v) => {
                let val = self.ev(exprs, *v);
                self.f[fidx(*f)] = val[0];
            }
            Step::Nz(v) => {
                let val = self.ev(exprs, *v);
                self.set_nz(&val);
            }
            Step::Alu { op, a, b: bv, d, cv } => {
                let av = self.ev(exprs, *a);
                let bvv = self.ev(exprs, *bv);
                let (res, c, ov) = self.alu_raw(*op, &av, &bvv);
                self.temps[*d as usize] = res.clone();
                if *cv {
                    self.f[fidx(Flag::C)] = c;
                    if matches!(op, Bin::Add | Bin::Sub) {
                        self.f[fidx(Flag::V)] = ov;
                    }
                }
                self.set_nz(&res);
            }
            Step::Cmp { a, b: bv } => {
                let av = self.ev(exprs, *a);
                let bvv = self.ev(exprs, *bv);
                let (res, c, ov) = self.alu_raw(Bin::Sub, &av, &bvv);
                self.f[fidx(Flag::C)] = c;
                self.f[fidx(Flag::V)] = ov;
                self.set_nz(&res);
            }
            Step::Test { a, b: bv } => {
                let av = self.ev(exprs, *a);
                let bvv = self.ev(exprs, *bv);
                let anded = self.b.band(&av, &bvv);
                self.f[fidx(Flag::Z)] = self.b.eq0(&anded);
                self.f[fidx(Flag::N)] = bvv[7];
                self.f[fidx(Flag::V)] = bvv[6];
            }
            Step::Br { cond, taken, disp2 } => {
                let cv = self.ev(exprs, *cond);
                let nz = self.b.neq0(&cv);
                let go = if *taken { nz } else { self.b.not(nz) };
                let disp_byte = if *disp2 { self.imm2 } else { self.imm };
                // both next-PCs computed obliviously, then selected
                let stepped = self.stepped_pc();
                let neg = disp_byte[7];
                let ff = self.b.kbyte(0xFF);
                let zz = self.b.kbyte(0x00);
                let ext = self.b.bsel(neg, &ff, &zz);
                let (blo, bhi) = split(&stepped);
                let (tlo, c8, _) = self.b.add8(&blo, &disp_byte, self.b.zero);
                let (thi, _, _) = self.b.add8(&bhi, &ext, c8);
                let lo = self.b.bsel(go, &tlo, &blo);
                let hi = self.b.bsel(go, &thi, &bhi);
                self.pc = concat(&lo, &hi);
            }
            Step::Jump { lo, hi } => {
                let l = self.ev(exprs, *lo);
                let h = self.ev(exprs, *hi);
                self.pc = concat(&l, &h);
            }
            Step::Push(v) => {
                let val = self.ev(exprs, *v);
                self.push_raw(&val);
            }
            Step::Pull(i) => {
                let val = self.pull_raw();
                self.temps[*i as usize] = val;
            }
            Step::PushF => {
                // compose P-style flags byte: N V - - D I Z C
                let mut bits = [self.b.zero; 8];
                bits[0] = self.f[fidx(Flag::C)];
                bits[1] = self.f[fidx(Flag::Z)];
                bits[2] = self.f[fidx(Flag::I)];
                bits[3] = self.f[fidx(Flag::D)];
                bits[6] = self.f[fidx(Flag::V)];
                bits[7] = self.f[fidx(Flag::N)];
                self.push_raw(&bits);
            }
            Step::PullF => {
                let p = self.pull_raw();
                self.f[fidx(Flag::C)] = p[0];
                self.f[fidx(Flag::Z)] = p[1];
                self.f[fidx(Flag::I)] = p[2];
                self.f[fidx(Flag::D)] = p[3];
                self.f[fidx(Flag::V)] = p[6];
                self.f[fidx(Flag::N)] = p[7];
            }
            Step::JumpPtr { plus_x } => {
                let base = concat(&self.imm, &self.imm2);
                let ptr = if *plus_x {
                    let x = self.regs[ridx(R8::X)];
                    let (blo, bhi) = split(&base);
                    let (lo, c, _) = self.b.add8(&blo, &x, self.b.zero);
                    let zero = self.b.kbyte(0);
                    let (hi, _, _) = self.b.add8(&bhi, &zero, c);
                    concat(&lo, &hi)
                } else {
                    base
                };
                let plo = self.b.mem_read(&ptr);
                // pointer increment WITHOUT wrap (65C02)
                let (blo, bhi) = split(&ptr);
                let one = self.b.kbyte(1);
                let (nlo, c2, _) = self.b.add8(&blo, &one, self.b.zero);
                let zero = self.b.kbyte(0);
                let (nhi, _, _) = self.b.add8(&bhi, &zero, c2);
                let nxt = concat(&nlo, &nhi);
                let phi = self.b.mem_read(&nxt);
                self.pc = concat(&plo, &phi);
            }
            Step::Brk => {
                // push P then pc+len+1 (hardware pushes PC+2 for BRK)
                let mut bits = [self.b.zero; 8];
                bits[0] = self.f[fidx(Flag::C)];
                bits[1] = self.f[fidx(Flag::Z)];
                bits[2] = self.f[fidx(Flag::I)];
                bits[3] = self.f[fidx(Flag::D)];
                bits[6] = self.f[fidx(Flag::V)];
                bits[7] = self.f[fidx(Flag::N)];
                self.push_raw(&bits);
                let after = self.inc_pc(self.len + 1);
                let (alo, ahi) = split(&after);
                self.push_raw(&ahi); // PCH
                self.push_raw(&alo); // PCL
                let ff = self.b.kbyte(0xFF);
                let vec_lo_addr = concat(&self.b.kbyte(0xFE), &ff); // $FFFE
                let vlo = self.b.mem_read(&vec_lo_addr);
                let vec_hi_addr = concat(&ff, &ff); // $FFFF
                let vhi = self.b.mem_read(&vec_hi_addr);
                self.pc = concat(&vlo, &vhi);
                self.f[fidx(Flag::I)] = self.b.one;
            }
            Step::RetI => {
                let p = self.pull_raw();
                self.f[fidx(Flag::C)] = p[0];
                self.f[fidx(Flag::Z)] = p[1];
                self.f[fidx(Flag::I)] = p[2];
                self.f[fidx(Flag::D)] = p[3];
                self.f[fidx(Flag::V)] = p[6];
                self.f[fidx(Flag::N)] = p[7];
                let lo = self.pull_raw();
                let hi = self.pull_raw();
                self.pc = concat(&lo, &hi); // no increment
            }
            Step::Ret => {
                let lo = self.pull_raw();
                let hi = self.pull_raw();
                let (nlo, nhi) = self.b.inc_word(&lo, &hi, 1);
                self.pc = concat(&nlo, &nhi);
            }
            Step::Call => {
                // push pc+len-1 (hi then lo); pc <- operand word
                let ret = self.inc_pc(self.len - 1);
                let (rlo, rhi) = split(&ret);
                self.push_raw(&rhi);
                self.push_raw(&rlo);
                self.pc = concat(&self.imm, &self.imm2);
            }
            Step::Nop => {}
        }
    }

    fn inc_pc(&mut self, n: u16) -> Word {
        let (lo, hi) = split(&self.pc);
        let (nlo, nhi) = self.b.inc_word(&lo, &hi, n);
        concat(&nlo, &nhi)
    }

    fn stepped_pc(&mut self) -> Word {
        self.inc_pc(self.len)
    }
}

// ---------------------------------------------------------------------------
// Top-level assembly
// ---------------------------------------------------------------------------

/// Build the single-step Boolar circuit for the generated m6502 tables.
pub fn step_bir() -> BIrBlocks<()> {
    step_bir_with_pre_init(&[])
}

/// Like [`step_bir`] but seeds storage pre-init segments (e.g. a RAM image).
pub fn step_bir_with_pre_init(pre_init: &[BIrPreInitSegment]) -> BIrBlocks<()> {
    let mut b = Bir::new();

    // Entry params: A X Y S PCL PCH C Z I D V N.
    let param = |i: usize| IRVarId(i as u32);
    let regs: [Byte; 4] = core::array::from_fn(|r| {
        core::array::from_fn(|i| param(r * 8 + i))
    });
    let pc: Word = concat(
        &core::array::from_fn(|i| param(32 + i)),
        &core::array::from_fn(|i| param(40 + i)),
    );
    let flags: [IRVarId; 6] = core::array::from_fn(|i| param(48 + i));

    // Fetch opcode bytes 0..=2 at symbolic addresses pc+k.
    let mut op = [[b.zero; 8]; 3];
    for (k, ob) in op.iter_mut().enumerate() {
        let (plo, phi) = split(&pc);
        let off = b.inc_word(&plo, &phi, k as u16);
        *ob = b.mem_read(&concat(&off.0, &off.1));
    }

    // Per-pattern indicator computation.
    let mut indicators: Vec<IRVarId> = Vec::with_capacity(DECODE_PATHS.len());
    let mut any = b.zero;
    for alts in DECODE_PATHS.iter() {
        if alts.is_empty() {
            indicators.push(b.zero);
            continue;
        }
        let mut alt_any = b.zero;
        for alt in *alts {
            let mut acc = b.one;
            for p in *alt {
                let bit = pred_gate(&mut b, p, &op);
                acc = b.and(acc, bit);
            }
            alt_any = b.or(alt_any, acc);
        }
        indicators.push(alt_any);
        any = b.or(any, alt_any);
    }
    // Fallback indicator: no documented pattern matched.
    let invalid_ind = b.not(any);

    // Evaluate every pattern's program obliviously from the entry state.
    let prog_count = PROGRAMS.len();
    let mut next_states: Vec<[Byte; 4]> = Vec::with_capacity(prog_count);
    let mut next_pcs: Vec<Word> = Vec::with_capacity(prog_count);
    let mut next_flags: Vec<[IRVarId; 6]> = Vec::with_capacity(prog_count);

    for (id, prog) in PROGRAMS.iter().enumerate() {
        let ind = indicators[id];
        let zero_byte = b.kbyte(0);
        let mut sym = SymState {
            b: &mut b,
            regs,
            pc,
            f: flags,
            imm: op[1],
            imm2: op[2],
            temps: [zero_byte; 8],
            len: u16::from(prog.len),
            guard: Some(ind),
        };
        for st in prog.steps {
            sym.exec_step(prog.exprs, st);
        }
        if !prog.control {
            let npc = sym.inc_pc(prog.len);
            sym.pc = npc;
        }
        next_states.push(sym.regs);
        next_pcs.push(sym.pc);
        next_flags.push(sym.f);
    }

    // Fallback "pattern": pass-through state, pc += 1.
    {
        let zero_byte = b.kbyte(0);
        let mut sym = SymState {
            b: &mut b,
            regs,
            pc,
            f: flags,
            imm: op[1],
            imm2: op[2],
            temps: [zero_byte; 8],
            len: 1,
            guard: Some(invalid_ind),
        };
        sym.pc = sym.inc_pc(1);
        next_states.push(sym.regs);
        next_pcs.push(sym.pc);
        next_flags.push(sym.f);
        indicators.push(invalid_ind);
    }

    // One-hot XOR combine across all branches.
    let mut out_regs: [Byte; 4] = core::array::from_fn(|_| b.kbyte(0));
    let mut out_pc: Word = [b.zero; 16];
    let mut out_flags: [IRVarId; 6] = [b.zero; 6];
    for (idx, ind) in indicators.iter().enumerate() {
        for r in 0..4usize {
            for i in 0..8usize {
                let contrib = b.and(*ind, next_states[idx][r][i]);
                out_regs[r][i] = b.xor(out_regs[r][i], contrib);
            }
        }
        for i in 0..16usize {
            let contrib = b.and(*ind, next_pcs[idx][i]);
            out_pc[i] = b.xor(out_pc[i], contrib);
        }
        for i in 0..6usize {
            let contrib = b.and(*ind, next_flags[idx][i]);
            out_flags[i] = b.xor(out_flags[i], contrib);
        }
    }

    // Terminator: return the next state.
    let mut args = Vec::with_capacity(STATE_BITS);
    for r in 0..4usize {
        args.extend_from_slice(&out_regs[r]);
    }
    args.extend_from_slice(&out_pc);
    args.extend_from_slice(&out_flags);
    let block = BIrBlock {
        params: STATE_BITS as u32,
        stmts: b.stmts,
        terminator: BIrTerminator::Jmp(BIrTarget {
            block: IRBlockTargetId::Return,
            args,
        }),
    };
    BIrBlocks {
        blocks: vec![block],
        pre_init: pre_init.to_vec(),
    }
}

fn pred_gate(b: &mut Bir, p: &Pred, op: &[[IRVarId; 8]; 3]) -> IRVarId {
    match p {
        Pred::Bit { byte, bit, val } => {
            let wire = op[*byte as usize][*bit as usize];
            if *val { wire } else { b.not(wire) }
        }
        Pred::MaskEq { byte, mask, value } => {
            let byte_bits = &op[*byte as usize];
            let mut acc = b.one;
            for bit in 0..8u8 {
                if mask & (1 << bit) != 0 {
                    let want = b.konst(value & (1 << bit) != 0);
                    let nb = b.not(byte_bits[bit as usize]);
                    let eq = b.select(want, byte_bits[bit as usize], nb);
                    acc = b.and(acc, eq);
                }
            }
            acc
        }
    }
}
