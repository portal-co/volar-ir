// @reliability: experimental
// @ai: assisted
//! [`WriteText`] + parse support for the **typed** region/gadget side-table
//! sections (see `docs/typed-gadgets-and-region-threading-plan.md`).
//!
//! These mirror the bit-level `regions { }` / `gadgets { }` sections in
//! [`crate::regions`], with typed anchors and typed gadget specs:
//!
//! ```text
//! typed_regions {
//!   input p0 [0, 8) -> {0}
//!   block_input b1 p0 [0, 8) -> {1}
//!   func_input f0 p0 [0, 8) -> {1}
//!   output o0 [0, 8) -> {0}
//!   func_output f0 r0 [0, 8) -> {0}
//!   storage S1 T3 [0, 4) -> {2}
//! }
//! typed_gadgets {
//!   gadget "pad8" on {all=[0], none=[1]} aux=[const <0x5a>, input p1 [0, 8), rng "n"]
//! }
//! typed_gadget_specs {
//!   spec "pad8" data T0x1 aux T2x1 encrypt_inline
//! }
//! ```
//!
//! Typed gadget *bodies* (`VCircuit`s) are not text-serialized here — specs
//! round-trip as **port-signature stubs** (`typed_gadget_specs`), sufficient
//! for table validation and binding resolution; bodies stay in rkyv/binary
//! artifacts. Round-trips therefore compare anchors, selectors, bindings,
//! and spec port signatures by name.

use crate::WriteText;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;
use volar_ir::gadget::PortKind;
use volar_ir::region::{RegionId, RegionSelector};
use volar_ir::typed_gadget::{
    TypedAnchor, TypedAuxSource, TypedGadgetBinding, TypedGadgetLibrary, TypedPort,
    TypedRegionEntry, TypedRegionTable,
};
use volar_ir_common::{Constant, StorageId};

// ============================================================================
// Printing
// ============================================================================

fn write_typed_anchor(anchor: &TypedAnchor, w: &mut dyn fmt::Write) -> fmt::Result {
    match anchor {
        TypedAnchor::Input { param, start, len } => {
            write!(w, "input p{} [{}, {})", param, start, start + len)
        }
        TypedAnchor::BlockInput {
            block,
            param,
            start,
            len,
        } => write!(
            w,
            "block_input b{} p{} [{}, {})",
            block.0, param, start, start + len
        ),
        TypedAnchor::FuncInput {
            func,
            param,
            start,
            len,
        } => write!(
            w,
            "func_input f{} p{} [{}, {})",
            func, param, start, start + len
        ),
        TypedAnchor::Output { out, start, len } => {
            write!(w, "output o{} [{}, {})", out, start, start + len)
        }
        TypedAnchor::FuncOutput {
            func,
            result,
            start,
            len,
        } => write!(
            w,
            "func_output f{} r{} [{}, {})",
            func, result, start, start + len
        ),
        TypedAnchor::Storage {
            storage,
            ty,
            addr_start,
            addr_len,
        } => write!(
            w,
            "storage S{} T{} [{}, {})",
            storage.0,
            ty.0,
            addr_start,
            addr_start + addr_len
        ),
    }
}

fn write_ids(ids: &alloc::collections::BTreeSet<RegionId>, w: &mut dyn fmt::Write) -> fmt::Result {
    w.write_str("{")?;
    for (i, id) in ids.iter().enumerate() {
        if i > 0 {
            w.write_str(", ")?;
        }
        write!(w, "{}", id.0)?;
    }
    w.write_str("}")
}

impl WriteText for TypedRegionTable {
    fn write_text(&self, w: &mut dyn fmt::Write) -> fmt::Result {
        w.write_str("typed_regions {\n")?;
        for e in &self.entries {
            w.write_str("  ")?;
            write_typed_anchor(&e.anchor, w)?;
            w.write_str(" -> ")?;
            write_ids(&e.regions, w)?;
            w.write_char('\n')?;
        }
        w.write_str("}\n")
    }
}

