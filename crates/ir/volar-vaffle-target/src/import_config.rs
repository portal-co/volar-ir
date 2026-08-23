use alloc::{collections::BTreeMap, string::String};
use volar_side::SideId;

/// How a WAFFLE function import maps to an oracle or action.
pub enum WaffleImportKind {
    /// Pure oracle — all WAFFLE params → `OracleDecl::params`; WAFFLE results → `OracleDecl::results`.
    Oracle {
        name: String,
        /// Side to attach to the call/output values emitted at each call
        /// site, if any (see `volar-side`).
        side: Option<SideId>,
    },
    /// Action — WAFFLE calling convention:
    ///   params = [guard (i32), arg_0 .. arg_{n_args-1}, fallback_0 .. fallback_{n_results-1}]
    ///   results = [result_0 .. result_{n_results-1}]
    Action {
        name: String,
        n_args: usize,
        /// Side to attach to the call/output values emitted at each call
        /// site, if any (see `volar-side`).
        side: Option<SideId>,
    },
}

/// Maps WAFFLE import names to their oracle/action declarations.
///
/// Pass to [`lower_waffle_module`](crate::lower_waffle_module) so that
/// matching imports are registered as `OracleDecl`/`ActionDecl` entries in
/// the output VAFFLE module and routed through the oracle/action calling
/// convention at every call site.
pub struct WaffleImportConfig {
    pub imports: BTreeMap<String, WaffleImportKind>,
    memory_address_bits: Option<usize>,
}

impl WaffleImportConfig {
    pub fn new() -> Self {
        Self { imports: BTreeMap::new(), memory_address_bits: None }
    }

    pub fn with_oracle(
        mut self,
        waffle_name: impl Into<String>,
        oracle_name: impl Into<String>,
    ) -> Self {
        self.imports.insert(
            waffle_name.into(),
            WaffleImportKind::Oracle { name: oracle_name.into(), side: None },
        );
        self
    }

    pub fn with_action(
        mut self,
        waffle_name: impl Into<String>,
        action_name: impl Into<String>,
        n_args: usize,
    ) -> Self {
        self.imports.insert(
            waffle_name.into(),
            WaffleImportKind::Action { name: action_name.into(), n_args, side: None },
        );
        self
    }

    /// Like [`with_oracle`](Self::with_oracle), but attaches `side` to every
    /// call/output value emitted at this oracle's call sites.
    pub fn with_oracle_side(
        mut self,
        waffle_name: impl Into<String>,
        oracle_name: impl Into<String>,
        side: SideId,
    ) -> Self {
        self.imports.insert(
            waffle_name.into(),
            WaffleImportKind::Oracle { name: oracle_name.into(), side: Some(side) },
        );
        self
    }

    /// Like [`with_action`](Self::with_action), but attaches `side` to every
    /// call/output value emitted at this action's call sites.
    pub fn with_action_side(
        mut self,
        waffle_name: impl Into<String>,
        action_name: impl Into<String>,
        n_args: usize,
        side: SideId,
    ) -> Self {
        self.imports.insert(
            waffle_name.into(),
            WaffleImportKind::Action { name: action_name.into(), n_args, side: Some(side) },
        );
        self
    }

    /// Limit the low address bits presented to memory storage operations.
    /// Effective-address arithmetic remains full-width; the default (`None`)
    /// preserves WASM's 32-bit byte-address behavior.
    pub fn with_memory_address_bits(mut self, bits: usize) -> Self {
        assert!(bits <= 32, "WASM memory addresses are 32 bits, got {bits}");
        self.memory_address_bits = Some(bits);
        self
    }

    pub fn memory_address_bits(&self) -> Option<usize> { self.memory_address_bits }
}

impl Default for WaffleImportConfig {
    fn default() -> Self { Self::new() }
}
