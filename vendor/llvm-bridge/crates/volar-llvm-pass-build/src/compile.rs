//! Compiles a generated shim source (see [`crate::shim`]) and emits the
//! linker arguments a `cdylib`-crate-type pass plugin needs to actually
//! expose `llvmGetPassPluginInfo` to `clang`/`opt`.

use std::path::Path;

/// Write `source` to `{out_dir}/{archive_name}.cpp`, compile it via `cc`
/// into a static archive named `{archive_name}`, and emit the
/// `cargo:rustc-cdylib-link-arg` lines that force-retain
/// `llvmGetPassPluginInfo` in the final cdylib.
///
/// Cargo links `cc`'s output as a static archive; ordinary archive
/// extraction would discard the object containing `llvmGetPassPluginInfo`
/// since nothing in Rust calls into it directly (only the reverse — C++
/// calling the plugin's Rust entry points). `-force_load`/`--whole-archive`
/// plus an explicit exported-symbol directive is what keeps it.
pub fn compile_and_link_shim(source: &str, out_dir: &Path, includedir: &str, archive_name: &str) {
    let cpp_path = out_dir.join(format!("{archive_name}.cpp"));
    std::fs::write(&cpp_path, source)
        .unwrap_or_else(|error| panic!("write {}: {error}", cpp_path.display()));
    println!("cargo:rerun-if-changed={}", cpp_path.display());

    cc::Build::new()
        .cpp(true)
        .file(&cpp_path)
        .include(includedir)
        .flag_if_supported("-std=c++17")
        .warnings(false)
        .compile(archive_name);

    let archive = out_dir.join(format!("lib{archive_name}.a"));
    if cfg!(target_os = "macos") {
        println!(
            "cargo:rustc-cdylib-link-arg=-Wl,-force_load,{}",
            archive.display()
        );
        println!("cargo:rustc-cdylib-link-arg=-Wl,-exported_symbol,_llvmGetPassPluginInfo");
    } else {
        println!(
            "cargo:rustc-cdylib-link-arg=-Wl,--whole-archive,{},--no-whole-archive",
            archive.display()
        );
        println!("cargo:rustc-cdylib-link-arg=-Wl,--export-dynamic-symbol=llvmGetPassPluginInfo");
    }
}
