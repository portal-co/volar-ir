//! `table` — the generated-data schema for retro CPU interpreters.
//!
//! The generator (`retrop-emit-volar`, living in the retrop repository)
//! emits per-architecture tables conforming to these types: pattern info,
//! decode paths (an OR-of-ANDs of byte predicates), and straight-line
//! semantics programs. Two independent engines in this crate interpret the
//! same tables:
//!
//! - [`crate::cpu::Machine`] — a concrete software interpreter;
//! - [`crate::bir`] — a compiler into Boolar IR (`volar_ir::boolar`),
//!   producing one oblivious single-block circuit per CPU step.
//!
//! Keeping the schema local (rather than depending on retrop) is what makes
//! this crate independently usable.

/// Which 8-bit register a value expression refers to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum R8 {
    /// Accumulator.
    A,
    /// X index.
    X,
    /// Y index.
    Y,
    /// Stack pointer.
    S,
}

/// A flag cell. Order matches the state bit layout: C Z I D V N.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Flag {
    /// Carry.
    C,
    /// Zero.
    Z,
    /// Interrupt disable.
    I,
    /// Decimal.
    D,
    /// Overflow.
    V,
    /// Negative.
    N,
}

/// Binary byte operations.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Bin {
    /// Wraparound addition.
    Add,
    /// Subtraction with borrow (`a - b - !C`).
    Sub,
    /// Bitwise AND.
    And,
    /// Bitwise OR.
    Ora,
    /// Bitwise XOR.
    Eor,
    /// Shift left; carry <- old bit 7.
    Asl,
    /// Shift right; carry <- old bit 0.
    Lsr,
    /// Rotate left through carry.
    Rol,
    /// Rotate right through carry.
    Ror,
}

/// Effective-address expressions (65C02 addressing modes).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ea {
    /// Operand word `opcode[1..3]` little-endian.
    Abs,
    /// Zero page: `opcode[1]`, high byte 0.
    Zp,
    /// `(opcode[1] + X) & 0xFF`.
    ZpX,
    /// `(opcode[1] + Y) & 0xFF`.
    ZpY,
    /// `abs + X` (16-bit).
    AbsX,
    /// `abs + Y` (16-bit).
    AbsY,
    /// `($nn,X)`: pointer `(zp+X)&0xFF`, wrapped pointer increment.
    IndZpX,
    /// `($nn),Y`: zp pointer (wrapped increment), plus Y.
    IndZpY,
    /// `($nnnn,X)` (65C02): pointer abs+X, no wrapped increment.
    IndAbsX,
}

/// Reference to an expression node in a [`Prog`]'s arena.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ExprId(pub u16);

/// One node of the expression arena.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VNode {
    /// Public constant.
    K(u8),
    /// Register cell.
    Reg(R8),
    /// Flag cell as 0/1.
    Flg(Flag),
    /// Temporary bound by [`Step::Let`]/[`Step::Alu`]/[`Step::Pull`].
    T(u8),
    /// First operand byte (`opcode[1]`).
    Imm,
    /// Second operand byte (`opcode[2]`).
    Imm2,
    /// Binary operation over two sub-expressions.
    Bin(Bin, ExprId, ExprId),
    /// Oblivious select: `c != 0 ? t : f`.
    Sel(ExprId, ExprId, ExprId),
    /// One if the operand is zero else zero.
    Eq0(ExprId),
    /// Memory load at an effective address.
    Load(Ea),
}