fn write_selector(sel: &RegionSelector, w: &mut dyn fmt::Write) -> fmt::Result {
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

fn write_typed_aux(src: &TypedAuxSource, w: &mut dyn fmt::Write) -> fmt::Result {
    match src {
        TypedAuxSource::InputRange { param, start } => write!(w, "input p{} [{}]", param, start),
        TypedAuxSource::Const(words) => {
            w.write_str("const ")?;
            for (i, word) in words.iter().enumerate() {
                if i > 0 {
                    w.write_str(", ")?;
                }
                if word.hi == 0 {
                    write!(w, "<{}>", word.lo)?;
                } else {
                    write!(w, "<{}:{}>", word.lo, word.hi)?;
                }
            }
            Ok(())
        }
        TypedAuxSource::Rng(name) => write!(w, "rng \"{}\"", name),
    }
}

/// Write the `typed_gadgets { … }` bindings section.
pub fn write_typed_gadget_bindings(
    bindings: &[TypedGadgetBinding],
    w: &mut dyn fmt::Write,
) -> fmt::Result {
    w.write_str("typed_gadgets {\n")?;
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
            write_typed_aux(src, w)?;
        }
        w.write_str("]")?;
        if let Some(rng) = &b.rng_source {
            w.write_str(" rng_source=\"")?;
            w.write_str(rng)?;
            w.write_str("\"")?;
        }
        w.write_char('\n')?;
    }
    w.write_str("}\n")
}

fn write_port(p: &TypedPort, w: &mut dyn fmt::Write) -> fmt::Result {
    let kind = match p.kind {
        PortKind::Data => "data",
        PortKind::Aux => "aux",
        PortKind::Rng => "rng",
    };
    {
        write!(w, "{}", kind)?;
        if !p.name.is_empty() {
            write!(w, ":\"{}\"", p.name)?;
        }
        write!(w, " T{}:{}", p.ty.0, p.count)
    }
}

/// A port-signature stub for one typed gadget spec: name + ports. Bodies are
/// not text-serialized (see module docs).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TypedGadgetSpecStub {
    pub name: String,
    pub ports: Vec<TypedPort>,
}

impl WriteText for TypedGadgetLibrary {
    fn write_text(&self, w: &mut dyn fmt::Write) -> fmt::Result {
        w.write_str("typed_gadget_specs {\n")?;
        for spec in &self.gadgets {
            w.write_str("  spec \"")?;
            w.write_str(&spec.name)?;
            w.write_str("\"")?;
            for p in &spec.ports {
                w.write_str(" ")?;
                write_port(p, w)?;
            }
            w.write_char('\n')?;
        }
        w.write_str("}\n")
    }
}

/// Extract the port-signature stubs of a library (what the text format
/// round-trips).
pub fn spec_stubs(lib: &TypedGadgetLibrary) -> Vec<TypedGadgetSpecStub> {
    lib.gadgets
        .iter()
        .map(|s| TypedGadgetSpecStub {
            name: s.name.clone(),
            ports: s.ports.clone(),
        })
        .collect()
}

// ============================================================================
// Parsing
// ============================================================================

#[cfg(feature = "parse")]
pub mod parse_impl {
    use super::*;
    use crate::parse::error::ParseError;
    use crate::parse::lexer::Lexer;

    fn bad_range() -> ParseError {
        ParseError::InvalidInt(String::from("range out of u16"))
    }

    fn parse_range16(lex: &mut Lexer) -> Result<(u16, u16), ParseError> {
        lex.expect_byte(b'[')?;
        let start = lex.read_u64()?;
        lex.expect_byte(b',')?;
        let end = lex.read_u64()?;
        lex.expect_byte(b')')?;
        let len = end.saturating_sub(start);
        Ok((
            u16::try_from(start).map_err(|_| bad_range())?,
            u16::try_from(len).map_err(|_| bad_range())?,
        ))
    }

