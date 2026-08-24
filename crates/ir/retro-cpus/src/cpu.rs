//! Concrete software interpreter for generated CPU tables.
//!
//! Independent of retrop: the decode tables and semantics programs under
//! [`crate::gen`] are plain data; this module is the execution engine.

pub use crate::table::State;
use crate::table::{Bin, Ea, ExprId, Flag, Pred, Prog, R8, Step, VNode};

/// A 64 KiB machine with 65C02 register/flag state.
#[derive(Clone, Debug)]
pub struct Machine {
    /// RAM image.
    pub ram: Vec<u8>,
    a: u8,
    x: u8,
    y: u8,
    s: u8,
    pc: u16,
    f: [bool; 6], // C Z I D V N
    imm: u8,
    imm2: u8,
    temps: [u8; 8],
    active_len: u16,
}

impl Machine {
    /// Fresh machine with the given state and zeroed RAM.
    pub fn new(st: State) -> Self {
        Machine {
            ram: vec![0; 0x10000],
            a: st.a,
            x: st.x,
            y: st.y,
            s: st.s,
            pc: st.pc,
            f: st.f,
            imm: 0,
            imm2: 0,
            temps: [0; 8],
            active_len: 1,
        }
    }

    /// Current full state.
    pub fn state(&self) -> State {
        State {
            a: self.a,
            x: self.x,
            y: self.y,
            s: self.s,
            pc: self.pc,
            f: self.f,
        }
    }

    fn rd_r(&self, r: R8) -> u8 {
        match r {
            R8::A => self.a,
            R8::X => self.x,
            R8::Y => self.y,
            R8::S => self.s,
        }
    }
    fn wr_r(&mut self, r: R8, v: u8) {
        match r {
            R8::A => self.a = v,
            R8::X => self.x = v,
            R8::Y => self.y = v,
            R8::S => self.s = v,
        }
    }
    fn rd_f(&self, f: Flag) -> bool {
        self.f[f as usize]
    }
    fn wr_f(&mut self, f: Flag, v: bool) {
        self.f[f as usize] = v;
    }

    /// Decode the instruction at PC against the generated paths and execute
    /// its semantics program (advancing past it when it does not itself
    /// perform control flow).
    pub fn step(&mut self) {
        let op = [
            self.ram[self.pc as usize],
            self.ram[(self.pc + 1) as usize],
            self.ram[(self.pc + 2) as usize],
        ];
        let mut matched: Option<&Prog> = None;
        for (id, alts) in crate::generated::m6502::DECODE_PATHS.iter().enumerate() {
            if alts.is_empty() {
                continue;
            }
            if alts.iter().any(|alt| alt.iter().all(|p| pred_holds(p, &op))) {
                matched = Some(&crate::generated::m6502::PROGRAMS[id]);
                break;
            }
        }
        let Some(prog) = matched else {
            // Undocumented opcode: skip one byte.
            self.pc = self.pc.wrapping_add(1);
            return;
        };
        self.imm = op[1];
        self.imm2 = op[2];
        self.temps = [0; 8];
        self.active_len = prog.len;
        for st in prog.steps {
            self.exec_step(prog.exprs, st);
        }
        if !prog.control {
            self.pc = self.pc.wrapping_add(u16::from(prog.len));
        }
    }

    fn ev(&self, exprs: &[VNode], e: ExprId) -> u8 {
        match exprs[e.0 as usize] {
            VNode::K(k) => k,
            VNode::Reg(r) => self.rd_r(r),
            VNode::Flg(f) => u8::from(self.rd_f(f)),
            VNode::T(i) => self.temps[i as usize],
            VNode::Imm => self.imm,
            VNode::Imm2 => self.imm2,
            VNode::Bin(op, a, b) => bin_op(op, self.ev(exprs, a), self.ev(exprs, b), self.rd_f(Flag::C)),
            VNode::Sel(c, t, f) => {
                if self.ev(exprs, c) != 0 {
                    self.ev(exprs, t)
                } else {
                    self.ev(exprs, f)
                }
            }
            VNode::Eq0(inner) => u8::from(self.ev(exprs, inner) == 0),
            VNode::Load(ea) => {
                let addr = self.ea(ea);
                self.ram[addr as usize]
            }
        }
    }

