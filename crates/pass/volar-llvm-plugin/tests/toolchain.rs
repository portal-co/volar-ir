//! End-to-end coverage for the dynamically loaded `volar-llvm-plugin`.
//!
//! Mirrors `cirrus-llvm-pass/tests/toolchain.rs`'s harness style (installed
//! `clang`/`opt` rather than inkwell, so the real dynamically-loaded plugin
//! boundary is what's under test), but scoped down to what this plugin
//! actually needs to prove: `opt -load-pass-plugin=... -passes=volar-reimport`
//! loads it and runs the full jump-thread -> structural-VAFFLE-import ->
//! replay round trip on a marked `i1`-shaped function, producing a
//! reimplementation that still computes the right answer -- both as
//! inspectable IR text and as an executable that's actually run. Set
//! `VOLAR_REQUIRE_EXTERNAL_LLVM_TESTS=1` in CI to turn an unavailable LLVM
//! 22 toolchain into a test failure instead of a skip.

use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const REQUIRED: &str = "VOLAR_REQUIRE_EXTERNAL_LLVM_TESTS";

struct Tools {
    clang: PathBuf,
    opt: PathBuf,
    plugin: PathBuf,
    root: PathBuf,
}

impl Tools {
    fn discover() -> Option<Self> {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).ancestors().nth(3)?.to_path_buf();
        let llvm_bin =
            env::var_os("VOLAR_LLVM_CONFIG").map(PathBuf::from).and_then(|path| path.parent().map(Path::to_path_buf));
        let clang = find_llvm_tool("clang", llvm_bin.as_deref(), &["clang-22", "clang", "/opt/homebrew/opt/llvm@22/bin/clang"])?;
        let opt = find_llvm_tool(
            "opt",
            llvm_bin.as_deref().or_else(|| clang.parent()),
            &["opt-22", "opt", "/opt/homebrew/opt/llvm@22/bin/opt"],
        )?;

        let target = cargo_target_dir(&root);
        // `cargo test` only guarantees an rlib for this package. Build the
        // cdylib explicitly so the plugin loaded below exactly matches the
        // source under test instead of a stale developer artifact.
        run(Command::new(env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo")))
            .current_dir(&root)
            .arg("build")
            .arg("--quiet")
            .arg("-p")
            .arg("volar-llvm-plugin"));
        let plugin = target.join("debug").join(dynamic_library_name("volar_llvm_plugin"));
        if !plugin.is_file() {
            return None;
        }
        Some(Tools { clang, opt, plugin, root })
    }

    fn header_dir(&self) -> PathBuf {
        self.root.join("crates/pass/volar-llvm-plugin-api/include")
    }

    fn fixtures_dir(&self) -> PathBuf {
        self.root.join("crates/pass/volar-llvm-plugin/tests/fixtures")
    }
}

