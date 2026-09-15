//! Generates the small C++ shim every LLVM New-PM pass-plugin cdylib needs:
//! `llvmGetPassPluginInfo`, one `PassInfoMixin` class per Rust-side
//! `run(Module) -> Result` entry point, and the pipeline-name/EP-callback
//! registration wiring them up.
//!
//! Extracted from `cirrus-llvm-pass`'s hand-written `cxx/pass_shim.cpp` —
//! that file's *shape* (not its cirrus-specific pass names/symbols) is
//! exactly the boilerplate every plugin repeats, so this generates the
//! equivalent source from a small config instead of hand-copying it.

/// One registered pass: a pipeline name plus the Rust `extern "C" fn`
/// symbol it calls (`fn(LLVMModuleRef, *mut *mut c_char) -> i32`, per
/// [`volar_llvm_pass_support`](https://docs.rs/volar-llvm-pass-support)'s
/// entry-point convention: negative means `*mut *mut c_char` holds an
/// allocated diagnostic, `0`/`1` mean unchanged/changed).
pub struct PluginPass {
    /// LLVM `-passes=` pipeline name, e.g. `"cirrus-lower"`.
    pub pipeline_name: String,
    /// The Rust entry-point symbol this pass calls.
    pub run_symbol: String,
    /// Prefix used in `report_fatal_error` messages, e.g. `"cirrus-lower"`.
    pub error_prefix: String,
    /// If present, also registers into `OptimizerEarlyEP` and
    /// `FullLinkTimeOptimizationEarlyEP` so this pass runs automatically
    /// during ordinary/LTO compiles, not only when explicitly named via
    /// `-passes=`.
    pub auto_register: Option<AutoRegister>,
}

/// Automatic-registration behavior for one [`PluginPass`].
pub struct AutoRegister {
    /// Skip `OptimizerEarlyEP` specifically during `FullLTOPreLink` (i.e.
    /// only run this pass on the merged whole-program module, not on each
    /// translation unit's individual pre-link module) — needed by passes
    /// that resolve state across module boundaries. `false` runs the pass
    /// in every phase, pre-link included.
    pub skip_full_lto_prelink: bool,
}

/// Full shim configuration for one plugin cdylib.
pub struct ShimConfig {
    /// Plugin name reported to LLVM (shown in `-passes=help` etc).
    pub plugin_name: String,
    /// Plugin version string reported to LLVM.
    pub plugin_version: String,
    /// Name of an empty `extern "C"` function this shim defines and the
    /// Rust side calls once per entry point, purely to force the linker to
    /// retain the archive member containing `llvmGetPassPluginInfo` (Cargo
    /// links the `cc`-built shim as a static archive, which ordinary
    /// archive extraction would otherwise discard since nothing in Rust
    /// directly calls into it).
    pub link_anchor_symbol: String,
    /// Rust symbol matching `extern "C" fn(*mut c_char)` that frees a
    /// diagnostic allocated by a failing `run_symbol` call.
    pub free_error_symbol: String,
    /// Every pass this plugin registers. Must be non-empty.
    pub passes: Vec<PluginPass>,
}

/// Render `config` into a complete, ready-to-compile `.cpp` source string.
pub fn generate_shim_source(config: &ShimConfig) -> String {
    generate_shim_source_with_scope(config, false)
}

/// Render a shim whose automatic passes run only after LLVM has built the
/// merged Full-LTO module.  Explicit `-passes=` use is retained.  This is for
/// transforms such as provider collection where running independently in
/// every ThinLTO partition would silently create incomplete catalogs.
///
/// Unlike adding another field to [`PluginPass`], this keeps the original
/// configuration ABI source-compatible for existing consumers.
pub fn generate_full_lto_shim_source(config: &ShimConfig) -> String {
    generate_shim_source_with_scope(config, true)
}