    fn parse_ids(lex: &mut Lexer) -> Result<Vec<u32>, ParseError> {
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

    fn parse_typed_anchor(lex: &mut Lexer) -> Result<TypedAnchor, ParseError> {
        let kind = lex.read_ident()?;
        let anchor = match kind {
            "input" => {
                lex.expect_byte(b'p')?;
                let param = lex.read_u32()?;
                let (start, len) = parse_range16(lex)?;
                TypedAnchor::Input { param, start, len }
            }
            "block_input" => {
                lex.expect_byte(b'b')?;
                let block = lex.read_u32()?;
                lex.expect_byte(b'p')?;
                let param = lex.read_u32()?;
                let (start, len) = parse_range16(lex)?;
                TypedAnchor::BlockInput {
                    block: volar_ir::ir::IRBlockId(block),
                    param,
                    start,
                    len,
                }
            }
            "func_input" => {
                lex.expect_byte(b'f')?;
                let func = lex.read_u32()?;
                lex.expect_byte(b'p')?;
                let param = lex.read_u32()?;
                let (start, len) = parse_range16(lex)?;
                TypedAnchor::FuncInput {
                    func,
                    param,
                    start,
                    len,
                }
            }
            "output" => {
                lex.expect_byte(b'o')?;
                let out = lex.read_u32()?;
                let (start, len) = parse_range16(lex)?;
                TypedAnchor::Output { out, start, len }
            }
            "func_output" => {
                lex.expect_byte(b'f')?;
                let func = lex.read_u32()?;
                lex.expect_byte(b'r')?;
                let result = lex.read_u32()?;
                let (start, len) = parse_range16(lex)?;
                TypedAnchor::FuncOutput {
                    func,
                    result,
                    start,
                    len,
                }
            }
            "storage" => {
                lex.expect_byte(b'S')?;
                let storage = lex.read_u32()?;
                lex.expect_byte(b'T')?;
                let ty = lex.read_u32()?;
                lex.expect_byte(b'[')?;
                let addr_start = lex.read_u64()?;
                lex.expect_byte(b',')?;
                let addr_end = lex.read_u64()?;
                lex.expect_byte(b')')?;
                TypedAnchor::Storage {
                    storage: StorageId(storage),
                    ty: volar_ir::ir::IRTypeId(ty),
                    addr_start,
                    addr_len: addr_end.saturating_sub(addr_start),
                }
            }
            other => return Err(ParseError::UnknownDirective(other.into())),
        };
        Ok(anchor)
    }

    /// Parse the body of a `typed_regions { … }` section (the opening brace
    /// is already consumed).
    pub fn parse_typed_region_entries(lex: &mut Lexer) -> Result<TypedRegionTable, ParseError> {
        let mut table = TypedRegionTable::new();
        loop {
            lex.skip();
            if lex.try_byte(b'}') {
                break;
            }
            let anchor = parse_typed_anchor(lex)?;
            lex.expect_byte(b'-')?;
            lex.expect_byte(b'>')?;
            let ids = parse_ids(lex)?;
            if ids.is_empty() {
                return Err(ParseError::EmptyRegionSet);
            }
            table.entries.push(TypedRegionEntry {
                anchor,
                regions: ids.into_iter().map(RegionId).collect(),
            });
        }
        Ok(table)
    }

    fn parse_selector(lex: &mut Lexer) -> Result<RegionSelector, ParseError> {
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
        Ok(RegionSelector {
            all_of: all_of.into_iter().map(RegionId).collect(),
            none_of: none_of.into_iter().map(RegionId).collect(),
        })
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

    fn parse_word_const(lex: &mut Lexer) -> Result<Constant, ParseError> {
        lex.expect_byte(b'<')?;
        let lo = lex.read_u128()?;
        // Optional high half (printed only when nonzero).
        let mut hi = 0u128;
        lex.skip();
        if lex.peek_byte() == Some(b':') {
            lex.expect_byte(b':')?;
            hi = lex.read_u128()?;
        }
        lex.expect_byte(b'>')?;
        Ok(Constant { hi, lo })
    }

    fn parse_typed_aux(lex: &mut Lexer) -> Result<TypedAuxSource, ParseError> {
        let kw = lex.read_ident()?;
        match kw {
            "input" => {
                lex.expect_byte(b'p')?;
                let param = lex.read_u32()?;
                lex.expect_byte(b'[')?;
                let start = lex.read_u64()?;
                lex.expect_byte(b']')?;
                Ok(TypedAuxSource::InputRange {
                    param,
                    start: u16::try_from(start).map_err(|_| bad_range())?,
                })
            }
            "const" => {
                let mut words = Vec::new();
                loop {
                    match lex.peek_byte() {
                        Some(b'<') => words.push(parse_word_const(lex)?),
                        // A comma continues this const source only if another
                        // word follows; otherwise it's the aux-list separator.
                        Some(b',') => {
                            let save = lex.save();
                            lex.try_byte(b',');
                            if lex.peek_byte() != Some(b'<') {
                                lex.restore(save);
                                break;
                            }
                        }
                        _ => break,
                    }
                }
                Ok(TypedAuxSource::Const(words))
            }
            "rng" => {
                let name = lex.read_string()?;
                Ok(TypedAuxSource::Rng(name))
            }
            other => Err(ParseError::UnknownDirective(other.into())),
        }
    }

    /// Parse the body of a `typed_gadgets { … }` bindings section.
    pub fn parse_typed_gadget_section(
        lex: &mut Lexer,
    ) -> Result<Vec<TypedGadgetBinding>, ParseError> {
        let mut out = Vec::new();
        loop {
            lex.skip();
            if lex.try_byte(b'}') {
                break;
            }
            lex.read_ident_kw("gadget")?;
            let name = lex.read_string()?;
            lex.read_ident_kw("on")?;
            let selector = parse_selector(lex)?;
            lex.read_ident_kw("aux")?;
            lex.expect_byte(b'=')?;
            lex.expect_byte(b'[')?;
            let mut aux_sources = Vec::new();
            loop {
                lex.skip();
                if lex.try_byte(b']') {
                    break;
                }
                aux_sources.push(parse_typed_aux(lex)?);
                lex.skip();
                if lex.try_byte(b',') {
                    continue;
                }
                if lex.try_byte(b']') {
                    break;
                }
                lex.expect_byte(b']')?;
                break;
            }
            let mut rng_source = None;
            lex.skip();
            if lex.try_keyword("rng_source") {
                lex.expect_byte(b'=')?;
                rng_source = Some(lex.read_string()?);
            }
            out.push(TypedGadgetBinding {
                gadget: name,
                selector,
                aux_sources,
                rng_source,
            });
        }
        Ok(out)
    }

    fn parse_port(lex: &mut Lexer) -> Result<TypedPort, ParseError> {
        let kind = match lex.read_ident()? {
            "data" => PortKind::Data,
            "aux" => PortKind::Aux,
            "rng" => PortKind::Rng,
            other => return Err(ParseError::UnknownDirective(other.into())),
        };
        let name = if lex.peek_byte() == Some(b':') {
            lex.expect_byte(b':')?;
            lex.read_string()?
        } else {
            String::new()
        };
        lex.expect_byte(b'T')?;
        let ty = lex.read_u32()?;
        lex.expect_byte(b':')?;
        let count = lex.read_usize()?;
        Ok(TypedPort {
            name,
            kind,
            ty: volar_ir::ir::IRTypeId(ty),
            count,
        })
    }

    /// Parse the body of a `typed_gadget_specs { … }` section.
    pub fn parse_typed_gadget_specs(lex: &mut Lexer) -> Result<Vec<TypedGadgetSpecStub>, ParseError> {
        let mut out = Vec::new();
        loop {
            lex.skip();
            if lex.try_byte(b'}') {
                break;
            }
            lex.read_ident_kw("spec")?;
            let name = lex.read_string()?;
            let mut ports = Vec::new();
            loop {
                if lex.peek_byte() != Some(b'd') && lex.peek_byte() != Some(b'a')
                    && lex.peek_byte() != Some(b'r')
                {
                    break;
                }
                ports.push(parse_port(lex)?);
            }
            out.push(TypedGadgetSpecStub { name, ports });
        }
        Ok(out)
    }

    impl Lexer<'_> {
        fn try_keyword(&mut self, kw: &str) -> bool {
            let save = self.save();
            match self.read_ident() {
                Ok(got) if got == kw => true,
                _ => {
                    self.restore(save);
                    false
                }
            }
        }
    }
}

#[cfg(all(test, feature = "parse"))]
mod tests {
    extern crate std;
    use super::*;
    use crate::parse::lexer::Lexer;
    use alloc::collections::BTreeSet;
    use alloc::vec;
    use volar_ir::region::RegionId;

