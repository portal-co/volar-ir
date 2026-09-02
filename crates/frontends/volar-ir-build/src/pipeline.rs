// @reliability: experimental
// @ai: assisted
//! Builder-style IR transformation pipeline.

#[cfg(feature = "llvm")]
use std::path::Path;
use std::path::PathBuf;

use volar_ir::ir::{IRBlocks, IRTypes};
use volar_lir_saved::{RecordingTarget, SavedLirModule};

// ============================================================================
// Source
// ============================================================================

enum Source {
    LirFile(PathBuf),
    Lir(SavedLirModule),
    VolarIrFile(PathBuf),
    VolarIr(IRBlocks, IRTypes),
    #[cfg(feature = "vaffle")]
    VaffleFile(PathBuf),
    #[cfg(feature = "vaffle")]
    Vaffle(vaffle::Module),
    #[cfg(feature = "wasm")]
    Wasm(PathBuf),
    #[cfg(feature = "llvm")]
    Llvm {
        origin: LlvmLibOrigin,
        entries: Vec<String>,
    },
    #[cfg(feature = "llvm")]
    LlvmDirect {
        origin: LlvmLibOrigin,
        entry: String,
    },
}

/// How an LLVM pipeline source is materialized at execute time.
#[cfg(feature = "llvm")]
enum LlvmLibOrigin {
    /// Existing `.ll`, `.bc`, or LTO static library (`.a` / `.lib`).
    Path(PathBuf),
    /// `cc::Build` that compiles a static library of full-LTO objects.
    #[cfg(feature = "cc")]
    Cc {
        build: Box<cc::Build>,
        lib_name: String,
        out_dir: PathBuf,
    },
    /// User command that must write [`crate::CommandBuild::output`].
    Command(crate::CommandBuild),
}

// ============================================================================
// PipelinePass
// ============================================================================

/// A pass applied during [`Pipeline`] execution.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum PipelinePass {
    /// Inline every non-recursive intra-module VAFFLE call, including tail
    /// calls. Source must be Vaffle.
    #[cfg(feature = "vaffle")]
    InlineVaffleEverything,
    /// Lower a VAFFLE module to Volar IR. Source must be Vaffle.
    #[cfg(feature = "vaffle")]
    LowerToVolarIr,
    /// Constant-fold and DCE Volar IR until stable. Source must be VolarIr.
    FoldIr,
    /// Movfuscate Volar IR into a single self-looping block. Arbitrary
    /// control flow; result may loop (`is_movfuscated()`). Mutually
    /// alternative to [`PipelinePass::UnrollIrEverything`] for circuit shape.
    Movfuscate,
    /// Unroll Volar IR into a single combinational circuit (`is_circuit()`).
    /// Requires concrete control flow; fails closed otherwise.
    UnrollIrEverything,
    /// Lower Volar IR to a saved LIR module. Source must be VolarIr.
    LowerToLir,
}

// ============================================================================
// Pipeline
// ============================================================================

/// Composable IR-transform pipeline for `build.rs` scripts and frontends.
///
/// Terminates at VAFFLE, Volar IR, or LIR. Object emit and weaving belong
/// to `volar-build`.
pub struct Pipeline {
    source: Source,
    passes: Vec<PipelinePass>,
    inline_entries: Vec<String>,
    #[cfg(feature = "wasm")]
    import_config: volar_vaffle_target::WaffleImportConfig,
}

impl Pipeline {
    /// Start from a pre-recorded `.lir` file.
    pub fn from_saved_lir(path: impl Into<PathBuf>) -> Self {
        Self::new(Source::LirFile(path.into()))
    }

    /// Start from an in-memory saved LIR module.
    pub fn from_saved_lir_module(saved: SavedLirModule) -> Self {
        Self::new(Source::Lir(saved))
    }

    /// Start from a rkyv-serialized `(IRBlocks, IRTypes)` file.
    pub fn from_volar_ir(path: impl Into<PathBuf>) -> Self {
        Self::new(Source::VolarIrFile(path.into()))
    }

