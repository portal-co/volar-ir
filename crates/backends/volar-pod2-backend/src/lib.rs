// @reliability: normal
// @ai: assisted
//! POD2 / Podlang backend for [`volar_circuit_source`].
//!
//! Boolean circuits become a reusable predicate module that *verifies* a
//! witness of wire values stored in a Merkle dictionary. Storage maps onto
//! Merkle arrays (`ArrayContains` / `ArrayUpdate`). Volar IR is unsupported.

use volar_circuit_source::{
    BackendCaps, CircuitSourceBackend, EmitError, EmitOptions, NamedBoolCircuit, NamedVolarCircuit,
    SourcePackage,
};

mod bool;
mod check;
mod ident;

pub use check::{
    check_bool_and, check_bool_not, check_bool_or, check_bool_xor, product, sum, verify_named_bool,
};
pub use ident::sanitize_ident;

/// Podlang module emitter.
#[derive(Clone, Copy, Debug, Default)]
pub struct Pod2Backend;

impl CircuitSourceBackend for Pod2Backend {
    fn caps() -> BackendCaps {
        BackendCaps {
            bool_circuit: true,
            volar_ir: false,
            merkle_storage: true,
            array_storage: true,
        }
    }

    fn sanitize_ident(name: &str) -> Result<String, EmitError> {
        ident::sanitize_ident(name)
    }

    fn emit_bool(
        &self,
        circuit: &NamedBoolCircuit,
        opt: &EmitOptions,
    ) -> Result<SourcePackage, EmitError> {
        bool::emit_bool(circuit, opt)
    }

    fn emit_volar(
        &self,
        _circuit: &NamedVolarCircuit,
        _opt: &EmitOptions,
    ) -> Result<SourcePackage, EmitError> {
        Err(EmitError::unsupported(
            "Volar IR",
            "POD2 backend emits boolean circuits and Merkle storage only",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use volar_circuit_source::{
        emit_bool_circuit, emit_volar_circuit, name_bool_circuit, EmitOptions, WireNames,
    };
    use volar_ir::boolar::BIrStmt;
    use volar_ir::circuit::{BCircuit, VCircuit};
    use volar_ir::ir::IRVarId;
    use volar_ir_common::TypeTable;

    fn and_circuit() -> BCircuit<()> {
        let mut c = BCircuit::new(2);
        c.push_stmt(BIrStmt::And(IRVarId(0), IRVarId(1)), ());
        c.outputs = vec![IRVarId(2)];
        c
    }

    #[test]
    fn module_not_request_only() {
        let pkg = emit_bool_circuit(&and_circuit(), &Pod2Backend, &EmitOptions::default()).unwrap();
        let src = pkg.file_text("volar_circuit.podlang").unwrap();
        assert!(src.contains("bool_and(a, b, out)"));
        assert!(src.contains("eval_circuit("));
        assert!(!src.contains("REQUEST("));
        assert!(pkg
            .file_text("examples/embed.podlang")
            .unwrap()
            .contains("REQUEST("));
    }

    #[test]
    fn named_wire_is_dict_key() {
        let mut opt = EmitOptions::default();
        opt.wires = WireNames::new();
        opt.wires.name_var(IRVarId(2), "carry");
        let pkg = emit_bool_circuit(&and_circuit(), &Pod2Backend, &opt).unwrap();
        let src = pkg.file_text("volar_circuit.podlang").unwrap();
        assert!(src.contains("\"carry\""));
        assert!(src.contains("bool_and("));
    }

    #[test]
    fn volar_is_rejected() {
        let mut types = TypeTable::new();
        let bit = types.bit();
        let c = VCircuit::<()>::new(vec![bit]);
        let err = emit_volar_circuit(&c, &types, &Pod2Backend, &EmitOptions::default()).unwrap_err();
        assert!(matches!(err, EmitError::Unsupported { .. }));
    }

    #[test]
    fn product_sum_matches_eval() {
        let named =
            name_bool_circuit(&and_circuit(), &EmitOptions::default(), sanitize_ident)
                .unwrap();
        assert!(verify_named_bool(&named, &[true, true]).unwrap());
        assert!(verify_named_bool(&named, &[true, false]).unwrap());
    }

    #[test]
    fn storage_update_is_emitted() {
        let mut c = BCircuit::new(2);
        c.push_stmt(BIrStmt::Zero, ());
        c.push_stmt(
            BIrStmt::StorageWrite {
                storage: volar_ir_common::StorageId(0),
                lane: volar_ir::boolar::LaneId(0),
                src: IRVarId(0),
                addr: vec![IRVarId(2)],
            },
            (),
        );
        c.outputs = vec![IRVarId(0)];
        let pkg = emit_bool_circuit(&c, &Pod2Backend, &EmitOptions::default()).unwrap();
        let src = pkg.file_text("volar_circuit.podlang").unwrap();
        assert!(src.contains("ArrayUpdate"));
        assert!(src.contains("mem0"));
    }
}