#[test]
fn clang_and_opt_load_the_plugin_and_execute_the_reimport() {
    let Some(tools) = Tools::discover() else {
        if env::var_os(REQUIRED).is_some() {
            panic!("{REQUIRED}=1 requires Clang and opt built with LLVM 22");
        }
        eprintln!("skipping external volar-llvm-plugin test: compatible LLVM 22 tools unavailable");
        return;
    };

    let output = env::temp_dir().join(format!("volar-llvm-plugin-{}", std::process::id()));
    fs::create_dir_all(&output).expect("create test output directory");
    let fixtures = tools.fixtures_dir();
    let header = tools.header_dir();

    // `volar-reimport` is registered only under its own pipeline name (see
    // build.rs's `auto_register: None`), matching `cirrus-deloopify`'s own
    // policy -- it rewrites function bodies wholesale, so auto-running it
    // during an ordinary `-fpass-plugin=`-driven `-O2` compile would be a
    // surprising default. It must be invoked explicitly via `opt
    // -passes=volar-reimport`, which is exactly what a real integration
    // would do (typically chained: `-passes=volar-reimport,default<O2>`).
    // `-O1` (not `-O0`): `volar-llvm-vaffle-import` is a pure-SSA structural
    // importer with no memory model (unlike `cirrus-llvm-frontend`'s
    // execution-mode one) -- it doesn't understand `alloca`, which `-O0`
    // emits for every local *and* which `mem2reg` cannot clean up on top,
    // since `-O0` also marks every function `optnone` (skipping every real
    // optimization pass, `mem2reg` included). `-O1` runs mem2reg itself as
    // part of its ordinary pipeline, producing plain SSA `xor i1`s.
    let plain_ir = output.join("xor3.ll");
    run(Command::new(&tools.clang)
        .arg("-O1")
        .arg("-S")
        .arg("-emit-llvm")
        .arg(format!("-I{}", header.display()))
        .arg(fixtures.join("xor3.c"))
        .arg("-o")
        .arg(&plain_ir));
    let opt_ir = output.join("xor3.opt.ll");
    run(Command::new(&tools.opt)
        .arg(format!("-load-pass-plugin={}", tools.plugin.display()))
        .arg("-passes=volar-reimport")
        .arg("-S")
        .arg(&plain_ir)
        .arg("-o")
        .arg(&opt_ir));
    let opt_text = fs::read_to_string(&opt_ir).expect("read opt output");
    assert!(!opt_text.contains("call void @__volar_entry"), "marker call must be erased:\n{opt_text}");
    assert!(!opt_text.contains("declare void @__volar_entry"), "marker declaration must be erased:\n{opt_text}");
    assert!(opt_text.contains("@xor3"), "the reimported function must take over the `xor3` name:\n{opt_text}");
    assert!(opt_text.contains(".volar_orig"), "the original body must survive, renamed out of the way:\n{opt_text}");

    // The `opt`-transformed IR must still compile and run correctly too --
    // not just contain the right names.
    let opt_object = output.join("xor3.opt.o");
    run(Command::new(&tools.clang).arg("-c").arg(&opt_ir).arg("-o").arg(&opt_object));
    let opt_exe = output.join("xor3.opt");
    run(Command::new(&tools.clang).arg(&opt_object).arg("-o").arg(&opt_exe));
    let opt_run_output = command_output(&mut Command::new(&opt_exe));
    assert!(opt_run_output.status.success(), "opt-transformed xor3 binary exited non-zero");
    assert_eq!(String::from_utf8_lossy(&opt_run_output.stdout).trim(), "0 1 0 1");
}

fn cargo_target_dir(root: &Path) -> PathBuf {
    env::var_os("CARGO_TARGET_DIR").map_or_else(|| root.join("target"), PathBuf::from)
}

fn dynamic_library_name(stem: &str) -> String {
    if cfg!(target_os = "macos") {
        format!("lib{stem}.dylib")
    } else if cfg!(target_os = "windows") {
        format!("{stem}.dll")
    } else {
        format!("lib{stem}.so")
    }
}

fn find_llvm_tool(name: &str, preferred_bin: Option<&Path>, fallbacks: &[&str]) -> Option<PathBuf> {
    preferred_bin
        .map(|directory| directory.join(name))
        .into_iter()
        .chain(fallbacks.iter().map(PathBuf::from))
        .find(|candidate| version_contains(candidate, &["--version"], "22."))
}

fn version_contains(program: &Path, arguments: &[&str], needle: &str) -> bool {
    command_text(program, arguments).is_some_and(|text| text.contains(needle))
}

fn command_text(program: &Path, arguments: &[&str]) -> Option<String> {
    let output = Command::new(program).args(arguments).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr)))
}

fn run(command: &mut Command) {
    let output = command_output(command);
    assert!(
        output.status.success(),
        "command failed ({:?}):\nstdout:\n{}\nstderr:\n{}",
        command,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn command_output(command: &mut Command) -> Output {
    command.output().unwrap_or_else(|error| panic!("run {command:?}: {error}"))
}
