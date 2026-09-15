#![no_std]

extern crate alloc;

pub mod apply_gadgets;
pub mod cfg_layout;
pub mod dispatch_accumulator;
pub mod from_reversible;
pub mod fuse_to_circuit;
pub mod lower_ir_to_boolar;
pub mod lower_lir;
pub mod lower_to_circuit;
pub mod movfuscate;
pub mod raise_to_z3;
pub mod region_lowering;
pub mod storage_to_mux_boolar;
pub mod storage_to_mux_ir;
pub mod to_reversible;
pub mod unroll_ir;

pub use apply_gadgets::{GadgetApplication, apply_gadgets};
pub use cfg_layout::{CfgLayoutReport, layout_cfg_for_movfuscation};
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
    movfuscate_biir_with_control_provenance, movfuscate_ir, movfuscate_ir_owned,
    movfuscate_ir_with_boundary, movfuscate_ir_with_boundary_and_watch,
    movfuscate_ir_with_control_provenance, pc_bits_needed, remap_movfusc_accum_info,
    remap_movfusc_boundaries, remap_movfusc_boundary, thread_synthetic_slots,
};
pub use raise_to_z3::raise_bits_to_z3;
pub use storage_to_mux_boolar::{
    StorageToMuxBoolarConfig, StorageToMuxBoolarError, storage_to_mux_boolar,
};
pub use storage_to_mux_ir::{StorageToMuxConfig, StorageToMuxError, storage_to_mux_ir};
pub use to_reversible::{
    ReversibleMode, ToReversibleError, UnknownVar, ValueWatchlist, VarWireMap, WireWatchEntry,
    WireWatchlist, to_reversible, to_reversible_with_mode, translate_watchlist,
};
pub use unroll_ir::{
    CfgSegmentUnroll, PrefixUnroll, PrefixUnrollOutcome, UnrollError, UnrollLimits,
    unroll_cfg_segments_with_limits, unroll_ir_everything, unroll_ir_everything_unbounded,
    unroll_ir_everything_with_limits, unroll_ir_prefix_with_limits,
};
