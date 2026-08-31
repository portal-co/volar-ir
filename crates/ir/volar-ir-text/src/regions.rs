// @reliability: experimental
// @ai: assisted
//! [`WriteText`] + [`ParseText`] for the region/gadget side-table sections
//! of the Boolar text format (see `docs/wire-regions-gadgets-plan.md`).
//!
//! Format summary (printed after the `boolar:` circuit body):
//! ```text
//! regions {
//!   input [0, 4) -> {0}
//!   input [4, 1) -> {2}
//!   output [0, 4) -> {0}
//!   storage S0 L0 [0, 1) -> {3}
//! }
//! gadgets {
//!   gadget "pad" on {all=[0], none=[1]} aux=[const [true]]
//! }
//! ```
//!
//! Region ids are printed as bare integers; `RegionNames` (if present) is
//! printed as a comment-like `#names` suffix map and is printer-only —
//! round-trips compare ids, never names.

use crate::boolar::SavedBIrBlocks;
use crate::WriteText;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;
use volar_ir::boolar::LaneId;
use volar_ir::gadget::{AuxSource, GadgetBinding};
use volar_ir::region::{RegionEntry, RegionTable, WireAnchor};
use volar_ir_common::StorageId;

// ============================================================================
// WriteText for RegionTable
// ============================================================================

fn write_anchor(anchor: &WireAnchor, w: &mut dyn fmt::Write) -> fmt::Result {
    match anchor {
        WireAnchor::Input { start, len } => write!(w, "input [{}, {})", start, start + len),
        WireAnchor::Output { start, len } => write!(w, "output [{}, {})", start, start + len),
        WireAnchor::Storage {
            storage,
            lane,
            start,
            len,
        } => write!(w, "storage S{} L{} [{}, {})", storage.0, lane.0, start, start + len),
    }
}

fn write_region_ids(ids: &alloc::collections::BTreeSet<volar_ir::region::RegionId>, w: &mut dyn fmt::Write) -> fmt::Result {
    w.write_str("{")?;
    for (i, id) in ids.iter().enumerate() {
        if i > 0 {
            w.write_str(", ")?;
        }
        write!(w, "{}", id.0)?;
    }
    w.write_str("}")
}

impl WriteText for RegionTable {
    fn write_text(&self, w: &mut dyn fmt::Write) -> fmt::Result {
        w.write_str("regions {\n")?;
        for e in &self.entries {
            w.write_str("  ")?;
            write_anchor(&e.anchor, w)?;
            w.write_str(" -> ")?;
            write_region_ids(&e.regions, w)?;
            w.write_char('\n')?;
        }
        w.write_str("}\n")
    }
}

// ============================================================================
// WriteText for gadget bindings
// ============================================================================

fn write_selector(
    sel: &volar_ir::region::RegionSelector,
    w: &mut dyn fmt::Write,
) -> fmt::Result {
    w.write_str("{all=[")?;
    for (i, id) in sel.all_of.iter().enumerate() {
        if i > 0 {
            w.write_str(", ")?;
        }
        write!(w, "{}", id.0)?;
    }
    w.write_str("], none=[")?;
    for (i, id) in sel.none_of.iter().enumerate() {
        if i > 0 {
            w.write_str(", ")?;
        }
        write!(w, "{}", id.0)?;
    }
    w.write_str("]}")
}

fn write_aux(src: &AuxSource, w: &mut dyn fmt::Write) -> fmt::Result {
    match src {
        AuxSource::InputRange { start, len } => write!(w, "input [{}, {})", start, len),
        AuxSource::Const(bits) => {
            w.write_str("const [")?;
            for (i, &b) in bits.iter().enumerate() {
                if i > 0 {
                    w.write_str(", ")?;
                }
                w.write_str(if b { "1" } else { "0" })?;
            }
            w.write_str("]")
        }
        AuxSource::Rng(name) => write!(w, "rng \"{}\"", name),
    }
}

/// Write the `gadgets { … }` section for a list of bindings.
pub fn write_gadget_bindings(bindings: &[GadgetBinding], w: &mut dyn fmt::Write) -> fmt::Result {
    w.write_str("gadgets {\n")?;
    for b in bindings {
        w.write_str("  gadget \"")?;
        w.write_str(&b.gadget)?;
        w.write_str("\" on ")?;
        write_selector(&b.selector, w)?;
        w.write_str(" aux=[")?;
        for (i, src) in b.aux_sources.iter().enumerate() {
            if i > 0 {
                w.write_str(", ")?;
            }
            write_aux(src, w)?;
        }
        w.write_str("]\n")?;
    }
    w.write_str("}\n")
}

// ============================================================================
// Parsing
// ============================================================================

