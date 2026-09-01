#![no_std]

use alloc::{collections::btree_map::BTreeMap, vec::Vec};
extern crate alloc;
pub mod boolar;
pub mod circuit;
pub mod gadget;
pub mod ir;
pub mod region;
pub mod typed_gadget;
#[cfg(feature = "lazy")]
pub mod lazy;
pub mod public;
pub mod rcircuit;

/// Re-export provenance handler types for convenience.
pub use volar_provenance::{self, KeepProvenance, MapProvenance, NoProvenance, ProvenanceHandler};
