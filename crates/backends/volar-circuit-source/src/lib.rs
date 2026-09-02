// @reliability: normal
// @ai: assisted
//! Generic textual source-code emitter for fused circuits.
//!
//! Walk [`BCircuit`] / [`VCircuit`], optionally name internal wires, then
//! hand a [`NamedBoolCircuit`] / [`NamedVolarCircuit`] to a
//! [`CircuitSourceBackend`]. The output is a reusable [`SourcePackage`]
//! (library files), not a prover `main` / `REQUEST`.

use volar_ir::circuit::{BCircuit, VCircuit};
use volar_ir::ir::IRTypes;

mod caps;
mod error;
mod eval;
mod named;
mod names;
mod package;
mod types;
mod walk;

pub use caps::{BackendCaps, CircuitSourceBackend};
pub use error::EmitError;
pub use eval::{bits_to_u64, const_bit, eval_named_bool, BoolStorageMap};
pub use named::{
    ExternalKind, NamedBoolCircuit, NamedBoolOp, NamedBoolStmt, NamedVolarCircuit, NamedVolarOp,
    NamedVolarParam, NamedVolarStmt, NamedWire,
};
pub use names::{is_ascii_ident, require_ident, EmitOptions, WireNames};
pub use package::{SourceFile, SourcePackage};
pub use types::{bit_type_id, ir_type_bit_width, type_is_bit};
pub use walk::{name_bool_circuit, name_volar_circuit};

/// Name a Boolar circuit and render it with `backend`.
pub fn emit_bool_circuit<B: CircuitSourceBackend, P: Clone>(
    circuit: &BCircuit<P>,
    backend: &B,
    opt: &EmitOptions,
) -> Result<SourcePackage, EmitError> {
    let caps = B::caps();
    if !caps.bool_circuit {
        return Err(EmitError::unsupported(
            "boolean circuits",
            "this backend does not implement bool emission",
        ));
    }
    let named = name_bool_circuit(circuit, opt, B::sanitize_ident)?;
    check_named_bool(&named, &caps)?;
    backend.emit_bool(&named, opt)
}

/// Name a Volar circuit and render it with `backend`.
pub fn emit_volar_circuit<B: CircuitSourceBackend, P: Clone>(
    circuit: &VCircuit<P>,
    types: &IRTypes,
    backend: &B,
    opt: &EmitOptions,
) -> Result<SourcePackage, EmitError> {
    let caps = B::caps();
    if !caps.volar_ir {
        return Err(EmitError::unsupported(
            "Volar IR",
            "this backend does not implement Volar-IR emission",
        ));
    }
    let named = name_volar_circuit(circuit, types, opt, B::sanitize_ident)?;
    check_named_volar(&named, &caps)?;
    backend.emit_volar(&named, opt)
}

fn check_named_bool(named: &NamedBoolCircuit, caps: &BackendCaps) -> Result<(), EmitError> {
    if named.has_externals() {
        return Err(EmitError::unsupported(
            "oracle/action/rng",
            "v1 circuit-source backends reject externals",
        ));
    }
    if named.has_storage() && !caps.supports_storage() {
        return Err(EmitError::unsupported(
            "storage",
            "run StorageToMux or use the POD2 backend",
        ));
    }
    Ok(())
}

fn check_named_volar(named: &NamedVolarCircuit, caps: &BackendCaps) -> Result<(), EmitError> {
    if named.has_externals() {
        return Err(EmitError::unsupported(
            "oracle/action/rng",
            "v1 circuit-source backends reject externals",
        ));
    }
    if named.has_storage() && !caps.supports_storage() {
        return Err(EmitError::unsupported(
            "storage",
            "run StorageToMux or use the POD2 backend",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use volar_ir::boolar::BIrStmt;
    use volar_ir::ir::IRVarId;

    fn and_circuit() -> BCircuit<()> {
        let mut c = BCircuit::new(2);
        c.push_stmt(BIrStmt::And(IRVarId(0), IRVarId(1)), ());
        c.outputs = vec![IRVarId(2)];
        c
    }

    struct Dummy;

    impl CircuitSourceBackend for Dummy {
        fn caps() -> BackendCaps {
            BackendCaps {
                bool_circuit: true,
                volar_ir: false,
                merkle_storage: false,
                array_storage: false,
            }
        }

        fn sanitize_ident(name: &str) -> Result<String, EmitError> {
            if is_ascii_ident(name) {
                Ok(name.to_string())
            } else {
                Err(EmitError::InvalidIdent {
                    name: name.to_string(),
                    reason: "not an ASCII ident".into(),
                })
            }
        }

        fn emit_bool(
            &self,
            circuit: &NamedBoolCircuit,
            _opt: &EmitOptions,
        ) -> Result<SourcePackage, EmitError> {
            Ok(SourcePackage::new(vec![SourceFile::new(
                "lib.txt",
                format!("{} stmts", circuit.stmts.len()),
            )]))
        }

        fn emit_volar(
            &self,
            _circuit: &NamedVolarCircuit,
            _opt: &EmitOptions,
        ) -> Result<SourcePackage, EmitError> {
            Err(EmitError::unsupported("Volar IR", "dummy"))
        }
    }

    #[test]
    fn names_params_and_results() {
        let named = name_bool_circuit(&and_circuit(), &EmitOptions::default(), Dummy::sanitize_ident)
            .unwrap();
        assert_eq!(named.inputs[0].name, "in0");
        assert_eq!(named.inputs[1].name, "in1");
        assert_eq!(named.stmts[0].dst.name, "w2");
        assert_eq!(named.outputs, vec![IRVarId(2)]);
    }

    #[test]
    fn explicit_name_forces_bind() {
        let mut opt = EmitOptions::default();
        opt.wires.name_var(IRVarId(2), "carry");
        let named = name_bool_circuit(&and_circuit(), &opt, Dummy::sanitize_ident).unwrap();
        assert_eq!(named.stmts[0].dst.name, "carry");
        assert!(named.stmts[0].dst.bind);
    }

    #[test]
    fn collision_is_an_error() {
        let mut opt = EmitOptions::default();
        opt.wires.name_var(IRVarId(0), "x");
        opt.wires.name_var(IRVarId(1), "x");
        let err = name_bool_circuit(&and_circuit(), &opt, Dummy::sanitize_ident).unwrap_err();
        assert!(matches!(err, EmitError::NameCollision { .. }));
    }

    #[test]
    fn oracle_is_rejected_at_emit() {
        let mut c = BCircuit::new(1);
        c.push_stmt(
            BIrStmt::OracleCall {
                name: "o".into(),
                args: vec![],
                num_bits: 1,
            },
            (),
        );
        c.outputs = vec![IRVarId(1)];
        let err = emit_bool_circuit(&c, &Dummy, &EmitOptions::default()).unwrap_err();
        assert!(matches!(err, EmitError::Unsupported { .. }));
    }

    #[test]
    fn eval_and() {
        let named = name_bool_circuit(&and_circuit(), &EmitOptions::default(), Dummy::sanitize_ident)
            .unwrap();
        let mut st = BoolStorageMap::new();
        assert_eq!(
            eval_named_bool(&named, &[true, true], &mut st).unwrap(),
            vec![true]
        );
        assert_eq!(
            eval_named_bool(&named, &[true, false], &mut st).unwrap(),
            vec![false]
        );
    }
}