    /// Start from in-memory Volar IR.
    pub fn from_volar_ir_blocks(blocks: IRBlocks, types: IRTypes) -> Self {
        Self::new(Source::VolarIr(blocks, types))
    }

    /// Start from a `.vaffle` file.
    #[cfg(feature = "vaffle")]
    pub fn from_vaffle(path: impl Into<PathBuf>) -> Self {
        Self::new(Source::VaffleFile(path.into()))
    }

    /// Start from an in-memory VAFFLE module.
    #[cfg(feature = "vaffle")]
    pub fn from_vaffle_module(module: vaffle::Module) -> Self {
        Self::new(Source::Vaffle(module))
    }

    /// Start from a `.wasm` file; WAFFLE parsing happens at execution time.
    #[cfg(feature = "wasm")]
    pub fn from_wasm(path: impl Into<PathBuf>) -> Self {
        Self::new(Source::Wasm(path.into()))
    }

    /// `from_wasm` plus [`Pipeline::inline_vaffle_everything`].
    #[cfg(feature = "wasm")]
    pub fn from_wasm_inlined(path: impl Into<PathBuf>) -> Self {
        Self::from_wasm(path).inline_vaffle_everything()
    }

    /// Start from LLVM bitcode (`.bc`), assembly (`.ll`), or an LTO static
    /// library (`.a` / `.lib`) via the call-preserving structural importer.
    #[cfg(feature = "llvm")]
    pub fn from_llvm(path: impl Into<PathBuf>, entries: &[&str]) -> Self {
        Self::from_llvm_origin(LlvmLibOrigin::Path(path.into()), entries)
    }

    /// Structural LLVM import plus [`Pipeline::inline_vaffle_everything`].
    #[cfg(feature = "llvm")]
    pub fn from_llvm_inlined(path: impl Into<PathBuf>, entries: &[&str]) -> Self {
        Self::from_llvm(path, entries).inline_vaffle_everything()
    }

    /// Start from LLVM via the execution-mode (direct) importer. Already
    /// produces `is_circuit()` Volar IR when it succeeds. Accepts `.ll`,
    /// `.bc`, or an LTO static library.
    #[cfg(feature = "llvm")]
    pub fn from_llvm_direct(path: impl Into<PathBuf>, entry: &str) -> Self {
        Self::new(Source::LlvmDirect {
            origin: LlvmLibOrigin::Path(path.into()),
            entry: entry.to_string(),
        })
    }

    /// Compile `build` to a static library of clang full-LTO objects, then
    /// import structurally. Requires a clang-like compiler; GCC LTO is
    /// rejected. `build` should already list source files / include paths.
    #[cfg(feature = "cc")]
    pub fn from_cc(build: cc::Build, lib_name: &str, entries: &[&str]) -> Self {
        Self::from_llvm_origin(cc_origin(build, lib_name), entries)
    }

    /// [`Pipeline::from_cc`] plus [`Pipeline::inline_vaffle_everything`].
    #[cfg(feature = "cc")]
    pub fn from_cc_inlined(build: cc::Build, lib_name: &str, entries: &[&str]) -> Self {
        Self::from_cc(build, lib_name, entries).inline_vaffle_everything()
    }

    /// [`Pipeline::from_cc`] then the execution-mode importer.
    #[cfg(feature = "cc")]
    pub fn from_cc_direct(build: cc::Build, lib_name: &str, entry: &str) -> Self {
        Self::new(Source::LlvmDirect {
            origin: cc_origin(build, lib_name),
            entry: entry.to_string(),
        })
    }

    /// Run `cmd` (no shell) to produce a static library, then import
    /// structurally.
    #[cfg(feature = "llvm")]
    pub fn from_command(cmd: crate::CommandBuild, entries: &[&str]) -> Self {
        Self::from_llvm_origin(LlvmLibOrigin::Command(cmd), entries)
    }