    fn sample_table() -> TypedRegionTable {
        TypedRegionTable {
            entries: vec![
                TypedRegionEntry {
                    anchor: TypedAnchor::Input { param: 0, start: 0, len: 8 },
                    regions: BTreeSet::from([RegionId(0)]),
                },
                TypedRegionEntry {
                    anchor: TypedAnchor::BlockInput {
                        block: volar_ir::ir::IRBlockId(1),
                        param: 0,
                        start: 2,
                        len: 3,
                    },
                    regions: BTreeSet::from([RegionId(1)]),
                },
                TypedRegionEntry {
                    anchor: TypedAnchor::FuncInput {
                        func: 2,
                        param: 0,
                        start: 0,
                        len: 4,
                    },
                    regions: BTreeSet::from([RegionId(2)]),
                },
                TypedRegionEntry {
                    anchor: TypedAnchor::Output { out: 0, start: 1, len: 4 },
                    regions: BTreeSet::from([RegionId(0)]),
                },
                TypedRegionEntry {
                    anchor: TypedAnchor::FuncOutput {
                        func: 0,
                        result: 1,
                        start: 0,
                        len: 2,
                    },
                    regions: BTreeSet::from([RegionId(3)]),
                },
                TypedRegionEntry {
                    anchor: TypedAnchor::Storage {
                        storage: StorageId(1),
                        ty: volar_ir::ir::IRTypeId(3),
                        addr_start: 0,
                        addr_len: 4,
                    },
                    regions: BTreeSet::from([RegionId(4)]),
                },
            ],
            names: alloc::collections::BTreeMap::new(),
        }
    }

