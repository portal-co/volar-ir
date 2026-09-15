//! `llvm-config` discovery for a pass-plugin crate's `build.rs`.
//!
//! Extracted from `cirrus-llvm-pass`'s original `build.rs`, generalized so a
//! second consumer (a future Volar-side plugin) doesn't have to duplicate
//! the same resolution order under a different env var name.

use std::env;
use std::path::PathBuf;
use std::process::Command;

/// Result of a successful [`probe`] call.
pub struct LlvmConfigProbe {
    /// Path to (or bare name of) the resolved `llvm-config` binary.
    pub llvm_config: String,
    /// `llvm-config --version` output, e.g. `"22.1.8"`.
    pub version: String,
    /// `llvm-config --includedir` output.
    pub includedir: String,
}

/// Resolve an `llvm-config` binary reporting `required_major` (e.g. `"22"`),
/// in this order:
///
/// 1. `$config_env` (e.g. `CIRRUS_LLVM_CONFIG`), used verbatim.
/// 2. `$sys_prefix_env/bin/llvm-config` (e.g. `LLVM_SYS_221_PREFIX`, the
///    `llvm-sys` crate's own override convention).
/// 3. A fixed candidate list: `llvm-config`, `llvm-config-{required_major}`,
///    the Homebrew `llvm@{required_major}` formula path, and the Debian
///    `/usr/lib/llvm-{required_major}` path — the first one that answers
///    `--version` successfully wins.
/// 4. `"llvm-config"` unqualified, letting the eventual `--version` check
///    below produce a clear failure rather than a silent wrong pick.
///
/// Emits `cargo:rerun-if-env-changed` for both env vars. Panics (as a
/// `build.rs` should) if the resolved binary doesn't report
/// `required_major`.
pub fn probe(config_env: &str, sys_prefix_env: &str, required_major: &str) -> LlvmConfigProbe {
    println!("cargo:rerun-if-env-changed={config_env}");
    println!("cargo:rerun-if-env-changed={sys_prefix_env}");

    let llvm_config = env::var(config_env).unwrap_or_else(|_| {
        env::var(sys_prefix_env)
            .map(|prefix| {
                PathBuf::from(prefix)
                    .join("bin/llvm-config")
                    .to_string_lossy()
                    .into_owned()
            })
            .unwrap_or_else(|_| {
                let candidates = [
                    "llvm-config".to_owned(),
                    format!("llvm-config-{required_major}"),
                    format!("/opt/homebrew/opt/llvm@{required_major}/bin/llvm-config"),
                    format!("/usr/lib/llvm-{required_major}/bin/llvm-config"),
                ];
                candidates
                    .into_iter()
                    .find(|candidate| Command::new(candidate).arg("--version").output().is_ok())
                    .unwrap_or_else(|| "llvm-config".to_owned())
            })
    });

    let version = query(&llvm_config, "--version");
    assert!(
        version.starts_with(&format!("{required_major}.")),
        "requires LLVM {required_major}, but {llvm_config} reports {version}",
    );
    let includedir = query(&llvm_config, "--includedir");

    LlvmConfigProbe {
        llvm_config,
        version,
        includedir,
    }
}

fn query(llvm_config: &str, argument: &str) -> String {
    let output = Command::new(llvm_config)
        .arg(argument)
        .output()
        .unwrap_or_else(|error| panic!("run {llvm_config} {argument}: {error}"));
    if !output.status.success() {
        panic!(
            "{llvm_config} {argument} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    String::from_utf8(output.stdout)
        .expect("llvm-config output is UTF-8")
        .trim()
        .to_owned()
}