    /// [`Pipeline::from_command`] plus [`Pipeline::inline_vaffle_everything`].
    #[cfg(feature = "llvm")]
    pub fn from_command_inlined(cmd: crate::CommandBuild, entries: &[&str]) -> Self {
        Self::from_command(cmd, entries).inline_vaffle_everything()
    }

    /// [`Pipeline::from_command`] then the execution-mode importer.
    #[cfg(feature = "llvm")]
    pub fn from_command_direct(cmd: crate::CommandBuild, entry: &str) -> Self {
        Self::new(Source::LlvmDirect {
            origin: LlvmLibOrigin::Command(cmd),
            entry: entry.to_string(),
        })
    }

    #[cfg(feature = "llvm")]
    fn from_llvm_origin(origin: LlvmLibOrigin, entries: &[&str]) -> Self {
        let mut p = Self::new(Source::Llvm {
            origin,
            entries: entries.iter().map(|s| (*s).to_string()).collect(),
        });
        p.inline_entries = entries.iter().map(|s| (*s).to_string()).collect();
        p
    }

    fn new(source: Source) -> Self {
        Pipeline {
            source,
            passes: vec![],
            inline_entries: vec![],
            #[cfg(feature = "wasm")]
            import_config: volar_vaffle_target::WaffleImportConfig::new(),
        }
    }

    /// Names used as roots for [`PipelinePass::InlineVaffleEverything`].
    /// Defaults to every export on the VAFFLE module.
    pub fn with_inline_entries(mut self, entries: &[&str]) -> Self {
        self.inline_entries = entries.iter().map(|s| (*s).to_string()).collect();
        self
    }

    /// Configure oracle/action import mappings for WASM pipelines.
    #[cfg(feature = "wasm")]
    pub fn with_import_config(mut self, config: volar_vaffle_target::WaffleImportConfig) -> Self {
        self.import_config = config;
        self
    }
}

impl Pipeline {
    /// Inline every non-recursive intra-module VAFFLE call.
    #[cfg(feature = "vaffle")]
    pub fn inline_vaffle_everything(mut self) -> Self {
        self.passes.push(PipelinePass::InlineVaffleEverything);
        self
    }

    /// Lower VAFFLE → Volar IR.
    #[cfg(feature = "vaffle")]
    pub fn lower_to_volar_ir(mut self) -> Self {
        self.passes.push(PipelinePass::LowerToVolarIr);
        self
    }

    /// Constant-fold Volar IR until stable.
    pub fn fold_ir(mut self) -> Self {
        self.passes.push(PipelinePass::FoldIr);
        self
    }

    /// Movfuscate Volar IR into a single self-looping block.
    pub fn movfuscate(mut self) -> Self {
        self.passes.push(PipelinePass::Movfuscate);
        self
    }

    /// Unroll Volar IR into a combinational circuit (concrete CF required).
    pub fn unroll_ir(mut self) -> Self {
        self.passes.push(PipelinePass::UnrollIrEverything);
        self
    }

    /// Lower Volar IR → saved LIR.
    pub fn lower_to_lir(mut self) -> Self {
        self.passes.push(PipelinePass::LowerToLir);
        self
    }
}

impl Pipeline {
    /// Execute all passes and return Volar IR.
    pub fn to_volar_ir(self) -> Result<(IRBlocks, IRTypes), Box<dyn std::error::Error>> {
        match self.execute()? {
            Executed::VolarIr(blocks, types) => Ok((blocks, types)),
            Executed::Lir(_) => Err(
                "to_volar_ir requires VolarIr stage; got Lir — add .lower_to_volar_ir() or start from a non-Lir source".into(),
            ),
            #[cfg(feature = "vaffle")]
            Executed::Vaffle(_) => Err(
                "to_volar_ir requires VolarIr stage; got Vaffle — add .lower_to_volar_ir()".into(),
            ),
        }
    }