    fn write_str(t: &TypedRegionTable) -> String {
        let mut s = String::new();
        WriteText::write_text(t, &mut s).expect("writes");
        s
    }

    #[test]
    fn typed_regions_text_roundtrip() {
        let table = sample_table();
        let text = write_str(&table);
        let mut lex = Lexer::new(&text);
        assert_eq!(lex.read_ident().unwrap(), "typed_regions");
        lex.expect_byte(b'{').unwrap();
        let parsed = parse_impl::parse_typed_region_entries(&mut lex).expect("parses");
        assert_eq!(parsed, table);
    }

    #[test]
    fn typed_gadget_bindings_text_roundtrip() {
        let bindings = vec![
            TypedGadgetBinding {
                gadget: String::from("pad8"),
                selector: RegionSelector {
                    all_of: BTreeSet::from([RegionId(0)]),
                    none_of: BTreeSet::from([RegionId(1)]),
                },
                aux_sources: vec![
                    TypedAuxSource::Const(vec![
                        Constant { hi: 0, lo: 0x5a },
                        Constant { hi: 1, lo: u128::MAX },
                    ]),
                    TypedAuxSource::InputRange { param: 1, start: 0 },
                    TypedAuxSource::Rng(String::from("nonce")),
                ],
                rng_source: Some(String::from("stream")),
            },
            TypedGadgetBinding {
                gadget: String::from("tiny"),
                selector: RegionSelector::all_of([RegionId(0)]),
                aux_sources: vec![],
                rng_source: None,
            },
        ];
        let mut s = String::new();
        write_typed_gadget_bindings(&bindings, &mut s).expect("writes");
        std::eprintln!("TYPED_GADGETS_TEXT:\n{s}");
        let mut lex = Lexer::new(&s);
        assert_eq!(lex.read_ident().unwrap(), "typed_gadgets");
        lex.expect_byte(b'{').unwrap();
        let parsed = parse_impl::parse_typed_gadget_section(&mut lex).expect("parses");
        assert_eq!(parsed, bindings);
    }

    #[test]
    fn typed_gadget_specs_text_roundtrip() {
        let lib = TypedGadgetLibrary::new().with(volar_ir::typed_gadget::TypedGadgetSpec {
            name: String::from("pad8"),
            ports: vec![
                TypedPort {
                    name: String::from("data"),
                    kind: PortKind::Data,
                    ty: volar_ir::ir::IRTypeId(0),
                    count: 1,
                },
                TypedPort {
                    name: String::from("key"),
                    kind: PortKind::Aux,
                    ty: volar_ir::ir::IRTypeId(2),
                    count: 3,
                },
            ],
            encrypt: volar_ir::circuit::VCircuit::new(vec![]),
            decrypt: None,
        });
        let mut s = String::new();
        WriteText::write_text(&lib, &mut s).expect("writes");
        let _ = s;
        let mut s2 = String::new();
        WriteText::write_text(&lib, &mut s2).expect("writes");
        std::eprintln!("TYPED_SPECS_TEXT:\n{s2}");
        let mut lex = Lexer::new(&s2);
        assert_eq!(lex.read_ident().unwrap(), "typed_gadget_specs");
        lex.expect_byte(b'{').unwrap();
        let parsed = parse_impl::parse_typed_gadget_specs(&mut lex).expect("parses");
        assert_eq!(parsed, spec_stubs(&lib));
    }
}
