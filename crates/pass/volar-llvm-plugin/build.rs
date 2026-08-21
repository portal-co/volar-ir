use std::env;
use std::path::PathBuf;

use volar_llvm_pass_build::{ShimConfig, compile_and_link_shim, generate_shim_source, probe};

fn main() {
    let llvm = probe("VOLAR_LLVM_CONFIG", "LLVM_SYS_221_PREFIX", "22");

    let config = ShimConfig {
        plugin_name: "volar-llvm-plugin".into(),
        plugin_version: "0.1".into(),
        link_anchor_symbol: "volar_llvm_plugin_link_anchor".into(),
        free_error_symbol: "volar_llvm_plugin_free_error".into(),
        passes: vec![volar_llvm_pass_build::PluginPass {
            pipeline_name: "volar-reimport".into(),
            run_symbol: "volar_llvm_plugin_run".into(),
            error_prefix: "volar-reimport".into(),
            // Runs only when explicitly named, matching cirrus-deloopify's
            // policy -- this pass rewrites function bodies wholesale
            // (unlike cirrus-lower, it isn't a "no-op unless a descriptor
            // opts in" pass), so auto-running it during ordinary/LTO
            // compiles would be a surprising default.
            auto_register: None,
        }],
    };
    let source = generate_shim_source(&config);

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR"));
    compile_and_link_shim(&source, &out_dir, &llvm.includedir, "volar_llvm_plugin_shim");
}