    /// Execute all passes and return a saved LIR module.
    pub fn to_lir(self) -> Result<SavedLirModule, Box<dyn std::error::Error>> {
        match self.execute()? {
            Executed::Lir(saved) => Ok(saved),
            Executed::VolarIr(blocks, types) => Ok(lower_volar_ir_to_lir(&blocks, &types)),
            #[cfg(feature = "vaffle")]
            Executed::Vaffle(_) => Err(
                "to_lir requires VolarIr or Lir stage; got Vaffle — add .lower_to_volar_ir()"
                    .into(),
            ),
        }
    }

    /// Execute all passes and return the VAFFLE module, if the pipeline
    /// stopped at that stage.
    #[cfg(feature = "vaffle")]
    pub fn to_vaffle(self) -> Result<vaffle::Module, Box<dyn std::error::Error>> {
        match self.execute()? {
            Executed::Vaffle(m) => Ok(m),
            _ => Err("to_vaffle requires the pipeline to terminate at the Vaffle stage".into()),
        }
    }

    fn execute(self) -> Result<Executed, Box<dyn std::error::Error>> {
        let mut stage = load_source(
            self.source,
            #[cfg(feature = "wasm")]
            self.import_config,
        )?;
        for pass in self.passes {
            stage = apply_pass(pass, stage, &self.inline_entries)?;
        }
        match stage {
            RuntimeStage::Lir(saved) => Ok(Executed::Lir(saved)),
            RuntimeStage::VolarIr(blocks, types) => Ok(Executed::VolarIr(blocks, types)),
            #[cfg(feature = "vaffle")]
            RuntimeStage::Vaffle(m) => Ok(Executed::Vaffle(m)),
        }
    }
}

enum Executed {
    Lir(SavedLirModule),
    VolarIr(IRBlocks, IRTypes),
    #[cfg(feature = "vaffle")]
    Vaffle(vaffle::Module),
}

enum RuntimeStage {
    Lir(SavedLirModule),
    VolarIr(IRBlocks, IRTypes),
    #[cfg(feature = "vaffle")]
    Vaffle(vaffle::Module),
}

fn load_source(
    source: Source,
    #[cfg(feature = "wasm")] import_config: volar_vaffle_target::WaffleImportConfig,
) -> Result<RuntimeStage, Box<dyn std::error::Error>> {
    match source {
        Source::LirFile(path) => {
            let bytes = std::fs::read(&path)?;
            let saved = rkyv::from_bytes::<SavedLirModule, rkyv::rancor::Error>(&bytes)?;
            Ok(RuntimeStage::Lir(saved))
        }
        Source::Lir(saved) => Ok(RuntimeStage::Lir(saved)),
        Source::VolarIrFile(path) => {
            let bytes = std::fs::read(&path)?;
            let (blocks, types) =
                rkyv::from_bytes::<(IRBlocks, IRTypes), rkyv::rancor::Error>(&bytes)
                    .map_err(|e| format!("failed to deserialize Volar IR file: {e}"))?;
            Ok(RuntimeStage::VolarIr(blocks, types))
        }
        Source::VolarIr(blocks, types) => Ok(RuntimeStage::VolarIr(blocks, types)),
        #[cfg(feature = "vaffle")]
        Source::VaffleFile(path) => {
            let bytes = std::fs::read(&path)?;
            let module = rkyv::from_bytes::<vaffle::Module, rkyv::rancor::Error>(&bytes)
                .map_err(|e| format!("failed to deserialize .vaffle file: {e}"))?;
            Ok(RuntimeStage::Vaffle(module))
        }
        #[cfg(feature = "vaffle")]
        Source::Vaffle(module) => Ok(RuntimeStage::Vaffle(module)),
        #[cfg(feature = "wasm")]
        Source::Wasm(path) => {
            let bytes = std::fs::read(&path)?;
            let waffle_module = portal_pc_waffle_frontend::from_wasm_bytes(
                &bytes,
                &portal_pc_waffle_frontend::FrontendOptions::default(),
            )
            .map_err(|e| format!("WAFFLE parse failed: {e}"))?;
            let mut target = volar_vaffle_target::VaffleTarget::new();
            volar_vaffle_target::lower_waffle_module(&waffle_module, &mut target, &import_config);
            Ok(RuntimeStage::Vaffle(target.module))
        }
        #[cfg(feature = "llvm")]
        Source::Llvm { origin, entries } => {
            let path = materialize_llvm_lib(origin)?;
            let context = inkwell::context::Context::create();
            let llvm_module = load_llvm_module(&context, &path)?;
            let entry_refs: Vec<&str> = entries.iter().map(|s| s.as_str()).collect();
            let module = volar_llvm_vaffle_import::import_module(&llvm_module, &entry_refs)?;
            Ok(RuntimeStage::Vaffle(module))
        }
        #[cfg(feature = "llvm")]
        Source::LlvmDirect { origin, entry } => {
            let path = materialize_llvm_lib(origin)?;
            let context = inkwell::context::Context::create();
            let llvm_module = load_llvm_module(&context, &path)?;
            let (blocks, types) = volar_llvm_ir_import::import_module(
                &llvm_module,
                &entry,
                volar_llvm_ir_import::LoweringLimits::default(),
            )?;
            Ok(RuntimeStage::VolarIr(blocks, types))
        }
    }
}

