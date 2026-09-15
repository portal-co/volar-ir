//! Build-dependencies-only helpers shared by LLVM New-PM pass-plugin
//! crates. Consume this from a `build.rs`, never from ordinary source —
//! it has no runtime LLVM binding of its own (see `volar-llvm-pass-support`
//! for the runtime side: entry-point wrapping and marker scanning).
//!
//! A plugin's `build.rs` typically does:
//!
//! ```ignore
//! let probe = volar_llvm_pass_build::probe("MY_LLVM_CONFIG", "LLVM_SYS_221_PREFIX", "22");
//! let source = volar_llvm_pass_build::generate_shim_source(&ShimConfig { .. });
//! let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
//! volar_llvm_pass_build::compile_and_link_shim(&source, &out_dir, &probe.includedir, "my_plugin_shim");
//! ```

mod compile;
mod probe;
mod shim;

pub use compile::compile_and_link_shim;
pub use probe::{LlvmConfigProbe, probe};
pub use shim::{
    AutoRegister, PluginPass, ShimConfig, generate_full_lto_shim_source, generate_shim_source,
};
