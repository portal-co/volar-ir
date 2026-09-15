//! Concrete integer constant-folding for a single instruction, given a
//! caller-supplied operand resolver.
//!
//! Deliberately narrow: only pure, side-effect-free integer arithmetic/
//! comparison/select/conversion opcodes fold. `call`, `load`, `store`,
//! `getelementptr`, `alloca`, and anything float/vector/pointer-typed are
//! never folded here — they always fall through to being cloned symbolically
//! by the caller, which is always safe (just less aggressively specialized).
//! This keeps the evaluator free of any memory model or purity analysis.

use inkwell::IntPredicate;
use inkwell::types::{AnyTypeEnum, BasicTypeEnum};
use inkwell::values::{BasicValueEnum, InstructionOpcode, InstructionValue, Operand};

/// Mask `value` down to `bits` significant bits (bits = 1..=128; for this
/// evaluator's `u64` domain, `bits` is clamped to 64).
fn mask(value: u64, bits: u32) -> u64 {
    if bits >= 64 {
        value
    } else {
        value & ((1u64 << bits) - 1)
    }
}

/// Sign-extend the low `bits` bits of `value` into a full `i64`.
fn sign_extend(value: u64, bits: u32) -> i64 {
    if bits == 0 || bits >= 64 {
        return value as i64;
    }
    let shift = 64 - bits;
    ((value << shift) as i64) >> shift
}

fn int_bit_width(ty: AnyTypeEnum<'_>) -> Option<u32> {
    match ty {
        AnyTypeEnum::IntType(t) => Some(t.get_bit_width()),
        _ => None,
    }
}

fn basic_int_bit_width(ty: BasicTypeEnum<'_>) -> Option<u32> {
    match ty {
        BasicTypeEnum::IntType(t) => Some(t.get_bit_width()),
        _ => None,
    }
}

/// Attempt to concretely fold `instr` to a `u64` bit pattern, resolving each
/// value operand through `resolve` (which should consult the caller's
/// environment first, then fall back to reading a literal LLVM constant).
///
/// Returns `None` if `instr`'s opcode isn't a supported pure integer op, its
/// result isn't an integer type, or any operand doesn't resolve — in every
/// `None` case the caller must clone `instr` symbolically instead.
pub fn fold_instruction<'ctx>(
    instr: InstructionValue<'ctx>,
    resolve: &impl Fn(BasicValueEnum<'ctx>) -> Option<u64>,
) -> Option<u64> {
    let result_bits = int_bit_width(instr.get_type())?;

    let operand_value = |i: u32| -> Option<u64> {
        let v = instr.get_operand(i).and_then(Operand::value)?;
        resolve(v)
    };
    let operand_bits = |i: u32| -> Option<u32> {
        let v = instr.get_operand(i).and_then(Operand::value)?;
        basic_int_bit_width(v.get_type())
    };

    match instr.get_opcode() {
        InstructionOpcode::Add => Some(mask(
            operand_value(0)?.wrapping_add(operand_value(1)?),
            result_bits,
        )),
        InstructionOpcode::Sub => Some(mask(
            operand_value(0)?.wrapping_sub(operand_value(1)?),
            result_bits,
        )),
        InstructionOpcode::Mul => Some(mask(
            operand_value(0)?.wrapping_mul(operand_value(1)?),
            result_bits,
        )),
        InstructionOpcode::UDiv => {
            let (a, b) = (operand_value(0)?, operand_value(1)?);
            if b == 0 {
                return None;
            }
            Some(mask(a / b, result_bits))
        }
        InstructionOpcode::SDiv => {
            let bits = operand_bits(0)?;
            let (a, b) = (
                sign_extend(operand_value(0)?, bits),
                sign_extend(operand_value(1)?, bits),
            );
            if b == 0 {
                return None;
            }
            Some(mask(a.wrapping_div(b) as u64, result_bits))
        }
        InstructionOpcode::URem => {
            let (a, b) = (operand_value(0)?, operand_value(1)?);
            if b == 0 {
                return None;
            }
            Some(mask(a % b, result_bits))
        }
        InstructionOpcode::SRem => {
            let bits = operand_bits(0)?;
            let (a, b) = (
                sign_extend(operand_value(0)?, bits),
                sign_extend(operand_value(1)?, bits),
            );
            if b == 0 {
                return None;
            }
            Some(mask(a.wrapping_rem(b) as u64, result_bits))
        }
        InstructionOpcode::And => Some(mask(operand_value(0)? & operand_value(1)?, result_bits)),
        InstructionOpcode::Or => Some(mask(operand_value(0)? | operand_value(1)?, result_bits)),
        InstructionOpcode::Xor => Some(mask(operand_value(0)? ^ operand_value(1)?, result_bits)),
        InstructionOpcode::Shl => {
            let shift = operand_value(1)?;
            if shift >= result_bits as u64 {
                return None;
            }
            Some(mask(
                operand_value(0)?.wrapping_shl(shift as u32),
                result_bits,
            ))
        }
        InstructionOpcode::LShr => {
            let shift = operand_value(1)?;
            if shift >= result_bits as u64 {
                return None;
            }
            Some(mask(
                operand_value(0)?.wrapping_shr(shift as u32),
                result_bits,
            ))
        }
        InstructionOpcode::AShr => {
            let bits = operand_bits(0)?;
            let shift = operand_value(1)?;
            if shift >= bits as u64 {
                return None;
            }
            Some(mask(
                (sign_extend(operand_value(0)?, bits) >> shift) as u64,
                result_bits,
            ))
        }
        InstructionOpcode::ICmp => {
            let bits = operand_bits(0)?;
            let (a, b) = (operand_value(0)?, operand_value(1)?);
            let pred = instr.get_icmp_predicate()?;
            let result = match pred {
                IntPredicate::EQ => a == b,
                IntPredicate::NE => a != b,
                IntPredicate::UGT => a > b,
                IntPredicate::UGE => a >= b,
                IntPredicate::ULT => a < b,
                IntPredicate::ULE => a <= b,
                IntPredicate::SGT => sign_extend(a, bits) > sign_extend(b, bits),
                IntPredicate::SGE => sign_extend(a, bits) >= sign_extend(b, bits),
                IntPredicate::SLT => sign_extend(a, bits) < sign_extend(b, bits),
                IntPredicate::SLE => sign_extend(a, bits) <= sign_extend(b, bits),
            };
            Some(result as u64)
        }
        InstructionOpcode::Select => {
            let cond = operand_value(0)?;
            if cond != 0 {
                operand_value(1)
            } else {
                operand_value(2)
            }
            .map(|v| mask(v, result_bits))
        }
        InstructionOpcode::Trunc | InstructionOpcode::ZExt => {
            Some(mask(operand_value(0)?, result_bits))
        }
        InstructionOpcode::SExt => {
            let bits = operand_bits(0)?;
            Some(mask(
                sign_extend(operand_value(0)?, bits) as u64,
                result_bits,
            ))
        }
        _ => None,
    }
}