#[cfg(feature = "llvm")]
fn materialize_llvm_lib(origin: LlvmLibOrigin) -> Result<PathBuf, Box<dyn std::error::Error>> {
    match origin {
        LlvmLibOrigin::Path(path) => Ok(path),
        #[cfg(feature = "cc")]
        LlvmLibOrigin::Cc {
            mut build,
            lib_name,
            out_dir,
        } => compile_cc_lto(&mut build, &lib_name, &out_dir),
        LlvmLibOrigin::Command(cmd) => cmd.run(),
    }
}

#[cfg(feature = "cc")]
fn cc_origin(mut build: cc::Build, lib_name: &str) -> LlvmLibOrigin {
    let out_dir = std::env::var_os("OUT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let dir = std::env::temp_dir().join(format!(
                "volar-ir-build-lto-{}-{}",
                std::process::id(),
                lib_name
            ));
            let _ = std::fs::create_dir_all(&dir);
            dir
        });
    build.out_dir(&out_dir);
    LlvmLibOrigin::Cc {
        build: Box::new(build),
        lib_name: lib_name.to_string(),
        out_dir,
    }
}

#[cfg(feature = "cc")]
fn compile_cc_lto(
    build: &mut cc::Build,
    lib_name: &str,
    out_dir: &Path,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    build.out_dir(out_dir);
    build.cargo_metadata(false);
    build.static_flag(true);
    build.flag("-flto=full");
    let compiler = build
        .try_get_compiler()
        .map_err(|e| format!("cc LTO pre-build: failed to detect C compiler: {e}"))?;
    if !compiler.is_like_clang() {
        return Err(format!(
            "cc LTO pre-build requires a clang-like compiler (got {}); GCC LTO is not LLVM bitcode",
            compiler.path().display()
        )
        .into());
    }
    build
        .try_compile(lib_name)
        .map_err(|e| format!("cc LTO pre-build compile failed: {e}"))?;
    let archive = out_dir.join(format!("lib{lib_name}.a"));
    if !archive.exists() {
        return Err(format!("cc LTO pre-build did not write {}", archive.display()).into());
    }
    Ok(archive)
}