fn generate_shim_source_with_scope(config: &ShimConfig, full_lto_only: bool) -> String {
    assert!(
        !config.passes.is_empty(),
        "a plugin shim needs at least one registered pass"
    );

    let mut out = String::new();
    out.push_str(
        "#include \"llvm/IR/Module.h\"\n\
         #include \"llvm/IR/PassManager.h\"\n\
         #include \"llvm/Pass.h\"\n\
         #include \"llvm/Passes/PassBuilder.h\"\n\
         #include \"llvm/Plugins/PassPlugin.h\"\n\
         #include \"llvm/Support/ErrorHandling.h\"\n\
         #include \"llvm-c/Core.h\"\n\n",
    );

    for pass in &config.passes {
        out.push_str(&format!(
            "extern \"C\" int {run}(LLVMModuleRef Module, char **Error);\n",
            run = pass.run_symbol
        ));
    }
    out.push_str(&format!(
        "extern \"C\" void {free_error}(char *Error);\n\n\
         // Referenced from Rust so the archive member that contains\n\
         // llvmGetPassPluginInfo is retained when Cargo links this cdylib.\n\
         extern \"C\" void {anchor}() {{}}\n\n\
         namespace {{\n\n",
        free_error = config.free_error_symbol,
        anchor = config.link_anchor_symbol,
    ));

    for pass in &config.passes {
        let class_name = class_name_for(&pass.pipeline_name);
        out.push_str(&format!(
            "class {class_name} : public llvm::PassInfoMixin<{class_name}> {{\n\
             public:\n\
             \x20 llvm::PreservedAnalyses run(llvm::Module &M, llvm::ModuleAnalysisManager &) {{\n\
             \x20   char *Error = nullptr;\n\
             \x20   int Result = {run}(reinterpret_cast<LLVMModuleRef>(&M), &Error);\n\
             \x20   if (Result < 0) {{\n\
             \x20     std::string Message = Error ? Error : \"unknown {error_prefix} pass failure\";\n\
             \x20     if (Error)\n\
             \x20       {free_error}(Error);\n\
             \x20     Message = \"{error_prefix}: \" + Message;\n\
             \x20     llvm::report_fatal_error(llvm::StringRef(Message));\n\
             \x20   }}\n\
             \x20   return Result == 0 ? llvm::PreservedAnalyses::all()\n\
             \x20                      : llvm::PreservedAnalyses::none();\n\
             \x20 }}\n\
             }};\n\n",
            class_name = class_name,
            run = pass.run_symbol,
            error_prefix = escape_cpp_string(&pass.error_prefix),
            free_error = config.free_error_symbol,
        ));
    }

    out.push_str("void registerCallbacks(llvm::PassBuilder &PB) {\n");
    for pass in &config.passes {
        let class_name = class_name_for(&pass.pipeline_name);
        out.push_str(&format!(
            "\x20 PB.registerPipelineParsingCallback(\n\
             \x20     [](llvm::StringRef Name, llvm::ModulePassManager &MPM,\n\
             \x20        llvm::ArrayRef<llvm::PassBuilder::PipelineElement>) {{\n\
             \x20       if (Name != \"{pipeline_name}\")\n\
             \x20         return false;\n\
             \x20       MPM.addPass({class_name}());\n\
             \x20       return true;\n\
             \x20     }});\n",
            pipeline_name = escape_cpp_string(&pass.pipeline_name),
            class_name = class_name,
        ));
    }
    for pass in &config.passes {
        let Some(auto) = &pass.auto_register else {
            continue;
        };
        let class_name = class_name_for(&pass.pipeline_name);
        if !full_lto_only && auto.skip_full_lto_prelink {
            out.push_str(&format!(
                "\x20 PB.registerOptimizerEarlyEPCallback(\n\
                 \x20     [](llvm::ModulePassManager &MPM, llvm::OptimizationLevel,\n\
                 \x20        llvm::ThinOrFullLTOPhase Phase) {{\n\
                 \x20       if (Phase != llvm::ThinOrFullLTOPhase::FullLTOPreLink)\n\
                 \x20         MPM.addPass({class_name}());\n\
                 \x20     }});\n",
                class_name = class_name,
            ));
        } else if !full_lto_only {
            out.push_str(&format!(
                "\x20 PB.registerOptimizerEarlyEPCallback(\n\
                 \x20     [](llvm::ModulePassManager &MPM, llvm::OptimizationLevel,\n\
                 \x20        llvm::ThinOrFullLTOPhase) {{\n\
                 \x20       MPM.addPass({class_name}());\n\
                 \x20     }});\n",
                class_name = class_name,
            ));
        }
        out.push_str(&format!(
            "\x20 PB.registerFullLinkTimeOptimizationEarlyEPCallback(\n\
             \x20     [](llvm::ModulePassManager &MPM, llvm::OptimizationLevel) {{\n\
             \x20       MPM.addPass({class_name}());\n\
             \x20     }});\n",
            class_name = class_name,
        ));
    }
    out.push_str("}\n\n} // namespace\n\n");

    out.push_str(&format!(
        "extern \"C\" LLVM_ATTRIBUTE_WEAK ::llvm::PassPluginLibraryInfo\n\
         llvmGetPassPluginInfo() {{\n\
         \x20 return {{LLVM_PLUGIN_API_VERSION, \"{plugin_name}\", \"{plugin_version}\", registerCallbacks}};\n\
         }}\n",
        plugin_name = escape_cpp_string(&config.plugin_name),
        plugin_version = escape_cpp_string(&config.plugin_version),
    ));

    out
}