#[cfg(feature = "parse")]
pub mod parse_impl {
    use super::*;
    use volar_ir::boolar::{BIrBlock, BIrBlocks};
    use crate::parse::error::ParseError;
    use crate::parse::lexer::Lexer;
    use volar_ir::region::RegionSelector;
    use volar_ir::region::RegionId;

    /// A parsed companion snapshot: circuit + regions + gadget bindings.
    pub struct SavedBoolarWithRegions {
        pub circuit: SavedBIrBlocks,
        pub regions: RegionTable,
        pub bindings: Vec<GadgetBinding>,
    }

    /// Parse a full document: header, `boolar:` body, then optional
    /// `regions { }` and `gadgets { }` sections.
    pub fn parse_with_regions(s: &str) -> Result<SavedBoolarWithRegions, ParseError> {
        let mut lex = Lexer::new(s);

        let header = lex.read_to_newline().trim();
        if header.is_empty() {
            return Err(ParseError::MissingVersionLine);
        }
        let header = header.to_string();
        if header.as_str() != crate::boolar::FORMAT_HEADER {
            if header.starts_with("volar-ir v") || header.starts_with("volar-bir v") {
                return Err(ParseError::UnsupportedVersion(header));
            }
            return Err(ParseError::MissingVersionLine);
        }
        let section = lex.read_ident()?;
        if section != crate::boolar::FORMAT_SECTION.trim_end_matches(':') || !lex.try_byte(b':') {
            return Err(ParseError::UnexpectedToken {
                line: 2,
                col: 1,
                got: section.to_string(),
            });
        }

        let mut blocks: Vec<BIrBlock<()>> = Vec::new();
        let mut regions = RegionTable::new();
        let mut bindings = Vec::new();
        let mut seen_regions = false;
        let mut seen_gadgets = false;

        loop {
            lex.skip();
            if lex.is_eof() {
                break;
            }
            let directive = lex.read_ident()?;
            match directive {
                "begin_block" => {
                    let _id = lex.read_u32()?;
                    blocks.push(crate::parse::ir::parse_bir_block(&mut lex)?);
                }
                "regions" => {
                    if seen_regions {
                        return Err(dup("regions"));
                    }
                    seen_regions = true;
                    lex.expect_byte(b'{')?;
                    regions = parse_region_entries(&mut lex)?;
                }
                "gadgets" => {
                    if seen_gadgets {
                        return Err(dup("gadgets"));
                    }
                    seen_gadgets = true;
                    lex.expect_byte(b'{')?;
                    bindings = parse_gadget_section(&mut lex)?;
                }
                other => return Err(ParseError::UnknownDirective(other.into())),
            }
        }

        Ok(SavedBoolarWithRegions {
            circuit: SavedBIrBlocks {
                blocks: BIrBlocks {
                    blocks,
                    pre_init: Vec::new(),
                },
            },
            regions,
            bindings,
        })
    }

    fn dup(name: &'static str) -> ParseError {
        ParseError::DuplicateSection(name)
    }

    fn parse_range(lex: &mut Lexer) -> Result<(u64, u64), ParseError> {
        lex.expect_byte(b'[')?;
        let start = lex.read_u64()?;
        lex.expect_byte(b',')?;
        let end = lex.read_u64()?;
        lex.expect_byte(b')')?;
        Ok((start, end))
    }

    fn parse_region_ids(lex: &mut Lexer) -> Result<Vec<u32>, ParseError> {
        lex.expect_byte(b'{')?;
        let mut out = Vec::new();
        loop {
            lex.skip();
            if lex.try_byte(b'}') {
                break;
            }
            out.push(lex.read_u32()?);
            lex.skip();
            if lex.try_byte(b',') {
                continue;
            }
            lex.expect_byte(b'}')?;
            break;
        }
        Ok(out)
    }

    fn parse_region_entries(lex: &mut Lexer) -> Result<RegionTable, ParseError> {
        let mut table = RegionTable::new();
        loop {
            lex.skip();
            if lex.try_byte(b'}') {
                break;
            }
            let kind = lex.read_ident()?;
            let anchor = match kind {
                "input" => {
                    let (s, e) = parse_range(lex)?;
                    WireAnchor::Input {
                        start: u32::try_from(s).map_err(|_| bad_range())?,
                        len: u32::try_from(e.saturating_sub(s)).map_err(|_| bad_range())?,
                    }
                }
                "output" => {
                    let (s, e) = parse_range(lex)?;
                    WireAnchor::Output {
                        start: u32::try_from(s).map_err(|_| bad_range())?,
                        len: u32::try_from(e.saturating_sub(s)).map_err(|_| bad_range())?,
                    }
                }
                "storage" => {
                    lex.expect_byte(b'S')?;
                    let st = lex.read_u32()?;
                    lex.expect_byte(b'L')?;
                    let lane = lex.read_u32()?;
                    let (s, e) = parse_range(lex)?;
                    WireAnchor::Storage {
                        storage: StorageId(st),
                        lane: LaneId(lane),
                        start: s,
                        len: e.saturating_sub(s),
                    }
                }
                other => return Err(ParseError::UnknownDirective(other.into())),
            };
            // Arrow `->` is not an ident (leading `-`); consume it directly.
            lex.expect_byte(b'-')?;
            lex.expect_byte(b'>')?;
            let ids = parse_region_ids(lex)?;
            if ids.is_empty() {
                return Err(ParseError::EmptyRegionSet);
            }
            table.entries.push(RegionEntry {
                anchor,
                regions: ids.into_iter().map(RegionId).collect(),
            });
        }
        Ok(table)
    }