#[cfg(feature = "llvm")]
fn load_llvm_module<'ctx>(
    context: &'ctx inkwell::context::Context,
    path: &Path,
) -> Result<inkwell::module::Module<'ctx>, Box<dyn std::error::Error>> {
    use inkwell::memory_buffer::MemoryBuffer;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let bytes = std::fs::read(path)?;
    // Unix `ar` magic (`!<arch>\n`) so a misnamed archive is still loaded
    // as members rather than as a single bitcode blob.
    if ext == "a" || ext == "lib" || bytes.starts_with(b"!<arch>\n") {
        return crate::lto_archive::load_lto_archive(context, path);
    }
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("module");
    if ext == "ll" {
        Ok(context
            .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(&bytes, name))?)
    } else {
        Ok(inkwell::module::Module::parse_bitcode_from_buffer(
            &MemoryBuffer::create_from_memory_range_copy(&bytes, name),
            context,
        )?)
    }
}

fn apply_pass(
    pass: PipelinePass,
    stage: RuntimeStage,
    inline_entries: &[String],
) -> Result<RuntimeStage, Box<dyn std::error::Error>> {
    match pass {
        #[cfg(feature = "vaffle")]
        PipelinePass::InlineVaffleEverything => match stage {
            RuntimeStage::Vaffle(mut module) => {
                let ids = resolve_inline_entries(&module, inline_entries)?;
                volar_ir_opt::inline_vaffle::inline_vaffle_everything(&mut module, &ids)?;
                Ok(RuntimeStage::Vaffle(module))
            }
            _ => Err("InlineVaffleEverything pass requires Vaffle stage".into()),
        },
        #[cfg(feature = "vaffle")]
        PipelinePass::LowerToVolarIr => match stage {
            RuntimeStage::Vaffle(module) => {
                let (blocks, types) = volar_vaffle_target::lower_vaffle_to_ir(&module);
                Ok(RuntimeStage::VolarIr(blocks, types))
            }
            _ => Err("LowerToVolarIr pass requires Vaffle stage".into()),
        },
        PipelinePass::FoldIr => match stage {
            RuntimeStage::VolarIr(mut blocks, types) => {
                loop {
                    let folded = volar_ir_opt::ir::fold_ir_blocks(&mut blocks, &types);
                    let deadcode = volar_ir_opt::ir::dce_ir_blocks(&mut blocks, &types);
                    if !folded && !deadcode {
                        break;
                    }
                }
                Ok(RuntimeStage::VolarIr(blocks, types))
            }
            _ => Err("FoldIr pass requires VolarIr stage".into()),
        },
        PipelinePass::Movfuscate => match stage {
            RuntimeStage::VolarIr(blocks, mut types) => {
                let blocks = volar_ir_passes::movfuscate_ir(&blocks, &mut types);
                Ok(RuntimeStage::VolarIr(blocks, types))
            }
            _ => Err("Movfuscate pass requires VolarIr stage".into()),
        },
        PipelinePass::UnrollIrEverything => match stage {
            RuntimeStage::VolarIr(blocks, types) => {
                let blocks = volar_ir_passes::unroll_ir_everything(&blocks, &types)?;
                Ok(RuntimeStage::VolarIr(blocks, types))
            }
            _ => Err("UnrollIrEverything pass requires VolarIr stage".into()),
        },
        PipelinePass::LowerToLir => match stage {
            RuntimeStage::VolarIr(blocks, types) => {
                Ok(RuntimeStage::Lir(lower_volar_ir_to_lir(&blocks, &types)))
            }
            _ => Err("LowerToLir pass requires VolarIr stage".into()),
        },
    }
}

#[cfg(feature = "vaffle")]
fn resolve_inline_entries(
    module: &vaffle::Module,
    inline_entries: &[String],
) -> Result<Vec<vaffle::FuncId>, Box<dyn std::error::Error>> {
    if inline_entries.is_empty() {
        if module.exports.is_empty() {
            let ids: Vec<vaffle::FuncId> = module
                .funcs
                .iter()
                .enumerate()
                .filter_map(|(i, d)| match d {
                    vaffle::FuncDecl::Body(_) => Some(vaffle::FuncId(i)),
                    _ => None,
                })
                .collect();
            if ids.is_empty() {
                return Err("InlineVaffleEverything: no function bodies to use as entries".into());
            }
            return Ok(ids);
        }
        return Ok(module.exports.values().copied().collect());
    }
    let mut ids = Vec::with_capacity(inline_entries.len());
    for name in inline_entries {
        let id = module
            .exports
            .get(name)
            .copied()
            .ok_or_else(|| format!("inline entry `{name}` is not exported"))?;
        ids.push(id);
    }
    Ok(ids)
}

