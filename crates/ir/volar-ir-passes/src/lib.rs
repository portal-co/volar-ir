#![no_std]

extern crate alloc;

pub mod apply_gadgets;
pub mod dispatch_accumulator;
pub mod from_reversible;
pub mod fuse_to_circuit;
pub mod lower_ir_to_boolar;
pub mod lower_lir;
pub mod lower_to_circuit;
pub mod movfuscate;
pub mod raise_to_z3;
pub mod to_reversible;

pub use apply_gadgets::{apply_gadgets, GadgetApplication};
pub use from_reversible::{
    CircuitTransformError, hardcode_circuit_inputs, remove_circuit_outputs, to_boolar_circuit,
};
pub use fuse_to_circuit::{
    lower_to_circuit_fused, to_circuit_fused_boolar, to_circuit_fused_volar,
};
pub use lower_ir_to_boolar::{
    ir_type_bits, lower_ir_to_boolar, lower_ir_to_boolar_with_lane_table,
};
pub use lower_to_circuit::{
    LoweringMode, lower_to_circuit, lower_to_circuit_ir,
    lower_to_circuit_ir_with_control_provenance, lower_to_circuit_with_control_provenance,
};
pub use movfuscate::{
    MovfuscAccumInfo, MovfuscAccumInit, MovfuscAccumStep, MovfuscBlockBoundary, movfuscate_biir,
    movfuscate_biir_with_control_provenance, movfuscate_ir, movfuscate_ir_with_boundary,
    movfuscate_ir_with_control_provenance, pc_bits_needed, remap_movfusc_accum_info,
    remap_movfusc_boundaries, remap_movfusc_boundary, thread_synthetic_slots,
};
pub use raise_to_z3::raise_bits_to_z3;
pub use to_reversible::{
    ReversibleMode, ToReversibleError, UnknownVar, ValueWatchlist, VarWireMap, WireWatchEntry,
    WireWatchlist, to_reversible, to_reversible_with_mode, translate_watchlist,
};