/// One effect of an instruction; expressions are arena references.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    /// Bind temporary `T(i) <- v`.
    Let(u8, ExprId),
    /// `mem[ea] <- v`.
    Store(Ea, ExprId),
    /// Register assignment.
    SetR(R8, ExprId),
    /// Flag assignment (nonzero of `v`).
    SetF(Flag, ExprId),
    /// N/Z from value.
    Nz(ExprId),
    /// ALU micro-op: `T(d) = op(a, b)`; N/Z always set; when `cv`, C/V are
    /// updated from carry-out/overflow. `Sub`/`Rol`/`Ror` read carry.
    Alu {
        /// Operation.
        op: Bin,
        /// Left operand.
        a: ExprId,
        /// Right operand.
        b: ExprId,
        /// Destination temp index.
        d: u8,
        /// Update C (and V for Add/Sub).
        cv: bool,
    },
    /// CMP-style: compute `a - b - !C`, set N/Z/C/V, discard result.
    Cmp {
        /// Left operand.
        a: ExprId,
        /// Right operand.
        b: ExprId,
    },
    /// BIT-style: Z from `a & b`; N from bit 7 of `b`; V from bit 6 of `b`.
    Test {
        /// Accumulator-side operand (Z source is `a & b`).
        a: ExprId,
        /// Memory-side operand (N/V come from bits 7/6).
        b: ExprId,
    },
    /// Branch: if `cond != 0 == taken`, `pc += len + sext(disp)`, else
    /// `pc += len`.
    Br {
        /// Branch condition.
        cond: ExprId,
        /// Branch when the condition is nonzero?
        taken: bool,
        /// Displacement source: `false` = `opcode[1]`, `true` = `opcode[2]`
        /// (Rockwell BBR/BBS).
        disp2: bool,
    },
    /// Unconditional PC load.
    Jump {
        /// New PC low byte.
        lo: ExprId,
        /// New PC high byte.
        hi: ExprId,
    },
    /// `mem[$0100 + S] <- v; S -= 1`.
    Push(ExprId),
    /// `S += 1; T(i) = mem[$0100 + S]`.
    Pull(u8),
    /// Push the composed P-style flags byte (N V - - D I Z C).
    PushF,
    /// Pull the flags byte into the flag cells.
    PullF,
    /// PC <- pointer deref: `p = abs (+ X)` (16-bit, no wrap),
    /// `pc = mem[p] | mem[p+1] << 8` (65C02 semantics).
    JumpPtr {
        /// Add X to the pointer base?
        plus_x: bool,
    },
    /// Push P then pc+len+1 (hi first; hardware pushes PC+2 for the 1-byte
    /// BRK); `pc <- $FFFE/$FFFF`; I <- 1.
    Brk,
    /// Pull P, lo, hi; PC restored verbatim (RTI does not increment).
    RetI,
    /// Pull lo, hi; `pc++` (RTS).
    Ret,
    /// Push pc+len-1 (hi then lo); `pc <- operand word` (JSR).
    Call,
    /// No effect.
    Nop,
}

/// A straight-line semantics program for one decoded pattern.
///
/// `control` marks programs whose steps write PC themselves; when false the
/// engines advance `pc += len` after execution.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Prog {
    /// Instruction length in bytes (public).
    pub len: u16,
    /// Do the steps perform control flow themselves?
    pub control: bool,
    /// Effects in execution order.
    pub steps: &'static [Step],
    /// Expression arena referenced by the steps.
    pub exprs: &'static [VNode],
}

/// Static info about one decode pattern.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PatternInfo {
    /// Disassembly mnemonic (`"<invalid>"` for the sentinel at id 0).
    pub mnemonic: &'static str,
    /// Addressing mode (empty for the sentinel).
    pub mode: &'static str,
    /// Instruction length in bytes.
    pub len: u8,
}

/// Full observable machine state (also the circuit input/output layout:
/// A X Y S PCL PCH, then C Z I D V N).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct State {
    /// A.
    pub a: u8,
    /// X.
    pub x: u8,
    /// Y.
    pub y: u8,
    /// S.
    pub s: u8,
    /// PC.
    pub pc: u16,
    /// Flags C Z I D V N.
    pub f: [bool; 6],
}

impl State {
    /// Flatten to the circuit's input/output bit order.
    pub fn bits(&self) -> std::vec::Vec<bool> {
        let mut out = std::vec::Vec::with_capacity(6 * 8 + 6);
        for byte in [self.a, self.x, self.y, self.s, self.pc as u8, (self.pc >> 8) as u8] {
            for i in 0..8u8 {
                out.push((byte >> i) & 1 == 1);
            }
        }
        out.extend(self.f);
        out
    }

    /// Unflatten from [`Self::bits`] order.
    pub fn from_bits(bits: &[bool]) -> Self {
        let byte = |base: usize| -> u8 {
            bits[base..base + 8]
                .iter()
                .enumerate()
                .fold(0u8, |acc, (i, b)| acc | (u8::from(*b) << i))
        };
        let pc = u16::from_le_bytes([byte(32), byte(40)]);
        let mut f = [false; 6];
        f.copy_from_slice(&bits[48..54]);
        State { a: byte(0), x: byte(8), y: byte(16), s: byte(24), pc, f }
    }
}

/// One byte predicate over fetched opcode bytes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pred {
    /// `bit(bit) of byte[byte] == val`.
    Bit {
        /// Fetched-byte index.
        byte: u16,
        /// Bit index (0 = LSB).
        bit: u8,
        /// Required value.
        val: bool,
    },
    /// `(byte[byte] & mask) == value`.
    MaskEq {
        /// Fetched-byte index.
        byte: u16,
        /// Compared bits.
        mask: u8,
        /// Required masked value.
        value: u8,
    },
}
