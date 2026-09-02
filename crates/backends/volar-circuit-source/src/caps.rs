// @reliability: normal
// @ai: assisted

use crate::error::EmitError;
use crate::named::{NamedBoolCircuit, NamedVolarCircuit};
use crate::names::EmitOptions;
use crate::package::SourcePackage;

/// What a [`CircuitSourceBackend`] can emit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackendCaps {
    pub bool_circuit: bool,
    pub volar_ir: bool,
    pub merkle_storage: bool,
    pub array_storage: bool,
}

impl BackendCaps {
    pub fn none() -> Self {
        Self {
            bool_circuit: false,
            volar_ir: false,
            merkle_storage: false,
            array_storage: false,
        }
    }

    pub fn supports_storage(&self) -> bool {
        self.merkle_storage || self.array_storage
    }
}

/// Language adapter: render a named circuit as a reusable source package.
pub trait CircuitSourceBackend {
    fn caps() -> BackendCaps;

    fn sanitize_ident(name: &str) -> Result<String, EmitError>;

    fn emit_bool(
        &self,
        circuit: &NamedBoolCircuit,
        opt: &EmitOptions,
    ) -> Result<SourcePackage, EmitError>;

    fn emit_volar(
        &self,
        circuit: &NamedVolarCircuit,
        opt: &EmitOptions,
    ) -> Result<SourcePackage, EmitError>;
}