pub(crate) fn lower_volar_ir_to_lir(blocks: &IRBlocks, types: &IRTypes) -> SavedLirModule {
    let mut rec = RecordingTarget::new();
    volar_ir_passes::lower_lir::lower_ir(blocks, types, "volar_module", &mut rec);
    rec.finish()
}

/// Serialize a VAFFLE [`Module`](vaffle::Module) to bytes for use as a `.vaffle`
/// file with [`Pipeline::from_vaffle`].
#[cfg(feature = "vaffle")]
pub fn serialize_vaffle_module(
    module: &vaffle::Module,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    Ok(rkyv::to_bytes::<rkyv::rancor::Error>(module)?.into_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use volar_ir::ir::{
        IRBlock, IRBlockId, IRBlockTargetId, IRBranchTarget, IRStmt, IRTerminator, IRType,
        IRTypeId, IRVarId,
    };
    use volar_ir_common::{Constant, Node, Type};

    fn bit_types() -> IRTypes {
        IRTypes(vec![IRType::Primitive(Type::Bit)])
    }

    #[test]
    fn unroll_const_branch_via_pipeline() {
        let types = bit_types();
        let blocks: IRBlocks<()> = IRBlocks::new(vec![
            IRBlock {
                params: vec![],
                stmts: vec![Node::new(
                    IRStmt::Const(Constant { hi: 0, lo: 1 }, IRTypeId(0)),
                    (),
                    None,
                )],
                terminator: IRTerminator::JumpCond {
                    condition: IRVarId(0),
                    then_target: IRBranchTarget::new(IRBlockTargetId::Block(IRBlockId(1)), vec![]),
                    else_target: IRBranchTarget::new(IRBlockTargetId::Block(IRBlockId(2)), vec![]),
                },
            },
            IRBlock {
                params: vec![],
                stmts: vec![Node::new(
                    IRStmt::Const(Constant { hi: 0, lo: 1 }, IRTypeId(0)),
                    (),
                    None,
                )],
                terminator: IRTerminator::Jmp {
                    target: IRBranchTarget::new(IRBlockTargetId::Return, vec![IRVarId(0)]),
                },
            },
            IRBlock {
                params: vec![],
                stmts: vec![Node::new(
                    IRStmt::Const(Constant { hi: 0, lo: 0 }, IRTypeId(0)),
                    (),
                    None,
                )],
                terminator: IRTerminator::Jmp {
                    target: IRBranchTarget::new(IRBlockTargetId::Return, vec![IRVarId(0)]),
                },
            },
        ]);
        let (out, _) = Pipeline::from_volar_ir_blocks(blocks, types)
            .unroll_ir()
            .to_volar_ir()
            .expect("unroll via pipeline");
        assert!(out.is_circuit());
    }

    #[test]
    fn unroll_rejects_symbolic_branch_via_pipeline() {
        let types = bit_types();
        let blocks: IRBlocks<()> = IRBlocks::new(vec![IRBlock {
            params: vec![IRTypeId(0)],
            stmts: vec![],
            terminator: IRTerminator::JumpCond {
                condition: IRVarId(0),
                then_target: IRBranchTarget::new(IRBlockTargetId::Return, vec![IRVarId(0)]),
                else_target: IRBranchTarget::new(IRBlockTargetId::Return, vec![IRVarId(0)]),
            },
        }]);
        let err = Pipeline::from_volar_ir_blocks(blocks, types)
            .unroll_ir()
            .to_volar_ir()
            .expect_err("symbolic branch");
        assert!(err.to_string().contains("symbolic branch"));
    }
}