    fn ea(&self, ea: Ea) -> u16 {
        match ea {
            Ea::Abs | Ea::AbsX | Ea::AbsY => {
                let base = u16::from_le_bytes([self.imm, self.imm2]);
                match ea {
                    Ea::AbsX => base.wrapping_add(u16::from(self.x)),
                    Ea::AbsY => base.wrapping_add(u16::from(self.y)),
                    _ => base,
                }
            }
            Ea::Zp | Ea::ZpX | Ea::ZpY => {
                let base = u16::from(self.imm);
                match ea {
                    Ea::ZpX => (base + u16::from(self.x)) & 0xFF, // page-zero wrap
                    Ea::ZpY => (base + u16::from(self.y)) & 0xFF,
                    _ => base,
                }
            }
            Ea::IndZpX | Ea::IndZpY => {
                let p = match ea {
                    // page-zero wrap on the index add
                    Ea::IndZpX => (self.imm.wrapping_add(self.x)) as usize,
                    _ => self.imm as usize,
                };
                let lo = self.ram[p];
                let hi = self.ram[(p + 1) & 0xFF]; // wrapped pointer increment
                let ptr = u16::from_le_bytes([lo, hi]);
                match ea {
                    Ea::IndZpY => ptr.wrapping_add(u16::from(self.y)),
                    _ => ptr,
                }
            }
            Ea::IndAbsX => {
                let ptr = u16::from_le_bytes([self.imm, self.imm2]).wrapping_add(u16::from(self.x));
                let lo = self.ram[ptr as usize];
                let hi = self.ram[(ptr + 1) as usize]; // no wrap (65C02)
                u16::from_le_bytes([lo, hi])
            }
        }
    }

    fn set_nz(&mut self, res: u8) {
        self.f[Flag::N as usize] = res & 0x80 != 0;
        self.f[Flag::Z as usize] = res == 0;
    }

    /// Raw ALU: returns (result, carry-out, signed overflow).
    fn alu_raw(&self, op: Bin, a: u8, b: u8) -> (u8, bool, bool) {
        match op {
            Bin::Add => add8(a, b, false),
            Bin::Sub => {
                // a - b - !C == a + ~b + C
                let c = self.rd_f(Flag::C);
                add8(a, !b, c)
            }
            Bin::And => (a & b, false, false),
            Bin::Ora => (a | b, false, false),
            Bin::Eor => (a ^ b, false, false),
            Bin::Asl => ((a << 1), a & 0x80 != 0, false),
            Bin::Lsr => ((a >> 1), a & 1 != 0, false),
            Bin::Rol => {
                let c = self.rd_f(Flag::C);
                (
                    (a << 1) | u8::from(c),
                    a & 0x80 != 0,
                    false,
                )
            }
            Bin::Ror => {
                let c = self.rd_f(Flag::C);
                (
                    (a >> 1) | (u8::from(c) << 7),
                    a & 1 != 0,
                    false,
                )
            }
        }
    }

    fn push(&mut self, v: u8) {
        self.ram[0x0100 + self.s as usize] = v;
        self.s = self.s.wrapping_sub(1);
    }

    fn pull(&mut self) -> u8 {
        self.s = self.s.wrapping_add(1);
        self.ram[0x0100 + self.s as usize]
    }

    /// P-style flags byte: N V - - D I Z C (bit 0 = C).
    fn flags_byte(&self) -> u8 {
        let mut b = 0u8;
        if self.f[Flag::C as usize] { b |= 0x01; }
        if self.f[Flag::Z as usize] { b |= 0x02; }
        if self.f[Flag::I as usize] { b |= 0x04; }
        if self.f[Flag::D as usize] { b |= 0x08; }
        if self.f[Flag::V as usize] { b |= 0x40; }
        if self.f[Flag::N as usize] { b |= 0x80; }
        b
    }

    fn unflags_byte(&mut self, p: u8) {
        self.f[Flag::C as usize] = p & 0x01 != 0;
        self.f[Flag::Z as usize] = p & 0x02 != 0;
        self.f[Flag::I as usize] = p & 0x04 != 0;
        self.f[Flag::D as usize] = p & 0x08 != 0;
        self.f[Flag::V as usize] = p & 0x40 != 0;
        self.f[Flag::N as usize] = p & 0x80 != 0;
    }

