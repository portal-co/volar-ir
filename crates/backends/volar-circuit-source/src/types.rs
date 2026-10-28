// @reliability: normal
// @ai: assisted

use volar_ir::ir::IRTypes;
use volar_ir_common::{IrType, Type, TypeId};

use crate::error::EmitError;

/// Bit width of a supported Volar IR type. Fail-closed on wide/field types.
pub fn ir_type_bit_width(types: &IRTypes, id: TypeId) -> Result<usize, EmitError> {
    let ty = types.0.get(id.0 as usize).ok_or_else(|| EmitError::TypeUnsupported {
        ty: format!("unknown type id {}", id.0),
    })?;
    match ty {
        // The `Type::_n` cutover: only Bit is a primitive; a wider bit-vector
        // is `Vec(n, Bit)` (handled by the Vec arm below).
        IrType::Primitive(Type::Bit) => Ok(1),
        IrType::PrimeField { .. } => Err(EmitError::TypeUnsupported {
            ty: "PrimeField".into(),
        }),
        IrType::ExtField { .. } => Err(EmitError::TypeUnsupported {
            ty: "ExtField".into(),
        }),
        IrType::Vec(n, elem) => Ok(n.saturating_mul(ir_type_bit_width(types, *elem)?)),
        IrType::Tuple(fields) => {
            let mut sum = 0usize;
            for f in fields {
                sum = sum.saturating_add(ir_type_bit_width(types, *f)?);
            }
            Ok(sum)
        }
        IrType::Block { .. } => Err(EmitError::TypeUnsupported {
            ty: "Block".into(),
        }),
        IrType::Func { .. } => Err(EmitError::TypeUnsupported { ty: "Func".into() }),
        _ => panic!("ir_type_bit_width: unhandled IrType variant — add lowering for this variant"),
    }
}

/// Locate the interned `Bit` primitive, if the table has one.
pub fn bit_type_id(types: &IRTypes) -> Result<TypeId, EmitError> {
    types
        .0
        .iter()
        .position(|t| matches!(t, IrType::Primitive(Type::Bit)))
        .map(|i| TypeId(i as u32))
        .ok_or_else(|| EmitError::TypeUnsupported {
            ty: "circuit has no interned Bit type".into(),
        })
}

pub fn type_is_bit(types: &IRTypes, id: TypeId) -> bool {
    types.is_bit(id)
}