    fn bad_range() -> ParseError {
        ParseError::InvalidInt(String::from("range out of u32"))
    }

    fn parse_id_list(lex: &mut Lexer) -> Result<Vec<u32>, ParseError> {
        lex.expect_byte(b'[')?;
        let mut out = Vec::new();
        loop {
            lex.skip();
            if lex.try_byte(b']') {
                break;
            }
            out.push(lex.read_u32()?);
            lex.skip();
            if lex.try_byte(b',') {
                continue;
            }
            lex.expect_byte(b']')?;
            break;
        }
        Ok(out)
    }

    fn parse_aux(lex: &mut Lexer) -> Result<AuxSource, ParseError> {
        let kw = lex.read_ident()?;
        match kw {
            "input" => {
                let (s, e) = parse_range(lex)?;
                Ok(AuxSource::InputRange {
                    start: u32::try_from(s).map_err(|_| bad_range())?,
                    len: u32::try_from(e.saturating_sub(s)).map_err(|_| bad_range())?,
                })
            }
            "const" => {
                lex.expect_byte(b'[')?;
                let mut bits = Vec::new();
                loop {
                    lex.skip();
                    if lex.try_byte(b']') {
                        break;
                    }
                    let raw = lex.read_u32()?;
                    match raw {
                        0 => bits.push(false),
                        1 => bits.push(true),
                        _ => {
                            return Err(ParseError::UnexpectedToken {
                                line: 0,
                                col: 0,
                                got: format!("bit {}", raw),
                            })
                        }
                    }
                    lex.skip();
                    if lex.try_byte(b',') {
                        continue;
                    }
                    lex.expect_byte(b']')?;
                    break;
                }
                Ok(AuxSource::Const(bits))
            }
            "rng" => {
                let name = lex.read_string()?;
                Ok(AuxSource::Rng(name))
            }
            other => Err(ParseError::UnknownDirective(other.into())),
        }
    }

    fn parse_gadget_section(lex: &mut Lexer) -> Result<Vec<GadgetBinding>, ParseError> {
        let mut out = Vec::new();
        loop {
            lex.skip();
            if lex.try_byte(b'}') {
                break;
            }
            lex.read_ident_kw("gadget")?;
            let name = lex.read_string()?;
            lex.read_ident_kw("on")?;
            lex.expect_byte(b'{')?;
            let mut all_of = Vec::new();
            let mut none_of = Vec::new();
            loop {
                lex.skip();
                if lex.try_byte(b'}') {
                    break;
                }
                let kw = lex.read_ident()?;
                match kw {
                    "all" => {
                        lex.expect_byte(b'=')?;
                        all_of = parse_id_list(lex)?
                    }
                    "none" => {
                        lex.expect_byte(b'=')?;
                        none_of = parse_id_list(lex)?
                    }
                    other => return Err(ParseError::UnknownDirective(other.into())),
                }
                lex.skip();
                if lex.try_byte(b',') {
                    continue;
                }
                lex.expect_byte(b'}')?;
                break;
            }
            lex.read_ident_kw("aux")?;
            lex.expect_byte(b'=')?;
            lex.expect_byte(b'[')?;
            let mut aux_sources = Vec::new();
            loop {
                lex.skip();
                if lex.try_byte(b']') {
                    break;
                }
                aux_sources.push(parse_aux(lex)?);
                lex.skip();
                if lex.try_byte(b',') {
                    continue;
                }
                lex.expect_byte(b']')?;
                break;
            }
            out.push(GadgetBinding {
                gadget: name,
                selector: RegionSelector {
                    all_of: all_of.into_iter().map(RegionId).collect(),
                    none_of: none_of.into_iter().map(RegionId).collect(),
                },
                aux_sources,
                rng_source: None,
            });
        }
        Ok(out)
    }

    impl Lexer<'_> {
        fn read_ident_kw(&mut self, kw: &str) -> Result<(), ParseError> {
            let got = self.read_ident()?;
            if got != kw {
                return Err(ParseError::UnexpectedToken {
                    line: self.pos().line,
                    col: self.pos().col,
                    got: got.into(),
                });
            }
            Ok(())
        }
    }
}
