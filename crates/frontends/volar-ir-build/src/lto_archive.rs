// @reliability: experimental
// @ai: assisted
//! Load a static library of LLVM LTO bitcode members into one module.
//!
//! Members may be raw bitcode (`.bc` / clang `-flto=full` "objects") or
//! native objects that embed bitcode in ELF `.llvmbc` / Mach-O
//! `__LLVM,__bitcode`. Native-only members and ThinLTO-only summaries
//! fail closed — they are not dropped.

use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use inkwell::context::Context;
use inkwell::memory_buffer::MemoryBuffer;
use inkwell::module::Module as LlvmModule;
use object::{Object, ObjectSection};

/// LLVM bitcode magic (`BC\xc0\xde`).
const BC_MAGIC: &[u8] = b"BC\xc0\xde";
/// Bitcode wrapper magic (`0xDE 0xC0 0x17 0x0B`, little-endian 0x0B17C0DE).
const BC_WRAPPER: &[u8] = &[0xDE, 0xC0, 0x17, 0x0B];

/// A user-specified command that must write a static library of LTO
/// bitcode objects to [`CommandBuild::output`].
#[derive(Clone, Debug)]
pub struct CommandBuild {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub output: PathBuf,
    pub cwd: Option<PathBuf>,
}

impl CommandBuild {
    /// Run the command (no shell). Non-zero exit or a missing output file
    /// is an error.
    pub fn run(&self) -> Result<PathBuf, Box<dyn std::error::Error>> {
        let mut cmd = Command::new(&self.program);
        cmd.args(&self.args);
        if let Some(cwd) = &self.cwd {
            cmd.current_dir(cwd);
        }
        let status = cmd.status().map_err(|e| {
            format!(
                "failed to spawn LTO pre-build command {}: {e}",
                self.program.display()
            )
        })?;
        if !status.success() {
            return Err(format!(
                "LTO pre-build command {} exited with {status}",
                self.program.display()
            )
            .into());
        }
        if !self.output.exists() {
            return Err(format!(
                "LTO pre-build command succeeded but did not write {}",
                self.output.display()
            )
            .into());
        }
        Ok(self.output.clone())
    }
}

/// True when `bytes` is raw LLVM bitcode or a bitcode wrapper.
pub fn is_llvm_bitcode(bytes: &[u8]) -> bool {
    bytes.starts_with(BC_MAGIC) || bytes.starts_with(BC_WRAPPER)
}

fn is_archive_index(name: &str) -> bool {
    name == "/" || name == "//" || name == "__.SYMDEF" || name.starts_with("__.SYMDEF")
}

/// Extract LLVM bitcode from one archive member. Fails if the member is
/// a native object without an embedded bitcode section.
pub fn extract_member_bitcode(
    name: &str,
    data: &[u8],
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    if is_llvm_bitcode(data) {
        return Ok(data.to_vec());
    }
    if let Ok(file) = object::File::parse(data) {
        for section in file.sections() {
            let Ok(sec_name) = section.name() else {
                continue;
            };
            let is_bc_section = sec_name == ".llvmbc"
                || sec_name == "__bitcode"
                || sec_name.ends_with(",__bitcode");
            if !is_bc_section {
                continue;
            }
            let bytes = section
                .data()
                .map_err(|e| format!("archive member `{name}` section `{sec_name}`: {e}"))?;
            if bytes.is_empty() {
                return Err(format!(
                    "archive member `{name}` has an empty LTO bitcode section `{sec_name}`"
                )
                .into());
            }
            return Ok(bytes.to_vec());
        }
        return Err(format!(
            "archive member `{name}` is a native object with no LLVM bitcode section \
             (expected ELF `.llvmbc` or Mach-O `__LLVM,__bitcode`; clang `-flto=full` only)"
        )
        .into());
    }
    Err(
        format!("archive member `{name}` is not LLVM bitcode and is not a recognized object file")
            .into(),
    )
}

fn reject_thinlto(name: &str, bitcode: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
    // ThinLTO summary-only bitcode still uses BC magic but names the
    // thinlto index in the bitstream. `link_in_module` cannot consume it.
    let as_ascii = String::from_utf8_lossy(bitcode);
    if as_ascii.contains("thinlto-file") || as_ascii.contains("\0THINLTO") {
        return Err(format!(
            "archive member `{name}` looks like ThinLTO-only bitcode; \
             volar-ir-build requires clang `-flto=full`"
        )
        .into());
    }
    Ok(())
}

/// Parse every LTO member of `path` and `link_in_module` them into one
/// LLVM module. Index members are skipped; every other member must yield
/// full IR bitcode.
pub fn load_lto_archive<'ctx>(
    context: &'ctx Context,
    path: &Path,
) -> Result<LlvmModule<'ctx>, Box<dyn std::error::Error>> {
    let file = std::fs::File::open(path)
        .map_err(|e| format!("failed to open LTO archive {}: {e}", path.display()))?;
    let mut archive = ar::Archive::new(file);
    let mut dest: Option<LlvmModule<'ctx>> = None;
    let mut members = 0usize;

    while let Some(entry) = archive.next_entry() {
        let mut entry = entry
            .map_err(|e| format!("failed to read archive entry in {}: {e}", path.display()))?;
        let name = String::from_utf8_lossy(entry.header().identifier()).into_owned();
        if is_archive_index(&name) {
            continue;
        }
        let mut data = Vec::new();
        entry.read_to_end(&mut data).map_err(|e| {
            format!(
                "failed to read archive member `{name}` in {}: {e}",
                path.display()
            )
        })?;
        let bitcode = extract_member_bitcode(&name, &data)?;
        reject_thinlto(&name, &bitcode)?;
        let buffer = MemoryBuffer::create_from_memory_range_copy(&bitcode, &name);
        let module = LlvmModule::parse_bitcode_from_buffer(&buffer, context)
            .map_err(|e| format!("failed to parse bitcode for archive member `{name}`: {e}"))?;
        members += 1;
        match dest.take() {
            None => dest = Some(module),
            Some(d) => {
                d.link_in_module(module).map_err(|e| {
                    format!(
                        "failed to link archive member `{name}` into {}: {e}",
                        path.display()
                    )
                })?;
                dest = Some(d);
            }
        }
    }

    dest.ok_or_else(|| {
        format!(
            "LTO archive {} has no bitcode members (saw {members} after skipping the symbol index)",
            path.display()
        )
        .into()
    })
}

/// Write `bitcode` as a single-member Unix archive at `path`.
pub fn write_bitcode_archive(
    path: &Path,
    member_name: &str,
    bitcode: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    write_bitcode_archive_members(path, &[(member_name, bitcode)])
}

/// Write several bitcode (or raw) members into a Unix archive.
pub fn write_bitcode_archive_members(
    path: &Path,
    members: &[(&str, &[u8])],
) -> Result<(), Box<dyn std::error::Error>> {
    let file = std::fs::File::create(path)?;
    let mut builder = ar::Builder::new(file);
    for (name, data) in members {
        let header = ar::Header::new(name.as_bytes().to_vec(), data.len() as u64);
        builder.append(&header, *data)?;
    }
    Ok(())
}