/// Derive a valid, collision-resistant C++ class name from a pipeline name
/// (e.g. `"cirrus-lower"` → `"Plugin_cirrus_lower"`).
fn class_name_for(pipeline_name: &str) -> String {
    let mut name = String::from("Plugin_");
    for ch in pipeline_name.chars() {
        name.push(if ch.is_ascii_alphanumeric() { ch } else { '_' });
    }
    name
}

fn escape_cpp_string(s: &str) -> String {
    s.chars()
        .flat_map(|c| match c {
            '"' => vec!['\\', '"'],
            '\\' => vec!['\\', '\\'],
            other => vec![other],
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cirrus_equivalent_config() -> ShimConfig {
        ShimConfig {
            plugin_name: "cirrus-llvm-pass".into(),
            plugin_version: "0.1".into(),
            link_anchor_symbol: "cirrus_llvm_pass_link_anchor".into(),
            free_error_symbol: "cirrus_llvm_pass_free_error".into(),
            passes: vec![
                PluginPass {
                    pipeline_name: "cirrus-lower".into(),
                    run_symbol: "cirrus_llvm_pass_run".into(),
                    error_prefix: "cirrus-lower".into(),
                    auto_register: Some(AutoRegister {
                        skip_full_lto_prelink: true,
                    }),
                },
                PluginPass {
                    pipeline_name: "cirrus-deloopify".into(),
                    run_symbol: "cirrus_llvm_pass_deloopify_run".into(),
                    error_prefix: "cirrus-deloopify".into(),
                    auto_register: None,
                },
            ],
        }
    }

    #[test]
    fn generates_both_pass_classes_and_symbols() {
        let source = generate_shim_source(&cirrus_equivalent_config());
        assert!(source.contains(
            "extern \"C\" int cirrus_llvm_pass_run(LLVMModuleRef Module, char **Error);"
        ));
        assert!(source.contains(
            "extern \"C\" int cirrus_llvm_pass_deloopify_run(LLVMModuleRef Module, char **Error);"
        ));
        assert!(source.contains("class Plugin_cirrus_lower"));
        assert!(source.contains("class Plugin_cirrus_deloopify"));
        assert!(source.contains("if (Name != \"cirrus-lower\")"));
        assert!(source.contains("if (Name != \"cirrus-deloopify\")"));
        assert!(source.contains("\"cirrus-lower: \" + Message"));
        assert!(source.contains("llvmGetPassPluginInfo"));
        assert!(source.contains("\"cirrus-llvm-pass\", \"0.1\", registerCallbacks"));
    }

    #[test]
    fn auto_register_only_applies_to_configured_passes() {
        let source = generate_shim_source(&cirrus_equivalent_config());
        // cirrus-lower opts in and skips FullLTOPreLink.
        assert!(source.contains("if (Phase != llvm::ThinOrFullLTOPhase::FullLTOPreLink)"));
        assert!(source.contains("MPM.addPass(Plugin_cirrus_lower());"));
        // cirrus-deloopify never opts in: its class is only ever
        // constructed once, in the explicit `-passes=cirrus-deloopify`
        // pipeline-parsing registration, never in an EarlyEP callback.
        assert_eq!(
            source
                .matches("MPM.addPass(Plugin_cirrus_deloopify())")
                .count(),
            1
        );
        assert_eq!(
            source.matches("MPM.addPass(Plugin_cirrus_lower())").count(),
            3
        );
    }

    #[test]
    fn full_lto_only_generator_skips_optimizer_registration() {
        let source = generate_full_lto_shim_source(&cirrus_equivalent_config());
        assert!(!source.contains("registerOptimizerEarlyEPCallback"));
        assert_eq!(
            source
                .matches("registerFullLinkTimeOptimizationEarlyEPCallback")
                .count(),
            1
        );
    }

    #[test]
    fn class_names_are_sanitized_and_stable() {
        assert_eq!(class_name_for("cirrus-lower"), "Plugin_cirrus_lower");
        assert_eq!(
            class_name_for("volar.thread+import"),
            "Plugin_volar_thread_import"
        );
    }

    #[test]
    #[should_panic(expected = "at least one registered pass")]
    fn empty_pass_list_panics() {
        generate_shim_source(&ShimConfig {
            plugin_name: "empty".into(),
            plugin_version: "0.1".into(),
            link_anchor_symbol: "anchor".into(),
            free_error_symbol: "free_error".into(),
            passes: vec![],
        });
    }
}
