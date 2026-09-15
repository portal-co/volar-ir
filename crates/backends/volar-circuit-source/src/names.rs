// @reliability: normal
// @ai: assisted

use std::collections::BTreeMap;

use volar_ir::ir::IRVarId;
use volar_ir_common::{StorageId, StoragePurpose, StorageRegistry};

use crate::error::EmitError;

/// Optional explicit names for SSA wires and storage spaces.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WireNames {
    vars: BTreeMap<IRVarId, String>,
    storages: BTreeMap<StorageId, String>,
}

impl WireNames {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn name_var(&mut self, id: IRVarId, name: impl Into<String>) -> &mut Self {
        self.vars.insert(id, name.into());
        self
    }

    pub fn name_storage(&mut self, id: StorageId, name: impl Into<String>) -> &mut Self {
        self.storages.insert(id, name.into());
        self
    }

    pub fn var(&self, id: IRVarId) -> Option<&str> {
        self.vars.get(&id).map(String::as_str)
    }

    pub fn storage(&self, id: StorageId) -> Option<&str> {
        self.storages.get(&id).map(String::as_str)
    }

    pub fn is_explicit_var(&self, id: IRVarId) -> bool {
        self.vars.contains_key(&id)
    }

    /// Name every storage recorded in a purpose registry, without
    /// overriding names already set. Labels come from each purpose's
    /// `Display` (e.g. `stack`, `wasm-memory-0`, `virt-regfile-3`), so
    /// backend output is human-readable for free.
    pub fn name_storages_from_purposes(
        &mut self,
        registry: &StorageRegistry<StoragePurpose>,
    ) -> &mut Self {
        for (id, purpose) in registry.iter() {
            self.storages.entry(id).or_insert_with(|| purpose.to_string());
        }
        self
    }
}

/// Package-level emission options.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmitOptions {
    /// Nargo package name / Podlang module name.
    pub package_name: String,
    /// Exported function / root predicate name.
    pub circuit_name: String,
    /// Optional `IRVarId` / `StorageId` → identifier overrides.
    pub wires: WireNames,
    /// When true, every wire is bound even if it has a single use.
    pub name_all_wires: bool,
}

impl Default for EmitOptions {
    fn default() -> Self {
        Self {
            package_name: String::from("volar_circuit"),
            circuit_name: String::from("eval_circuit"),
            wires: WireNames::new(),
            name_all_wires: false,
        }
    }
}

/// True when `s` is an ASCII identifier `[A-Za-z_][A-Za-z0-9_]*`.
pub fn is_ascii_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Reject empty or non-ASCII-ident strings after sanitization.
pub fn require_ident(name: &str, reason: &str) -> Result<(), EmitError> {
    if is_ascii_ident(name) {
        Ok(())
    } else {
        Err(EmitError::InvalidIdent {
            name: name.to_string(),
            reason: reason.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use volar_ir_common::{StoragePurpose, StorageRegistry, VirtStorageRole};

    #[test]
    fn name_storages_from_purposes_fills_without_overriding() {
        let mut registry = StorageRegistry::<StoragePurpose>::new();
        let stack = registry.register(StoragePurpose::Stack);
        let mem = registry.register(StoragePurpose::WasmMemory { index: 3 });
        let virt = registry.register(StoragePurpose::Virt {
            role: VirtStorageRole::RegisterFile,
            detail: 2,
        });

        let mut names = WireNames::new();
        names.name_storage(stack, "custom-stack");
        names.name_storages_from_purposes(&registry);

        // Pre-set name wins; the rest come from the purposes' Display.
        assert_eq!(names.storage(stack), Some("custom-stack"));
        assert_eq!(names.storage(mem), Some("wasm-memory-3"));
        assert_eq!(names.storage(virt), Some("virt-regfile-2"));
    }
}