    fn exec_step(&mut self, exprs: &[VNode], st: &Step) {
        match st {
            Step::Let(i, v) => self.temps[*i as usize] = self.ev(exprs, *v),
            Step::Store(ea, v) => {
                let val = self.ev(exprs, *v);
                let addr = self.ea(*ea);
                self.ram[addr as usize] = val;
            }
            Step::SetR(r, v) => {
                let val = self.ev(exprs, *v);
                self.wr_r(*r, val);
            }
            Step::SetF(f, v) => {
                let val = self.ev(exprs, *v);
                self.wr_f(*f, val & 1 != 0);
            }
            Step::Nz(v) => {
                let val = self.ev(exprs, *v);
                self.set_nz(val);
            }
            Step::Alu { op, a, b, d, cv } => {
                let av = self.ev(exprs, *a);
                let bv = self.ev(exprs, *b);
                let (res, c, ov) = self.alu_raw(*op, av, bv);
                self.temps[*d as usize] = res;
                if *cv {
                    self.wr_f(Flag::C, c);
                    if matches!(op, Bin::Add | Bin::Sub) {
                        self.wr_f(Flag::V, ov);
                    }
                }
                self.set_nz(res);
            }
            Step::Cmp { a, b } => {
                let av = self.ev(exprs, *a);
                let bv = self.ev(exprs, *b);
                let (res, c, ov) = self.alu_raw(Bin::Sub, av, bv);
                self.wr_f(Flag::C, c);
                self.wr_f(Flag::V, ov);
                self.set_nz(res);
            }
            Step::Test { a, b } => {
                let av = self.ev(exprs, *a);
                let bv = self.ev(exprs, *b);
                self.f[Flag::Z as usize] = av & bv == 0;
                self.f[Flag::N as usize] = bv & 0x80 != 0;
                self.f[Flag::V as usize] = bv & 0x40 != 0;
            }
            Step::Br { cond, taken, disp2 } => {
                let c = self.ev(exprs, *cond);
                let go = (c != 0) == *taken;
                let disp = if *disp2 { self.imm2 } else { self.imm };
                self.pc = if go {
                    self.pc
                        .wrapping_add(u16::from(self.active_len))
                        .wrapping_add(i16::from(disp as i8) as u16)
                } else {
                    self.pc.wrapping_add(u16::from(self.active_len))
                };
            }
            Step::Jump { lo, hi } => {
                let l = self.ev(exprs, *lo);
                let h = self.ev(exprs, *hi);
                self.pc = u16::from_le_bytes([l, h]);
            }
            Step::Push(v) => {
                let val = self.ev(exprs, *v);
                self.push(val);
            }
            Step::Pull(i) => {
                let v = self.pull();
                self.temps[*i as usize] = v;
            }
            Step::PushF => {
                let p = self.flags_byte();
                self.push(p);
            }
            Step::PullF => {
                let p = self.pull();
                self.unflags_byte(p);
            }
            Step::JumpPtr { plus_x } => {
                let ptr = u16::from_le_bytes([self.imm, self.imm2]);
                let ptr = if *plus_x { ptr.wrapping_add(u16::from(self.x)) } else { ptr };
                let lo = self.ram[ptr as usize];
                let hi = self.ram[(ptr + 1) as usize]; // no wrap (65C02)
                self.pc = u16::from_le_bytes([lo, hi]);
            }
            Step::Brk => {
                let after = self
                    .pc
                    .wrapping_add(u16::from(self.active_len) + 1); // hardware pushes PC+2
                let [lo, hi] = after.to_le_bytes();
                self.push(hi);
                self.push(lo);
                let p = self.flags_byte();
                self.push(p);
                let vlo = self.ram[0xFFFE];
                let vhi = self.ram[0xFFFF];
                self.pc = u16::from_le_bytes([vlo, vhi]);
                self.f[Flag::I as usize] = true;
            }
            Step::RetI => {
                let p = self.pull();
                self.unflags_byte(p);
                let lo = self.pull();
                let hi = self.pull();
                self.pc = u16::from_le_bytes([lo, hi]); // no increment
            }
            Step::Ret => {
                let lo = self.pull();
                let hi = self.pull();
                self.pc = u16::from_le_bytes([lo, hi]).wrapping_add(1);
            }
            Step::Call => {
                let ret = self.pc.wrapping_add(u16::from(self.active_len) - 1);
                let [rl, rh] = ret.to_le_bytes();
                self.push(rh);
                self.push(rl);
                self.pc = u16::from_le_bytes([self.imm, self.imm2]);
            }
            Step::Nop => {}
        }
    }
}

fn pred_holds(p: &Pred, op: &[u8; 3]) -> bool {
    match p {
        Pred::Bit { byte, bit, val } => (op[*byte as usize] >> bit) & 1 == u8::from(*val),
        Pred::MaskEq { byte, mask, value } => op[*byte as usize] & mask == *value,
    }
}

/// Apply a binary op concretely.
fn bin_op(op: Bin, a: u8, b: u8, c: bool) -> u8 {
    match op {
        Bin::Add => a.wrapping_add(b),
        Bin::Sub => {
            // a - b - !C
            let borrow = !c;
            a.wrapping_sub(b).wrapping_sub(u8::from(borrow))
        }
        Bin::And => a & b,
        Bin::Ora => a | b,
        Bin::Eor => a ^ b,
        Bin::Asl => a << 1,
        Bin::Lsr => a >> 1,
        Bin::Rol => (a << 1) | u8::from(c),
        Bin::Ror => (a >> 1) | (u8::from(c) << 7),
    }
}

/// Ripple-carry add with carry-in; returns (sum, carry-out, overflow).
pub(crate) fn add8(a: u8, b: u8, cin: bool) -> (u8, bool, bool) {
    let r = u16::from(a) + u16::from(b) + u16::from(cin);
    let sum = r as u8;
    let carry = r > 0xFF;
    let overflow = ((a ^ sum) & (b ^ sum) & 0x80) != 0;
    (sum, carry, overflow)
}
