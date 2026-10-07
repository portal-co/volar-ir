// @pinnedness: unpinned
// @stability: unstable
// @ai: assisted
//! C99 backend for `LirTarget`.
//!
//! Emits a complete `.c` file suitable for testing with `cc -O0 -std=c99`.
//! Each function becomes a C function. SSA values are `vN` locals; block
//! parameters are pre-declared `blockM_pK` variables at the function top.
//!
//! # Record-then-render expression folding
//!
//! Instructions are not rendered to text as they are recorded: each function
//! records a structured item stream (lazy [`Expr`] trees referencing value
//! IDs), and `end_function` renders it through a use-count-driven pass:
//!
//! - **Lazy values**: a definition's C expression is materialized only where
//!   it is actually needed.
//! - **Dead-value elimination**: a pure or memory-read definition with zero
//!   surviving uses is never emitted, cascading through its operands — an
//!   aggregate call result unpacked into scalars costs nothing for unread
//!   fields.
//! - **Assignments-as-values**: a definition with exactly one use is folded
//!   into that use as a sub-expression (its definition statement
//!   disappears). A call result with exactly one use folds into the
//!   consuming statement; when two or more side-effecting folds land in the
//!   same statement they are sequenced with the comma operator through
//!   fresh temporaries (`(t0 = f(x), t1 = g(y), t0 + t1)`), since C leaves
//!   their evaluation order unspecified.
//! - Definitions with two or more uses, call/RNG statements, and memory
//!   reads outside a pure window are materialized as `T vN = expr;` at
//!   their definition position, as before.
//!
//! Soundness: inlining only moves evaluation of a definition into a later
//! statement from which every referenced SSA name still holds the same
//! value. The single-use fold is allowed only when every item between the
//! definition and its use is another pure definition — no label, terminator,
//! store, call, or memory read may intervene, so no intervening edge can
//! reassign a block parameter and no intervening effect can change what the
//! folded expression reads.
//!
//! # Jump-edge parallel-assignment safety
//!
//! Jump/branch/switch argument expressions are assigned directly to the
//! target's `blockM_pK` variables unless an argument expression (after
//! folding) mentions one of those variables — the swap/clobber hazard the
//! two-phase `_tN` temporaries exist for. Only then are temporaries used.
//!
//! # Phase 2 additions
//!
//! Supports `LirType::Arr` and `LirType::Struct`:
//! - Arrays are typedef'd as `typedef struct { T data[N]; } Arr_T_N;`
//! - Structs are typedef'd from their `StructDef` fields.
//! - `arr_set` emits a copy-and-mutate pattern (functional update).
//! - `call_extern` emits `extern` declarations in `finish()`.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as FmtWrite,
    string::String,
    vec::Vec,
};
use volar_ir_common::{Type as NativeType, TypeTable};
use volar_lir::{
    BranchTarget, HeapAllocExt, IcmpPred, LirAbi, LirTarget, LirType, StackAllocExt, StructDef,
    StructId,
};

pub use volar_lir::NameConfig;

/// Sanitize a field name for C: if it starts with a digit (e.g. tuple fields
/// "0", "1", …), prefix with `_` to make it a valid C identifier.
fn c_field_name(name: &str) -> String {
    if name.starts_with(|c: char| c.is_ascii_digit()) {
        format!("_{name}")
    } else {
        name.to_string()
    }
}

/// True when `hay` contains `ident` as a whole identifier (the character
/// after a match must not continue the identifier).
fn contains_ident(hay: &str, ident: &str) -> bool {
    let mut from = 0;
    while let Some(pos) = hay[from..].find(ident) {
        let abs = from + pos + ident.len();
        let next = hay[abs..].chars().next();
        let continues = next
            .map(|c| c.is_alphanumeric() || c == '_')
            .unwrap_or(false);
        if !continues {
            return true;
        }
        from += pos + ident.len();
    }
    false
}

// ============================================================================
// Buffered expression IR (record-then-render folding)
// ============================================================================

/// A lazily-rendered C expression. Leaves reference values by ID; rendering
/// substitutes materialized values by name and folds single-use definitions
/// into their use site (see the module docs).
enum Expr {
    /// Reference to a value by ID.
    Name(u32),
    Const(i64),
    /// `{l} {op} {r}`.
    Bin(&'static str, Box<Expr>, Box<Expr>),
    /// Prefix unary: `{op}{v}`.
    Un(&'static str, Box<Expr>),
    /// C cast: `({ty})({inner})`.
    Cast(String, Box<Expr>),
    /// Comparison, optionally through a signed cast on both operands.
    Icmp {
        op: &'static str,
        signed: Option<&'static str>,
        l: Box<Expr>,
        r: Box<Expr>,
    },
    /// `{c} ? {t} : {e}`.
    Select(Box<Expr>, Box<Expr>, Box<Expr>),
    /// Member access: `{inner}.{field}` (a local aggregate's field).
    Field(Box<Expr>, String),
    /// `{base}[{idx}]` — `local.data[i]` for a local array value, or a memory
    /// read when the base is a pointer.
    Index(Box<Expr>, Box<Expr>),
    /// Memory read: `*{ptr}`.
    Deref(Box<Expr>),
    /// Compound literal: `({ty}){{ ... }}`.
    Compound {
        ty: String,
        inits: Vec<Init>,
    },
    /// Pure helper call: `{name}({args})` (e.g. `volar_gf8_mul`).
    FnPure(String, Vec<Expr>),
    /// Function call (side-effecting): `{name}({args})`.
    Call(String, Vec<Expr>),
}

/// One designated initializer of an [`Expr::Compound`].
enum Init {
    /// `.{field} = {expr}` (struct fields; array `data` with one element).
    One(String, Expr),
    /// `.{field} = { {e0}, {e1}, ... }` (array `data` members).
    List(String, Vec<Expr>),
}

/// What kind of definition a recorded value has; drives folding decisions.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ValKind {
    /// No side effects, reads no memory: freely foldable and droppable.
    Pure,
    /// Reads memory (`*p`, `p[i]` through a pointer): droppable when unused;
    /// foldable into its single use only across a pure window.
    MemRead,
    /// Result of a call: the statement always runs (side effects); the value
    /// folds into its single use only across a pure window.
    CallRet,
    /// Written by the RNG hook through `&vN` — the address escapes, so the
    /// named local must exist and the statement always runs.
    RngVal,
}

/// One recorded instruction or control-flow construct.
enum Item {
    /// `{c_type} v{id} = expr;` (RNG values render their two-statement form).
    Def {
        id: u32,
        c_type: String,
        expr: Expr,
        kind: ValKind,
    },
    /// A statement-level expression with no result (void call): `expr;`.
    Effect(Expr),
    /// `{place} = {val};` where `place` is a `Deref` or pointer `Index`.
    Store {
        place: Expr,
        val: Expr,
    },
    /// `block{b}:;`
    Label(u32),
    Jump {
        target: u32,
        args: Vec<Expr>,
    },
    Branch {
        cond: Expr,
        then_b: u32,
        then_args: Vec<Expr>,
        else_b: u32,
        else_args: Vec<Expr>,
    },
    Switch {
        index: Expr,
        cases: Vec<(i64, u32, Vec<Expr>)>,
        default_b: u32,
        default_args: Vec<Expr>,
    },
    Ret(Option<Expr>),
}

// ============================================================================
// Handles
// ============================================================================

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CValue(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CBlock(pub u32);

// ============================================================================
// Per-block metadata
// ============================================================================

struct BlockMeta {
    /// How many parameters this block has.
    param_count: u32,
    /// The value-ID of the first param; params are IDs [base, base+count).
    param_base: u32,
}

// ============================================================================
// Per-function state
// ============================================================================

struct FunctionState {
    name: String,
    ret_ty: Option<LirType>,

    /// Number of C function parameters (= entry block's function-level params).
    func_param_count: usize,

    /// Block-param pre-declarations, emitted before any label.
    preamble: String,
    /// Recorded instruction stream (rendered at `end_function` by the
    /// folding pass).
    items: Vec<Item>,

    /// One entry per block.
    blocks: Vec<BlockMeta>,
    /// Maps value ID → C name (`blockM_pK` or `vN`).
    value_name: Vec<String>,
    /// Maps value ID → pre-computed C type string (e.g. `"uint8_t"`, `"Arr_U8_16"`).
    value_ctype: Vec<String>,
    /// Maps value ID → LirType (for type queries in the backend).
    value_type: Vec<LirType>,
    /// `Some(block)` when the value is that block's parameter (a bare,
    /// pre-declared name that edges reassign, not a definition).
    value_block_param: Vec<Option<u32>>,

    /// Next fresh value ID.
    next_value: u32,
    /// Which block is currently being emitted.
    current_block: Option<u32>,
}

impl FunctionState {
    fn alloc_value(&mut self, ty: LirType, c_type: String, name: String) -> CValue {
        let id = self.next_value;
        self.next_value += 1;
        self.value_name.push(name);
        self.value_ctype.push(c_type);
        self.value_type.push(ty);
        self.value_block_param.push(None);
        CValue(id)
    }

    /// Record a definition: `{c_type} v{id} = <rendered expr>;`. The text is
    /// produced later by the folding renderer.
    fn record_def(&mut self, ty: LirType, c_type: String, expr: Expr, kind: ValKind) -> CValue {
        let id = self.next_value;
        self.next_value += 1;
        self.value_name.push(format!("v{id}"));
        self.value_ctype.push(c_type.clone());
        self.value_type.push(ty);
        self.value_block_param.push(None);
        self.items.push(Item::Def {
            id,
            c_type,
            expr,
            kind,
        });
        CValue(id)
    }

    fn name_of(&self, v: CValue) -> &str {
        &self.value_name[v.0 as usize]
    }

    fn type_of(&self, v: CValue) -> &LirType {
        &self.value_type[v.0 as usize]
    }

    fn ctype_of(&self, v: CValue) -> &str {
        &self.value_ctype[v.0 as usize]
    }
}

// ============================================================================
// The backend
// ============================================================================

/// C99 backend implementing `LirTarget`.
///
/// Call `begin_function` / emit instructions / `end_function` one or more
/// times, then call `finish()` to obtain the complete C source file.
pub struct CBackend {
    completed_functions: Vec<String>,
    current: Option<FunctionState>,

    // --- Phase 2: aggregate type support ---
    /// Registered struct definitions in `define_struct` call order.
    struct_defs: Vec<StructDef>,
    /// Struct names indexed by `StructId`.
    struct_names: Vec<String>,
    /// Unified list of all type definitions (array and struct typedefs) in
    /// dependency order.  Emitted as-is in `finish()`.
    all_typedefs: Vec<String>,
    /// Set of already-registered array typedef names for deduplication.
    array_typedef_set: BTreeSet<String>,
    /// Set of already-registered field typedefs and helper implementations.
    field_helper_set: BTreeSet<String>,
    /// Rendered `extern RetType name(ArgTypes...);` declarations.
    extern_decls: Vec<String>,
    /// Rendered forward declarations (`RetType name(ArgTypes...);`, no
    /// `extern`) for sibling functions called via [`LirTarget::call`] before
    /// their own `begin_function`/`end_function` has produced a definition —
    /// C requires declaration-before-use, unlike the other backends.
    sibling_decls: Vec<String>,
    /// Signatures of functions defined in this translation unit, keyed by
    /// (applied) name. `call_extern` consults this before emitting an
    /// `extern` declaration: an `extern` that disagrees with a same-TU
    /// definition is a conflicting-types compile error, so a defined name
    /// gets a forward declaration from its real signature instead.
    defined_sigs: BTreeMap<String, (Vec<String>, String)>,
    /// Next StructId to assign.
    next_struct_id: StructId,
    /// Name configuration: prefix and per-name remaps applied to all defined
    /// and called function names.  See [`NameConfig`].
    pub name_config: NameConfig,
    /// Name of the C function to call for `Rng` stmts.
    /// Expected signature: `void rng_fn(void *out, size_t len);`
    /// Default: `"volar_rng"`.
    pub rng_fn: String,
    /// When `false`, the folding pass is disabled and every recorded
    /// definition materializes eagerly (A/B debugging escape hatch; also
    /// settable via `VOLAR_C_NOFOLD=1` in [`CBackend::new`]).
    pub fold_expressions: bool,
}

impl CBackend {
    pub fn new() -> Self {
        CBackend {
            completed_functions: Vec::new(),
            current: None,
            struct_defs: Vec::new(),
            struct_names: Vec::new(),
            all_typedefs: Vec::new(),
            array_typedef_set: BTreeSet::new(),
            field_helper_set: BTreeSet::new(),
            defined_sigs: BTreeMap::new(),
            extern_decls: Vec::new(),
            sibling_decls: Vec::new(),
            next_struct_id: 0,
            name_config: NameConfig::default(),
            rng_fn: "volar_rng".to_string(),
            fold_expressions: std::env::var("VOLAR_C_NOFOLD").is_err(),
        }
    }

    /// Set the name configuration (prefix + per-name remaps).
    pub fn with_name_config(mut self, config: NameConfig) -> Self {
        self.name_config = config;
        self
    }

    /// Convenience: set a prefix applied to all emitted function names.
    pub fn with_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.name_config.prefix = prefix.into();
        self
    }

    /// Set the C function name used for `Rng` stmts.
    pub fn with_rng_fn(mut self, name: impl Into<String>) -> Self {
        self.rng_fn = name.into();
        self
    }

    /// Finalize and return the complete C source.
    ///
    /// Emits (in order):
    /// 1. `#include` headers
    /// 2. Type definitions (array + struct typedefs in dependency order)
    /// 3. Extern declarations
    /// 4. Function definitions
    pub fn finish(self) -> String {
        let mut out = String::new();
        out.push_str("#include <stdint.h>\n");
        out.push_str("#include <stdbool.h>\n");
        out.push_str("#include <stdlib.h>\n\n");

        // All type definitions in dependency order.
        for td in &self.all_typedefs {
            out.push_str(td);
        }
        if !self.all_typedefs.is_empty() {
            out.push('\n');
        }

        // Extern declarations.
        for decl in &self.extern_decls {
            out.push_str(decl);
        }
        if !self.extern_decls.is_empty() {
            out.push('\n');
        }

        // Sibling forward declarations.
        for decl in &self.sibling_decls {
            out.push_str(decl);
        }
        if !self.sibling_decls.is_empty() {
            out.push('\n');
        }

        // Function definitions.
        for func in self.completed_functions {
            out.push_str(&func);
            out.push('\n');
        }
        out
    }

    // ---- Internal helpers ---------------------------------------------------

    fn state(&mut self) -> &mut FunctionState {
        self.current
            .as_mut()
            .expect("CBackend: not inside a function")
    }

    /// Convert a `LirType` to its C type name string.
    fn type_to_c(&self, ty: &LirType) -> String {
        lir_type_to_c_free(ty, &self.struct_names)
    }

    /// Register an array typedef (and any nested array typedefs) in DFS order.
    /// No-ops if already registered.
    ///
    /// Recurses through pointers: a `Ptr(Arr(..))` value (e.g. the Vec
    /// fat-pointer's `data` field, or a `Box<[T; N]>` parameter) renders as
    /// `Arr_T_N*`, which requires the `Arr_T_N` typedef to exist.
    fn register_array_typedef(&mut self, ty: &LirType) {
        match ty {
            LirType::Arr(elem, len) => {
                // Register the element type first (handles nesting).
                let elem_clone = *elem.clone();
                self.register_array_typedef(&elem_clone);
                let name = arr_typedef_name(elem, *len);
                if self.array_typedef_set.insert(name.clone()) {
                    let elem_c = lir_type_to_c_free(&elem_clone, &self.struct_names);
                    let td = format!("typedef struct {{ {elem_c} data[{len}]; }} {name};\n");
                    self.all_typedefs.push(td);
                }
            }
            LirType::Ptr(inner) => self.register_array_typedef(inner),
            LirType::ExtField { .. } => self.register_ext_field(ty),
            LirType::PrimeField { .. } => self.register_prime_field(ty),
            _ => {}
        }
    }

    fn register_ext_field(&mut self, ty: &LirType) {
        let LirType::ExtField {
            wrapped,
            degree,
            irreducible,
        } = ty
        else {
            unreachable!()
        };
        assert_eq!(
            **wrapped,
            LirType::Native(NativeType::Bit),
            "CBackend currently supports extension fields over Bit coefficients only"
        );
        let mut types = TypeTable::new();
        let bit = types.bit();
        types
            .ext_field(bit, *degree, irreducible.clone())
            .unwrap_or_else(|error| {
                panic!("CBackend: invalid extension-field descriptor: {error:?}")
            });

        let type_name = ext_field_c_type_name(ty);
        if !self.field_helper_set.insert(type_name.clone()) {
            return;
        }
        let width = *degree as usize;
        let words = width.div_ceil(64);
        let helper = |op: &str| ext_field_helper_name(ty, op);
        let mut source = format!("typedef struct {{ uint64_t limb[{words}]; }} {type_name};\n");
        for op in ["add", "sub", "xor", "and", "or"] {
            let symbol = match op {
                "add" | "sub" | "xor" => "^",
                "and" => "&",
                "or" => "|",
                _ => unreachable!(),
            };
            let name = helper(op);
            writeln!(
                source,
                "static {type_name} {name}({type_name} a, {type_name} b) {{"
            )
            .unwrap();
            writeln!(source, "  {type_name} out = {{0}};").unwrap();
            writeln!(
                source,
                "  for (size_t i = 0; i < {words}; ++i) out.limb[i] = a.limb[i] {symbol} b.limb[i];"
            )
            .unwrap();
            if width % 64 != 0 {
                writeln!(
                    source,
                    "  out.limb[{}] &= UINT64_C(0x{:x});",
                    words - 1,
                    (1u64 << (width % 64)) - 1
                )
                .unwrap();
            }
            writeln!(source, "  return out;\n}}\n").unwrap();
        }
        let not_name = helper("not");
        writeln!(source, "static {type_name} {not_name}({type_name} a) {{").unwrap();
        writeln!(
            source,
            "  for (size_t i = 0; i < {words}; ++i) a.limb[i] = ~a.limb[i];"
        )
        .unwrap();
        if width % 64 != 0 {
            writeln!(
                source,
                "  a.limb[{}] &= UINT64_C(0x{:x});",
                words - 1,
                (1u64 << (width % 64)) - 1
            )
            .unwrap();
        }
        writeln!(source, "  return a;\n}}\n").unwrap();

        for (op, direction) in [("shl", 1i64), ("lshr", -1i64)] {
            let name = helper(op);
            writeln!(
                source,
                "static {type_name} {name}({type_name} a, uint64_t shift) {{"
            )
            .unwrap();
            writeln!(source, "  {type_name} out = {{0}};").unwrap();
            writeln!(
                source,
                "  if (shift < {width}) for (uint64_t i = 0; i < {width}; ++i) {{"
            )
            .unwrap();
            if direction > 0 {
                writeln!(source, "    if (i + shift < {width} && ((a.limb[i >> 6] >> (i & 63)) & 1)) out.limb[(i + shift) >> 6] |= UINT64_C(1) << ((i + shift) & 63);").unwrap();
            } else {
                writeln!(source, "    if (i >= shift && ((a.limb[i >> 6] >> (i & 63)) & 1)) out.limb[(i - shift) >> 6] |= UINT64_C(1) << ((i - shift) & 63);").unwrap();
            }
            writeln!(source, "  }}\n  return out;\n}}\n").unwrap();
        }

        let eq_name = helper("eq");
        writeln!(
            source,
            "static bool {eq_name}({type_name} a, {type_name} b) {{"
        )
        .unwrap();
        writeln!(source, "  for (size_t i = 0; i < {words}; ++i) if (a.limb[i] != b.limb[i]) return false;\n  return true;\n}}\n").unwrap();

        let mul_name = helper("mul");
        let mut reduction = vec![0u64; words];
        for (index, coefficient) in irreducible.iter().take(width).enumerate() {
            assert!(
                *coefficient <= 1,
                "CBackend: GF(2) coefficient is not a bit"
            );
            if *coefficient != 0 {
                reduction[index / 64] |= 1u64 << (index % 64);
            }
        }
        let reduction_values = reduction
            .iter()
            .map(|word| format!("UINT64_C(0x{word:x})"))
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(
            source,
            "static {type_name} {mul_name}({type_name} a, {type_name} b) {{"
        )
        .unwrap();
        writeln!(source, "  {type_name} out = {{0}};\n  const uint64_t reduction[{words}] = {{{reduction_values}}};").unwrap();
        writeln!(source, "  for (size_t i = 0; i < {width}; ++i) {{").unwrap();
        writeln!(source, "    if ((b.limb[i >> 6] >> (i & 63)) & 1) for (size_t j = 0; j < {words}; ++j) out.limb[j] ^= a.limb[j];").unwrap();
        writeln!(
            source,
            "    uint64_t carry = (a.limb[{}] >> {}) & 1;",
            (width - 1) / 64,
            (width - 1) % 64
        )
        .unwrap();
        writeln!(source, "    uint64_t previous = 0;\n    for (size_t j = 0; j < {words}; ++j) {{ uint64_t next = a.limb[j] >> 63; a.limb[j] = (a.limb[j] << 1) | previous; previous = next; }}").unwrap();
        if width % 64 != 0 {
            writeln!(
                source,
                "    a.limb[{}] &= UINT64_C(0x{:x});",
                words - 1,
                (1u64 << (width % 64)) - 1
            )
            .unwrap();
        }
        writeln!(source, "    if (carry) for (size_t j = 0; j < {words}; ++j) a.limb[j] ^= reduction[j];\n  }}\n  return out;\n}}\n").unwrap();
        self.all_typedefs.push(source);
    }

    fn register_prime_field(&mut self, ty: &LirType) {
        let LirType::PrimeField { k, n } = ty else {
            unreachable!()
        };
        assert!(
            (2..=64).contains(k),
            "CBackend supports prime fields with 2 <= k <= 64"
        );
        assert_eq!(
            n.len(),
            1,
            "CBackend prime-field modulus offset must fit one limb"
        );
        assert!(
            n[0] > 0,
            "CBackend prime-field modulus offset must be positive"
        );
        let type_name = prime_field_c_type(*k);
        let helper_prefix = prime_field_helper_prefix(ty);
        if !self.field_helper_set.insert(helper_prefix.clone()) {
            return;
        }
        let modulus = format!("(((__uint128_t)1 << {k}) - UINT64_C(0x{:x}))", n[0]);
        let mut source = String::new();
        let a = format!("((__uint128_t)a % {modulus})");
        let b = format!("((__uint128_t)b % {modulus})");
        for op in ["add", "sub", "mul"] {
            let name = format!("{helper_prefix}_{op}");
            let expr = match op {
                "add" => format!("(({a} + {b}) % {modulus})"),
                "sub" => format!("(({a} + {modulus} - {b}) % {modulus})"),
                "mul" => format!("(({a} * {b}) % {modulus})"),
                _ => unreachable!(),
            };
            writeln!(source, "static {type_name} {name}({type_name} a, {type_name} b) {{ return ({type_name}){expr}; }}").unwrap();
        }
        self.all_typedefs.push(format!("{source}\n"));
    }

    fn field_binop(&mut self, lhs: CValue, rhs: CValue, op: &str) -> CValue {
        let ty = self.state().type_of(lhs).clone();
        assert_eq!(
            ty,
            *self.state().type_of(rhs),
            "field operation requires matching types"
        );
        self.register_array_typedef(&ty);
        let c_type = self.type_to_c(&ty);
        let name = match &ty {
            LirType::ExtField { .. } => ext_field_helper_name(&ty, op),
            LirType::PrimeField { .. } => format!("{}_{}", prime_field_helper_prefix(&ty), op),
            _ => unreachable!(),
        };
        let expr = Expr::FnPure(name, vec![Expr::Name(lhs.0), Expr::Name(rhs.0)]);
        self.state().record_def(ty, c_type, expr, ValKind::Pure)
    }

    fn binop(&mut self, lhs: CValue, op: &'static str, rhs: CValue) -> CValue {
        let ty = self.state().type_of(lhs).clone();
        let c_type = self.type_to_c(&ty);
        let expr = Expr::Bin(op, Box::new(Expr::Name(lhs.0)), Box::new(Expr::Name(rhs.0)));
        self.state().record_def(ty, c_type, expr, ValKind::Pure)
    }

    fn unop(&mut self, op: &'static str, val: CValue) -> CValue {
        let ty = self.state().type_of(val).clone();
        let c_type = self.type_to_c(&ty);
        let expr = Expr::Un(op, Box::new(Expr::Name(val.0)));
        self.state().record_def(ty, c_type, expr, ValKind::Pure)
    }
}

impl Default for CBackend {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Aggregate pack / unpack helpers
// ============================================================================

impl CBackend {
    /// Recursively unpack an aggregate `CValue` into leaf scalar `CValue`s,
    /// recording one pure field-read definition per scalar. Unused scalars
    /// are dropped by the folding pass, so an unread call result costs
    /// nothing beyond the call itself. (Function parameters are unpacked the
    /// same way; their definitions render at the top of the body.)
    fn unpack_to_scalars(&mut self, agg: CValue, ty: &LirType) -> Vec<CValue> {
        match ty.clone() {
            LirType::Arr(elem, n) => {
                let elem_c = self.type_to_c(&elem);
                let mut result = Vec::new();
                for i in 0..n {
                    let expr = Expr::Index(
                        Box::new(Expr::Field(Box::new(Expr::Name(agg.0)), "data".to_string())),
                        Box::new(Expr::Const(i as i64)),
                    );
                    let child = self.state().record_def(
                        elem.as_ref().clone(),
                        elem_c.clone(),
                        expr,
                        ValKind::Pure,
                    );
                    let mut children = self.unpack_to_scalars(child, &elem);
                    result.append(&mut children);
                }
                result
            }
            LirType::Struct(id) => {
                let field_tys: Vec<LirType> = self.struct_defs[id as usize]
                    .fields
                    .iter()
                    .map(|f| f.ty.clone())
                    .collect();
                let field_names: Vec<String> = self.struct_defs[id as usize]
                    .fields
                    .iter()
                    .map(|f| f.name.clone())
                    .collect();
                let mut result = Vec::new();
                for (fname, fty) in field_names.iter().zip(field_tys.iter()) {
                    let fc = self.type_to_c(fty);
                    let cfname = c_field_name(fname);
                    let expr = Expr::Field(Box::new(Expr::Name(agg.0)), cfname);
                    let child = self
                        .state()
                        .record_def(fty.clone(), fc, expr, ValKind::Pure);
                    let mut children = self.unpack_to_scalars(child, fty);
                    result.append(&mut children);
                }
                result
            }
            // Already a scalar — return as-is.
            _ => vec![agg],
        }
    }

    /// Recursively pack a flat slice of scalars into the given aggregate `LirType`.
    ///
    /// `offset` is advanced by the number of scalars consumed.  Emits
    /// construction instructions to the body.
    fn lir_scalar_count(&self, ty: &LirType) -> usize {
        match ty {
            LirType::Arr(elem, n) => n * self.lir_scalar_count(elem),
            LirType::Struct(id) => self.struct_defs[*id as usize]
                .fields
                .iter()
                .map(|f| self.lir_scalar_count(&f.ty))
                .sum(),
            LirType::Ptr(_) => 1,
            _ => 1,
        }
    }

    /// Recursively pack a flat slice of scalars into a pure compound-literal
    /// [`Expr`] for the given aggregate `LirType`. No definitions are
    /// recorded — the tree folds into whatever statement consumes it.
    ///
    /// `offset` is advanced by the number of scalars consumed.
    fn pack_expr(&self, ty: &LirType, scalars: &[CValue], offset: &mut usize) -> Expr {
        match ty {
            LirType::Arr(elem, n) => {
                let members: Vec<Expr> = (0..*n)
                    .map(|_| self.pack_expr(elem, scalars, offset))
                    .collect();
                let type_name = arr_typedef_name(elem, *n);
                Expr::Compound {
                    ty: type_name,
                    inits: vec![Init::List("data".to_string(), members)],
                }
            }
            LirType::Struct(id) => {
                let field_tys: Vec<LirType> = self.struct_defs[*id as usize]
                    .fields
                    .iter()
                    .map(|f| f.ty.clone())
                    .collect();
                let field_names: Vec<String> = self.struct_defs[*id as usize]
                    .fields
                    .iter()
                    .map(|f| f.name.clone())
                    .collect();
                let struct_name = self.struct_names[*id as usize].clone();
                let mut inits = Vec::new();
                for (fname, fty) in field_names.iter().zip(field_tys.iter()) {
                    let e = self.pack_expr(fty, scalars, offset);
                    inits.push(Init::One(c_field_name(fname), e));
                }
                Expr::Compound {
                    ty: struct_name,
                    inits,
                }
            }
            // Scalar — consume one value from the flat list.
            _ => {
                let val = scalars[*offset];
                *offset += 1;
                Expr::Name(val.0)
            }
        }
    }

    // ---- Folding decision + render --------------------------------------

    /// Render a recorded function body through the use-count-driven folding
    /// pass (see the module docs). Returns the body text.
    fn render_body(&self, state: &FunctionState) -> String {
        let mut plan = FoldPlan::compute(state);
        if !self.fold_expressions {
            plan.disable();
        }
        let mut out = String::new();
        let mut r = FoldRenderer {
            state,
            plan: &plan,
            next_tmp: 0,
            comma_temps: BTreeMap::new(),
        };
        for item in &state.items {
            r.render_item(item, &mut out, self.rng_fn.as_str());
        }
        if std::env::var("VOLAR_C_FOLD_STATS").is_ok() {
            let total_defs = state
                .items
                .iter()
                .filter(|i| matches!(i, Item::Def { .. }))
                .count();
            let dropped = plan.dropped.iter().filter(|d| **d).count();
            let inlined = plan.inline.iter().filter(|d| **d).count();
            eprintln!(
                "[c-fold] {}: {total_defs} defs, {dropped} dropped, {inlined} inlined, body {} bytes",
                state.name,
                out.len()
            );
        }
        out
    }
}

// ============================================================================
// Folding: use counting + drop/inline decisions
// ============================================================================

struct FoldPlan {
    /// Value never rendered (pure / mem-read def with zero surviving uses).
    dropped: Vec<bool>,
    /// Value folded into its unique use site instead of materialized.
    inline: Vec<bool>,
    /// Subset of `inline`: the folded definition has side effects (call), so
    /// when two or more fold into one statement they go through comma temps.
    effect_inline: Vec<bool>,
    /// Item index of each value's definition (`usize::MAX` for non-defs).
    def_item: Vec<usize>,
}

impl FoldPlan {
    fn compute(state: &FunctionState) -> FoldPlan {
        let n = state.next_value as usize;
        let mut uses = vec![0u32; n];
        let mut first_use: Vec<Option<usize>> = vec![None; n];
        let mut multi_use = vec![false; n];
        let mut def_item = vec![usize::MAX; n];

        fn walk_expr(
            e: &Expr,
            uses: &mut Vec<u32>,
            first_use: &mut Vec<Option<usize>>,
            multi_use: &mut Vec<bool>,
            item_idx: usize,
        ) {
            match e {
                Expr::Name(v) => {
                    let v = *v as usize;
                    uses[v] += 1;
                    if first_use[v].is_some() {
                        multi_use[v] = true;
                    } else {
                        first_use[v] = Some(item_idx);
                    }
                }
                Expr::Const(_) => {}
                Expr::Bin(_, l, r) => {
                    walk_expr(l, uses, first_use, multi_use, item_idx);
                    walk_expr(r, uses, first_use, multi_use, item_idx);
                }
                Expr::Un(_, v) | Expr::Cast(_, v) | Expr::Field(v, _) | Expr::Deref(v) => {
                    walk_expr(v, uses, first_use, multi_use, item_idx)
                }
                Expr::Icmp { l, r, .. } => {
                    walk_expr(l, uses, first_use, multi_use, item_idx);
                    walk_expr(r, uses, first_use, multi_use, item_idx);
                }
                Expr::Select(c, t, e) => {
                    walk_expr(c, uses, first_use, multi_use, item_idx);
                    walk_expr(t, uses, first_use, multi_use, item_idx);
                    walk_expr(e, uses, first_use, multi_use, item_idx);
                }
                Expr::Index(p, i) => {
                    walk_expr(p, uses, first_use, multi_use, item_idx);
                    walk_expr(i, uses, first_use, multi_use, item_idx);
                }
                Expr::Compound { inits, .. } => {
                    for init in inits {
                        match init {
                            Init::One(_, e) => walk_expr(e, uses, first_use, multi_use, item_idx),
                            Init::List(_, es) => {
                                for e in es {
                                    walk_expr(e, uses, first_use, multi_use, item_idx)
                                }
                            }
                        }
                    }
                }
                Expr::FnPure(_, args) | Expr::Call(_, args) => {
                    for a in args {
                        walk_expr(a, uses, first_use, multi_use, item_idx)
                    }
                }
            }
        }

        for (idx, item) in state.items.iter().enumerate() {
            let mut count = |e: &Expr| walk_expr(e, &mut uses, &mut first_use, &mut multi_use, idx);
            match item {
                Item::Def { id, expr, .. } => {
                    def_item[*id as usize] = idx;
                    count(expr);
                }
                Item::Effect(e) => count(e),
                Item::Store { place, val } => {
                    count(place);
                    count(val);
                }
                Item::Jump { args, .. } => {
                    for a in args {
                        count(a)
                    }
                }
                Item::Branch {
                    cond,
                    then_args,
                    else_args,
                    ..
                } => {
                    count(cond);
                    for a in then_args.iter().chain(else_args.iter()) {
                        count(a)
                    }
                }
                Item::Switch {
                    index,
                    cases,
                    default_args,
                    ..
                } => {
                    count(index);
                    for (_, _, args) in cases {
                        for a in args {
                            count(a)
                        }
                    }
                    for a in default_args {
                        count(a)
                    }
                }
                Item::Ret(Some(e)) => count(e),
                Item::Ret(None) | Item::Label(_) => {}
            }
        }

        // Pass 2 (reverse): drop pure / mem-read defs with zero surviving
        // uses, cascading through their operands. Operand definitions always
        // precede their users in the stream, so a single reverse pass sees
        // final counts.
        let mut dropped = vec![false; n];
        let mut counts = uses.clone();
        fn subtract_expr(e: &Expr, counts: &mut Vec<u32>) {
            match e {
                Expr::Name(v) => counts[*v as usize] -= 1,
                Expr::Const(_) => {}
                Expr::Bin(_, l, r) => {
                    subtract_expr(l, counts);
                    subtract_expr(r, counts);
                }
                Expr::Un(_, v) | Expr::Cast(_, v) | Expr::Field(v, _) | Expr::Deref(v) => {
                    subtract_expr(v, counts)
                }
                Expr::Icmp { l, r, .. } => {
                    subtract_expr(l, counts);
                    subtract_expr(r, counts);
                }
                Expr::Select(c, t, e) => {
                    subtract_expr(c, counts);
                    subtract_expr(t, counts);
                    subtract_expr(e, counts);
                }
                Expr::Index(p, i) => {
                    subtract_expr(p, counts);
                    subtract_expr(i, counts);
                }
                Expr::Compound { inits, .. } => {
                    for init in inits {
                        match init {
                            Init::One(_, e) => subtract_expr(e, counts),
                            Init::List(_, es) => {
                                for e in es {
                                    subtract_expr(e, counts)
                                }
                            }
                        }
                    }
                }
                Expr::FnPure(_, args) | Expr::Call(_, args) => {
                    for a in args {
                        subtract_expr(a, counts)
                    }
                }
            }
        }

        for item in state.items.iter().rev() {
            if let Item::Def { id, expr, kind, .. } = item {
                let id = *id as usize;
                let must_keep = matches!(kind, ValKind::CallRet | ValKind::RngVal);
                if !must_keep && counts[id] == 0 {
                    dropped[id] = true;
                    subtract_expr(expr, &mut counts);
                }
            }
        }

        // Pass 3: inline decisions. A single-use def folds into its unique
        // use only when every item between the definition and the use is
        // another pure definition — no label, terminator, store, call, RNG,
        // or memory read may intervene (module-doc soundness note).
        let mut inline = vec![false; n];
        let mut effect_inline = vec![false; n];
        for item in state.items.iter() {
            if let Item::Def { id, kind, .. } = item {
                let id = *id as usize;
                if dropped[id] || counts[id] != 1 || multi_use[id] {
                    continue;
                }
                let d = def_item[id];
                let u = match first_use[id] {
                    Some(u) if u > d => u,
                    _ => continue,
                };
                // RNG values are written through `&vN`: the named local must
                // exist and its statement must run — never folded.
                if *kind == ValKind::RngVal {
                    continue;
                }
                let pure_window = state.items[d + 1..u].iter().all(|it| {
                    matches!(
                        it,
                        Item::Def {
                            kind: ValKind::Pure,
                            ..
                        }
                    )
                });
                if !pure_window {
                    continue;
                }
                inline[id] = true;
                if *kind == ValKind::CallRet {
                    effect_inline[id] = true;
                }
            }
        }

        FoldPlan {
            dropped,
            inline,
            effect_inline,
            def_item,
        }
    }

    /// Escape hatch: materialize everything (no drops, no folds).
    fn disable(&mut self) {
        for slot in self.dropped.iter_mut() {
            *slot = false;
        }
        for slot in self.inline.iter_mut() {
            *slot = false;
        }
        for slot in self.effect_inline.iter_mut() {
            *slot = false;
        }
    }
}

// ============================================================================
// Rendering
// ============================================================================

struct FoldRenderer<'p> {
    state: &'p FunctionState,
    plan: &'p FoldPlan,
    /// Fresh temporaries: two-phase jump temps (`_tN`) and comma temps (`_cN`).
    next_tmp: u32,
    /// Comma temps for the statement being rendered:
    /// effecting-inline value ID → temp name.
    comma_temps: BTreeMap<u32, String>,
}

impl<'p> FoldRenderer<'p> {
    // ---- Expression rendering ----

    /// Render an expression in operand position, folding single-use
    /// definitions into the output.
    fn expr(&mut self, e: &Expr) -> String {
        match e {
            Expr::Name(v) => {
                let idx = *v as usize;
                if self.plan.inline[idx] {
                    // Effecting folds route through their comma temp when the
                    // statement armed one; a lone effecting fold renders
                    // directly (a single call in a statement needs no
                    // sequencing).
                    if let Some(tmp) = self.comma_temps.get(v).cloned() {
                        return tmp;
                    }
                    let state = self.state;
                    if let Item::Def { expr, .. } = &state.items[self.plan.def_item[idx]] {
                        return self.expr(expr);
                    }
                    unreachable!("inline def for value {v} not found");
                }
                self.state.value_name[idx].clone()
            }
            Expr::Const(c) => format!("{c}"),
            Expr::Bin(op, l, r) => format!("{} {op} {}", self.paren(l), self.paren(r)),
            Expr::Un(op, v) => format!("{op}{}", self.paren(v)),
            Expr::Cast(ty, v) => format!("({ty}){}", self.paren(v)),
            Expr::Icmp { op, signed, l, r } => match signed {
                None => format!("{} {op} {}", self.paren(l), self.paren(r)),
                Some(sc) => {
                    format!("(({sc}){}) {op} (({sc}){})", self.paren(l), self.paren(r))
                }
            },
            Expr::Select(c, t, e) => {
                format!("{} ? {} : {}", self.paren(c), self.paren(t), self.paren(e))
            }
            Expr::Field(base, field) => format!("{}.{}", self.atom(base), c_field_name(field)),
            Expr::Index(p, i) => format!("{}[{}]", self.atom(p), self.expr(i)),
            Expr::Deref(p) => format!("*{}", self.atom(p)),
            Expr::Compound { ty, inits } => {
                let mut s = format!("({ty}){{ ");
                for init in inits {
                    match init {
                        Init::One(name, e) => {
                            let _ = write!(s, ".{} = {}, ", c_field_name(name), self.expr(e));
                        }
                        Init::List(name, es) => {
                            let _ = write!(s, ".{} = {{ ", c_field_name(name));
                            for e in es {
                                let _ = write!(s, "{}, ", self.expr(e));
                            }
                            s.push_str("}, ");
                        }
                    }
                }
                s.push('}');
                s
            }
            Expr::FnPure(name, args) => format!("{name}({})", self.arg_list(args)),
            Expr::Call(name, args) => format!("{name}({})", self.arg_list(args)),
        }
    }

    fn arg_list(&mut self, args: &[Expr]) -> String {
        args.iter()
            .map(|a| self.expr(a))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Parenthesize unless the expression effectively renders atomically
    /// (its own parens, postfix chains, calls, literals). An inlined
    /// definition counts as whatever kind its definition tree is, so folding
    /// `p + i` into `*(p + i)` keeps its parens.
    fn paren(&mut self, e: &Expr) -> String {
        if self.is_operator_kind(e) {
            format!("({})", self.expr(e))
        } else {
            self.expr(e)
        }
    }

    /// True when the expression renders as an operator expression (needs
    /// parens in operand/postfix positions), recursing through inlined
    /// single-use definitions.
    fn is_operator_kind(&self, e: &Expr) -> bool {
        match e {
            Expr::Name(v) => {
                if self.plan.inline[*v as usize] {
                    let state = self.state;
                    match &state.items[self.plan.def_item[*v as usize]] {
                        Item::Def { expr, .. } => self.is_operator_kind(expr),
                        _ => false,
                    }
                } else {
                    false
                }
            }
            Expr::Bin(_, _, _) | Expr::Un(_, _) | Expr::Icmp { .. } | Expr::Select(_, _, _) => true,
            _ => false,
        }
    }

    /// Operand of a postfix `.` / `[` / prefix `*`: parenthesize folded
    /// operator expressions so the postfix application stays well-formed.
    fn atom(&mut self, e: &Expr) -> String {
        self.paren(e)
    }

    // ---- Statement rendering with comma sequencing ----

    /// Collect effecting-inline value IDs reachable in `e`, as
    /// `(def_item, id)` pairs (program order when sorted).
    fn collect_effecting(&self, e: &Expr, out: &mut Vec<(usize, u32)>) {
        match e {
            Expr::Name(v) => {
                if self.plan.effect_inline[*v as usize] {
                    out.push((self.plan.def_item[*v as usize], *v));
                }
            }
            Expr::Const(_) => {}
            Expr::Bin(_, l, r) => {
                self.collect_effecting(l, out);
                self.collect_effecting(r, out);
            }
            Expr::Un(_, v) | Expr::Cast(_, v) | Expr::Field(v, _) | Expr::Deref(v) => {
                self.collect_effecting(v, out)
            }
            Expr::Icmp { l, r, .. } => {
                self.collect_effecting(l, out);
                self.collect_effecting(r, out);
            }
            Expr::Select(c, t, e) => {
                self.collect_effecting(c, out);
                self.collect_effecting(t, out);
                self.collect_effecting(e, out);
            }
            Expr::Index(p, i) => {
                self.collect_effecting(p, out);
                self.collect_effecting(i, out);
            }
            Expr::Compound { inits, .. } => {
                for init in inits {
                    match init {
                        Init::One(_, e) => self.collect_effecting(e, out),
                        Init::List(_, es) => {
                            for e in es {
                                self.collect_effecting(e, out)
                            }
                        }
                    }
                }
            }
            Expr::FnPure(_, args) | Expr::Call(_, args) => {
                for a in args {
                    self.collect_effecting(a, out)
                }
            }
        }
    }

    /// Render the expression parts of one statement. Returns
    /// `(comma_prefix_items, rendered_parts)`: when two or more effecting
    /// (call-result) folds are reachable, they are sequenced in program
    /// order with the comma operator through fresh pre-declared temporaries
    /// whose declaration lines are pushed to `out` first, and the caller
    /// wraps the parts with [`Self::wrap`].
    fn stmt_exprs(&mut self, parts: &[&Expr], out: &mut String) -> (Vec<String>, Vec<String>) {
        let mut ids: Vec<(usize, u32)> = Vec::new();
        for p in parts {
            self.collect_effecting(p, &mut ids);
        }
        if ids.len() < 2 {
            self.comma_temps.clear();
            let rendered = parts.iter().map(|p| self.expr(p)).collect();
            return (Vec::new(), rendered);
        }

        // Sequence by definition (program) order: each temp's right-hand side
        // only references values defined earlier (SSA), so earlier temps are
        // already assigned when a later one's initializer references them.
        ids.sort();
        ids.dedup();
        let mut decls = String::new();
        let state = self.state;
        for (_, v) in &ids {
            let tmp = format!("_c{}", self.next_tmp);
            self.next_tmp += 1;
            let _ = writeln!(decls, "  {} {tmp};", state.value_ctype[*v as usize]);
            self.comma_temps.insert(*v, tmp.clone());
        }
        let mut prefix = Vec::new();
        for (_, v) in &ids {
            let tmp = self.comma_temps[v].clone();
            let rhs = match &state.items[self.plan.def_item[*v as usize]] {
                Item::Def { expr, .. } => self.expr(expr),
                _ => unreachable!("effect_inline value has a Def item"),
            };
            prefix.push(format!("{tmp} = {rhs}"));
        }
        let rendered = parts.iter().map(|p| self.expr(p)).collect();
        self.comma_temps.clear();
        out.push_str(&decls);
        (prefix, rendered)
    }

    /// Fold comma prefix items and the statement core into one expression.
    fn wrap(&self, prefix: Vec<String>, core: String) -> String {
        if prefix.is_empty() {
            core
        } else {
            let mut items = prefix;
            items.push(core);
            format!("({})", items.join(", "))
        }
    }

    // ---- Items ----

    fn render_item(&mut self, item: &Item, out: &mut String, rng_fn: &str) {
        match item {
            Item::Label(b) => {
                let _ = writeln!(out, "block{b}:;");
            }
            Item::Def {
                id,
                c_type,
                expr,
                kind,
            } => {
                let idx = *id as usize;
                if self.plan.dropped[idx] || self.plan.inline[idx] {
                    return;
                }
                if *kind == ValKind::RngVal {
                    let name = &self.state.value_name[idx];
                    let _ = writeln!(out, "  {c_type} {name};");
                    let _ = writeln!(out, "  {rng_fn}(&{name}, sizeof({c_type}));");
                    return;
                }
                let (prefix, r) = self.stmt_exprs(&[expr], out);
                let core = self.wrap(prefix, r.into_iter().next().unwrap());
                let _ = writeln!(out, "  {c_type} {} = {core};", self.state.value_name[idx]);
            }
            Item::Effect(e) => {
                let (prefix, r) = self.stmt_exprs(&[e], out);
                let core = self.wrap(prefix, r.into_iter().next().unwrap());
                let _ = writeln!(out, "  {core};");
            }
            Item::Store { place, val } => {
                let (prefix, r) = self.stmt_exprs(&[place, val], out);
                let core = self.wrap(prefix, format!("{} = {}", r[0], r[1]));
                let _ = writeln!(out, "  {core};");
            }
            Item::Jump { target, args } => {
                self.render_edge(*target, args, out, None);
            }
            Item::Branch {
                cond,
                then_b,
                then_args,
                else_b,
                else_args,
            } => {
                let (prefix, r) = self.stmt_exprs(&[cond], out);
                let cond_s = self.wrap(prefix, r.into_iter().next().unwrap());
                let _ = writeln!(out, "  if ({cond_s}) {{");
                self.render_edge(*then_b, then_args, out, Some("    "));
                let _ = writeln!(out, "  }} else {{");
                self.render_edge(*else_b, else_args, out, Some("    "));
                let _ = writeln!(out, "  }}");
            }
            Item::Switch {
                index,
                cases,
                default_b,
                default_args,
            } => {
                let (prefix, r) = self.stmt_exprs(&[index], out);
                let index_s = self.wrap(prefix, r.into_iter().next().unwrap());
                let _ = writeln!(out, "  switch ({index_s}) {{");
                for (key, block, args) in cases {
                    let _ = writeln!(out, "  case {key}: {{");
                    self.render_edge(*block, args, out, None);
                    let _ = writeln!(out, "  }}");
                }
                let _ = writeln!(out, "  default: {{");
                self.render_edge(*default_b, default_args, out, None);
                let _ = writeln!(out, "  }}");
                let _ = writeln!(out, "  }}");
            }
            Item::Ret(Some(e)) => {
                let (prefix, r) = self.stmt_exprs(&[e], out);
                let core = self.wrap(prefix, r.into_iter().next().unwrap());
                let _ = writeln!(out, "  return {core};");
            }
            Item::Ret(None) => {
                let _ = writeln!(out, "  return;");
            }
        }
    }

    /// Render one control-flow edge: argument expressions assigned to the
    /// target block's `blockM_pK` variables, then `goto`. Direct assignment
    /// unless a rendered argument mentions a target parameter (the
    /// parallel-assignment hazard) — only then two-phase `_tN` temps.
    fn render_edge(&mut self, target: u32, args: &[Expr], out: &mut String, indent: Option<&str>) {
        let ind = indent.unwrap_or("  ");
        let count = self.state.blocks[target as usize].param_count as usize;
        assert_eq!(args.len(), count, "jump arg count mismatch");
        let mut rendered = Vec::with_capacity(args.len());
        for a in args {
            let (prefix, r) = self.stmt_exprs(&[a], out);
            let core = self.wrap(prefix, r.into_iter().next().unwrap());
            rendered.push(core);
        }
        let mut hazard = false;
        for (i, block) in self.state.value_block_param.iter().enumerate() {
            if *block == Some(target)
                && rendered
                    .iter()
                    .any(|s| contains_ident(s, &self.state.value_name[i].clone()))
            {
                hazard = true;
                break;
            }
        }
        if !hazard {
            for (i, s) in rendered.iter().enumerate() {
                let _ = writeln!(out, "{ind}block{target}_p{i} = {s};");
            }
        } else {
            let base = self.next_tmp;
            self.next_tmp += count as u32;
            for (i, s) in rendered.iter().enumerate() {
                let c_type = &self.state.value_ctype[args[i].name_id()];
                let _ = writeln!(out, "{ind}{c_type} _t{} = {s};", base + i as u32);
            }
            for i in 0..count {
                let _ = writeln!(out, "{ind}block{target}_p{i} = _t{};", base + i as u32);
            }
        }
        let _ = writeln!(out, "{ind}goto block{target};");
    }
}

impl Expr {
    /// The value ID of this expression's first `Name` leaf. Edge arguments
    /// are value references; folded trees still carry the leaf whose C type
    /// the two-phase temp needs.
    fn name_id(&self) -> usize {
        fn first_name(e: &Expr) -> Option<u32> {
            match e {
                Expr::Name(v) => Some(*v),
                Expr::Const(_) => None,
                Expr::Bin(_, l, r) => first_name(l).or_else(|| first_name(r)),
                Expr::Un(_, v) | Expr::Cast(_, v) | Expr::Field(v, _) | Expr::Deref(v) => {
                    first_name(v)
                }
                Expr::Icmp { l, r, .. } => first_name(l).or_else(|| first_name(r)),
                Expr::Select(c, t, e) => first_name(c)
                    .or_else(|| first_name(t))
                    .or_else(|| first_name(e)),
                Expr::Index(p, i) => first_name(p).or_else(|| first_name(i)),
                Expr::Compound { inits, .. } => inits.iter().find_map(|i| match i {
                    Init::One(_, e) => first_name(e),
                    Init::List(_, es) => es.iter().find_map(first_name),
                }),
                Expr::FnPure(_, args) | Expr::Call(_, args) => args.iter().find_map(first_name),
            }
        }
        first_name(self).map(|v| v as usize).unwrap_or(0)
    }
}

// ============================================================================
// LirTarget impl
// ============================================================================

impl LirTarget for CBackend {
    type Value = CValue;
    type Block = CBlock;

    // ---- Type registration --------------------------------------------------

    fn define_struct(&mut self, def: StructDef) -> StructId {
        let id = self.next_struct_id;
        self.next_struct_id += 1;

        // Register array typedefs for all field types (pre-pass before rendering).
        let field_tys: Vec<LirType> = def.fields.iter().map(|f| f.ty.clone()).collect();
        for ty in &field_tys {
            self.register_array_typedef(ty);
        }

        // Render the typedef with C-safe field names.
        let mut s = "typedef struct {\n".to_string();
        for field in &def.fields {
            let c_type = self.type_to_c(&field.ty);
            let fname = c_field_name(&field.name);
            writeln!(s, "  {c_type} {fname};").unwrap();
        }
        writeln!(s, "}} {};", def.name).unwrap();

        self.struct_names.push(def.name.clone());
        self.all_typedefs.push(s);
        self.struct_defs.push(def);
        id
    }

    // ---- Value type query ---------------------------------------------------

    fn value_scalar_type(&self, val: &CValue) -> LirType {
        self.current
            .as_ref()
            .expect("value_scalar_type: not inside a function")
            .value_type[val.0 as usize]
            .clone()
    }

    // ---- Function management ------------------------------------------------

    /// Begin a new C function.
    ///
    /// `params` may contain aggregate types (`Arr`/`Struct`).  Each aggregate
    /// parameter becomes one C function parameter (for ABI compatibility) and
    /// is then immediately unpacked into scalar definitions (rendered at the
    /// top of the body; unread scalar params are folded away).
    /// Returns one `Vec<CValue>` per parameter containing the flat scalars.
    fn begin_function(
        &mut self,
        name: &str,
        params: &[LirType],
        ret: Option<LirType>,
    ) -> (CBlock, Vec<Vec<CValue>>) {
        assert!(
            self.current.is_none(),
            "begin_function called while inside a function"
        );

        // Register array typedefs for param and return types.
        for ty in params {
            self.register_array_typedef(ty);
        }
        if let Some(ref ty) = ret {
            self.register_array_typedef(ty);
        }

        let param_ctypes: Vec<String> = params.iter().map(|ty| self.type_to_c(ty)).collect();
        let applied_name = self.name_config.apply(name);
        let ret_c = ret
            .as_ref()
            .map(|ty| self.type_to_c(ty))
            .unwrap_or_else(|| "void".to_string());
        self.defined_sigs
            .insert(applied_name.clone(), (param_ctypes.clone(), ret_c));

        let mut state = FunctionState {
            name: self.name_config.apply(name),
            ret_ty: ret,
            func_param_count: params.len(),
            preamble: String::new(),
            items: Vec::new(),
            blocks: vec![BlockMeta {
                param_count: 0,
                param_base: 0,
            }],
            value_name: Vec::new(),
            value_ctype: Vec::new(),
            value_type: Vec::new(),
            value_block_param: Vec::new(),
            next_value: 0,
            current_block: None,
        };

        // Allocate one value per parameter for the C function signature.
        // These are the aggregate values as seen by the C ABI.
        let agg_params: Vec<CValue> = params
            .iter()
            .zip(param_ctypes.iter())
            .enumerate()
            .map(|(i, (ty, c_type))| state.alloc_value(ty.clone(), c_type.clone(), format!("v{i}")))
            .collect();

        self.current = Some(state);

        // Unpack each aggregate param to scalars. These become the first
        // recorded definitions — they render at the top of the body, and
        // unread scalar params are folded away by the renderer.
        let param_val_groups: Vec<Vec<CValue>> = agg_params
            .iter()
            .zip(params.iter())
            .map(|(&agg, ty)| self.unpack_to_scalars(agg, ty))
            .collect();

        (CBlock(0), param_val_groups)
    }

    fn end_function(&mut self) {
        let state = self
            .current
            .take()
            .expect("end_function called outside a function");

        let ret_cty = state
            .ret_ty
            .as_ref()
            .map(|ty| self.type_to_c(ty))
            .unwrap_or_else(|| "void".to_string());

        let mut param_list = String::new();
        for i in 0..state.func_param_count {
            if i > 0 {
                param_list.push_str(", ");
            }
            let c_type = &state.value_ctype[i];
            let name = &state.value_name[i];
            param_list.push_str(&format!("{c_type} {name}"));
        }

        let mut func = String::new();
        writeln!(func, "{ret_cty} {}({param_list}) {{", state.name).unwrap();
        func.push_str(&state.preamble);
        let body = self.render_body(&state);
        func.push_str(&body);
        writeln!(func, "}}").unwrap();

        // Reconcile extern declarations: a caller lowered via `call_extern`
        // before this definition existed may have emitted an `extern` whose
        // signature disagrees with the real one (a conflicting-types compile
        // error). Replace it with a matching forward declaration — the
        // definition doubles as the declaration for later callers.
        {
            let name = state.name.clone();
            let proto = format!("{ret_cty} {name}({param_list});\n");
            let wrong_marker = format!(" {name}(");
            self.extern_decls = self
                .extern_decls
                .drain(..)
                .flat_map(|d| {
                    if d.starts_with("extern ") && d.contains(&wrong_marker) && d != proto {
                        Some(proto.clone())
                    } else {
                        Some(d)
                    }
                })
                .collect();
        }

        self.completed_functions.push(func);
    }

    // ---- Block management ---------------------------------------------------

    fn create_block(&mut self) -> CBlock {
        let state = self.state();
        let id = state.blocks.len() as u32;
        state.blocks.push(BlockMeta {
            param_count: 0,
            param_base: state.next_value,
        });
        CBlock(id)
    }

    fn add_block_param(&mut self, block: CBlock, ty: LirType) -> CValue {
        self.register_array_typedef(&ty);
        let c_type = self.type_to_c(&ty);
        let state = self.state();
        let block_id = block.0;
        let param_idx = state.blocks[block_id as usize].param_count;
        state.blocks[block_id as usize].param_count += 1;

        let c_name = format!("block{block_id}_p{param_idx}");
        writeln!(state.preamble, "  {c_type} {c_name};").unwrap();
        let v = state.alloc_value(ty, c_type, c_name);
        state.value_block_param[v.0 as usize] = Some(block.0);
        v
    }

    fn switch_to_block(&mut self, block: CBlock) {
        let state = self.state();
        state.current_block = Some(block.0);
        state.items.push(Item::Label(block.0));
    }

    // ---- Constants ----------------------------------------------------------

    fn iconst(&mut self, ty: LirType, val: i64) -> CValue {
        self.register_array_typedef(&ty);
        let c_type = self.type_to_c(&ty);
        let expr = match &ty {
            LirType::ExtField { .. } => ext_field_constant_expr(&ty, val, &self.struct_names),
            LirType::PrimeField { .. } => Expr::Const(prime_field_const_value(&ty, val)),
            _ => Expr::Const(val),
        };
        self.state().record_def(ty, c_type, expr, ValKind::Pure)
    }

    // ---- Arithmetic ---------------------------------------------------------

    fn add(&mut self, lhs: CValue, rhs: CValue) -> CValue {
        match self.state().type_of(lhs).clone() {
            LirType::ExtField { .. } => self.field_binop(lhs, rhs, "add"),
            LirType::PrimeField { .. } => self.field_binop(lhs, rhs, "add"),
            LirType::Native(NativeType::Bit) => self.binop(lhs, "^", rhs),
            _ => self.binop(lhs, "+", rhs),
        }
    }
    fn sub(&mut self, lhs: CValue, rhs: CValue) -> CValue {
        match self.state().type_of(lhs).clone() {
            LirType::ExtField { .. } => self.field_binop(lhs, rhs, "sub"),
            LirType::PrimeField { .. } => self.field_binop(lhs, rhs, "sub"),
            LirType::Native(NativeType::Bit) => self.binop(lhs, "^", rhs),
            _ => self.binop(lhs, "-", rhs),
        }
    }
    fn mul(&mut self, lhs: CValue, rhs: CValue) -> CValue {
        match self.state().type_of(lhs).clone() {
            LirType::ExtField { .. } => self.field_binop(lhs, rhs, "mul"),
            LirType::PrimeField { .. } => self.field_binop(lhs, rhs, "mul"),
            LirType::Native(NativeType::Bit) => self.binop(lhs, "&", rhs),
            _ => self.binop(lhs, "*", rhs),
        }
    }
    fn udiv(&mut self, lhs: CValue, rhs: CValue) -> CValue {
        assert!(
            !is_field_type(self.state().type_of(lhs)),
            "division is not defined for field values"
        );
        self.binop(lhs, "/", rhs)
    }
    fn sdiv(&mut self, lhs: CValue, rhs: CValue) -> CValue {
        assert!(
            !is_field_type(self.state().type_of(lhs)),
            "division is not defined for field values"
        );
        self.binop(lhs, "/", rhs)
    }

    // ---- Bitwise ------------------------------------------------------------

    fn and(&mut self, lhs: CValue, rhs: CValue) -> CValue {
        match self.state().type_of(lhs).clone() {
            LirType::ExtField { .. } => self.field_binop(lhs, rhs, "and"),
            LirType::PrimeField { .. } => panic!("bitwise AND is not defined for prime fields"),
            _ => self.binop(lhs, "&", rhs),
        }
    }
    fn or(&mut self, lhs: CValue, rhs: CValue) -> CValue {
        match self.state().type_of(lhs).clone() {
            LirType::ExtField { .. } => self.field_binop(lhs, rhs, "or"),
            LirType::PrimeField { .. } => panic!("bitwise OR is not defined for prime fields"),
            _ => self.binop(lhs, "|", rhs),
        }
    }
    fn xor(&mut self, lhs: CValue, rhs: CValue) -> CValue {
        match self.state().type_of(lhs).clone() {
            LirType::ExtField { .. } => self.field_binop(lhs, rhs, "xor"),
            LirType::PrimeField { .. } => panic!("bitwise XOR is not defined for prime fields"),
            _ => self.binop(lhs, "^", rhs),
        }
    }
    fn not(&mut self, val: CValue) -> CValue {
        let ty = self.state().type_of(val).clone();
        if matches!(ty, LirType::ExtField { .. }) {
            self.register_array_typedef(&ty);
            let c_type = self.type_to_c(&ty);
            let expr = Expr::FnPure(ext_field_helper_name(&ty, "not"), vec![Expr::Name(val.0)]);
            return self.state().record_def(ty, c_type, expr, ValKind::Pure);
        }
        assert!(
            !matches!(ty, LirType::PrimeField { .. }),
            "bitwise NOT is not defined for prime fields"
        );
        // For bools, use logical `!`; for integers, use bitwise `~`.
        let op = if ty == LirType::Bool { "!" } else { "~" };
        self.unop(op, val)
    }
    fn shl(&mut self, val: CValue, shift: CValue) -> CValue {
        let ty = self.state().type_of(val).clone();
        if matches!(ty, LirType::ExtField { .. }) {
            self.register_array_typedef(&ty);
            let c_type = self.type_to_c(&ty);
            let expr = Expr::FnPure(
                ext_field_helper_name(&ty, "shl"),
                vec![Expr::Name(val.0), Expr::Name(shift.0)],
            );
            return self.state().record_def(ty, c_type, expr, ValKind::Pure);
        }
        assert!(
            !matches!(ty, LirType::PrimeField { .. }),
            "bitwise shifts are not defined for prime fields"
        );
        self.binop(val, "<<", shift)
    }
    fn lshr(&mut self, val: CValue, shift: CValue) -> CValue {
        let ty = self.state().type_of(val).clone();
        if matches!(ty, LirType::ExtField { .. }) {
            self.register_array_typedef(&ty);
            let c_type = self.type_to_c(&ty);
            let expr = Expr::FnPure(
                ext_field_helper_name(&ty, "lshr"),
                vec![Expr::Name(val.0), Expr::Name(shift.0)],
            );
            return self.state().record_def(ty, c_type, expr, ValKind::Pure);
        }
        assert!(
            !matches!(ty, LirType::PrimeField { .. }),
            "bitwise shifts are not defined for prime fields"
        );
        self.binop(val, ">>", shift)
    }

    fn ashr(&mut self, val: CValue, shift: CValue) -> CValue {
        let ty = self.state().type_of(val).clone();
        assert!(
            !is_field_type(&ty),
            "arithmetic shift is not defined for field values"
        );
        let c_type = self.type_to_c(&ty);
        let signed_ty = signed_variant(&ty);
        let expr = Expr::Bin(
            ">>",
            Box::new(Expr::Cast(
                signed_ty.to_string(),
                Box::new(Expr::Name(val.0)),
            )),
            Box::new(Expr::Name(shift.0)),
        );
        self.state().record_def(ty, c_type, expr, ValKind::Pure)
    }

    // ---- Comparison ---------------------------------------------------------

    fn icmp(&mut self, pred: IcmpPred, lhs: CValue, rhs: CValue) -> CValue {
        let ty = self.state().type_of(lhs).clone();
        if is_field_type(&ty) {
            assert!(
                matches!(pred, IcmpPred::Eq | IcmpPred::Ne),
                "ordered comparisons are not defined for field values"
            );
            let equal = match &ty {
                LirType::ExtField { .. } => {
                    self.register_array_typedef(&ty);
                    let expr = Expr::FnPure(
                        ext_field_helper_name(&ty, "eq"),
                        vec![Expr::Name(lhs.0), Expr::Name(rhs.0)],
                    );
                    self.state()
                        .record_def(LirType::Bool, "bool".to_string(), expr, ValKind::Pure)
                }
                LirType::PrimeField { k, n } => {
                    let _ = (k, n);
                    let expr = Expr::Icmp {
                        op: "==",
                        signed: None,
                        l: Box::new(Expr::Name(lhs.0)),
                        r: Box::new(Expr::Name(rhs.0)),
                    };
                    self.state()
                        .record_def(LirType::Bool, "bool".to_string(), expr, ValKind::Pure)
                }
                _ => unreachable!(),
            };
            return if pred == IcmpPred::Eq {
                equal
            } else {
                self.not(equal)
            };
        }
        let op: &'static str = match pred {
            IcmpPred::Eq => "==",
            IcmpPred::Ne => "!=",
            IcmpPred::Ult | IcmpPred::Slt => "<",
            IcmpPred::Ule | IcmpPred::Sle => "<=",
            IcmpPred::Ugt | IcmpPred::Sgt => ">",
            IcmpPred::Uge | IcmpPred::Sge => ">=",
        };
        let signed: Option<&'static str> = match pred {
            IcmpPred::Slt | IcmpPred::Sle | IcmpPred::Sgt | IcmpPred::Sge => {
                let ty = self.state().type_of(lhs).clone();
                Some(signed_variant(&ty))
            }
            _ => None,
        };
        let expr = Expr::Icmp {
            op,
            signed,
            l: Box::new(Expr::Name(lhs.0)),
            r: Box::new(Expr::Name(rhs.0)),
        };
        self.state()
            .record_def(LirType::Bool, "bool".to_string(), expr, ValKind::Pure)
    }

    // ---- Conversions --------------------------------------------------------

    fn zext(&mut self, val: CValue, dst_ty: LirType) -> CValue {
        assert!(
            !is_field_type(self.state().type_of(val)) && !is_field_type(&dst_ty),
            "integer extension involving field values is unsupported"
        );
        self.register_array_typedef(&dst_ty);
        let c_type = self.type_to_c(&dst_ty);
        let expr = Expr::Cast(c_type.clone(), Box::new(Expr::Name(val.0)));
        self.state().record_def(dst_ty, c_type, expr, ValKind::Pure)
    }

    fn sext(&mut self, val: CValue, dst_ty: LirType) -> CValue {
        assert!(
            !is_field_type(self.state().type_of(val)) && !is_field_type(&dst_ty),
            "integer extension involving field values is unsupported"
        );
        self.register_array_typedef(&dst_ty);
        let src_ty = self.state().type_of(val).clone();
        let signed_src = signed_variant(&src_ty);
        let c_type = self.type_to_c(&dst_ty);
        let expr = Expr::Cast(
            c_type.clone(),
            Box::new(Expr::Cast(
                signed_src.to_string(),
                Box::new(Expr::Name(val.0)),
            )),
        );
        self.state().record_def(dst_ty, c_type, expr, ValKind::Pure)
    }

    fn trunc(&mut self, val: CValue, dst_ty: LirType) -> CValue {
        assert!(
            !is_field_type(self.state().type_of(val)) && !is_field_type(&dst_ty),
            "integer truncation involving field values is unsupported"
        );
        self.register_array_typedef(&dst_ty);
        let c_type = self.type_to_c(&dst_ty);
        let expr = Expr::Cast(c_type.clone(), Box::new(Expr::Name(val.0)));
        self.state().record_def(dst_ty, c_type, expr, ValKind::Pure)
    }

    // ---- Select -------------------------------------------------------------

    fn select(&mut self, cond: CValue, then_val: CValue, else_val: CValue) -> CValue {
        let ty = self.state().type_of(then_val).clone();
        let c_type = self.type_to_c(&ty);
        let expr = Expr::Select(
            Box::new(Expr::Name(cond.0)),
            Box::new(Expr::Name(then_val.0)),
            Box::new(Expr::Name(else_val.0)),
        );
        self.state().record_def(ty, c_type, expr, ValKind::Pure)
    }

    // ---- Extern calls -------------------------------------------------------

    /// Call an external C function.
    ///
    /// `arg_tys` gives the ABI (possibly aggregate) type for each logical arg.
    /// `args` is the flat scalar list.  The backend packs scalars into C
    /// aggregates before the call and unpacks the return value afterwards.
    fn call_extern(
        &mut self,
        name: &str,
        arg_tys: &[LirType],
        args: &[CValue],
        ret_ty: Option<LirType>,
    ) -> Vec<CValue> {
        let name = self.name_config.apply(name);
        let name = name.as_str();
        // Pack flat scalars into C aggregate arguments.
        let expected: Vec<usize> = arg_tys.iter().map(|ty| self.lir_scalar_count(ty)).collect();
        let expected_total: usize = expected.iter().sum();
        if expected_total != args.len() {
            panic!(
                "call_extern '{name}': arg_tys expect {expected_total} scalars ({expected:?} for {arg_tys:?}), flat args provided {}",
                args.len()
            );
        }
        let mut offset = 0usize;
        let packed_args: Vec<Expr> = arg_tys
            .iter()
            .map(|ty| self.pack_expr(ty, args, &mut offset))
            .collect();

        // Build the extern declaration using the aggregate C types.
        // Register typedefs first: an `Arr`-typed argument renders into the
        // declaration, so its typedef must exist.
        for ty in arg_tys {
            self.register_array_typedef(ty);
        }
        if let Some(ty) = &ret_ty {
            self.register_array_typedef(ty);
        }
        let arg_c_tys: Vec<String> = arg_tys.iter().map(|ty| self.type_to_c(ty)).collect();
        let ret_c_ty = ret_ty
            .as_ref()
            .map(|ty| self.type_to_c(ty))
            .unwrap_or_else(|| "void".to_string());

        // If this translation unit also DEFINES a function with this name
        // (possible when a caller falls back to `call_extern` for a function
        // that was planned and emitted as an instance), an `extern` with a
        // differing signature is a conflicting-types compile error. Emit a
        // forward declaration from the definition's real signature instead —
        // the call itself stays as emitted below, so signature agreement is
        // the caller's responsibility (as before).
        if let Some((def_params, def_ret)) = self.defined_sigs.get(name) {
            if def_params != &arg_c_tys || def_ret != &ret_c_ty {
                let params_str = def_params.join(", ");
                let proto = format!("{def_ret} {name}({params_str});\n");
                if !self.sibling_decls.contains(&proto) {
                    self.sibling_decls.push(proto);
                }
            }
            // Signature matches (or a decl already exists): no extern needed.
            // Fall through to the call emission.
        } else {
            let params_str = arg_c_tys.join(", ");
            let extern_decl = format!("extern {ret_c_ty} {name}({params_str});\n");
            if !self.extern_decls.contains(&extern_decl) {
                self.extern_decls.push(extern_decl);
            }
        }

        match ret_ty {
            Some(ret) => {
                // Record the call as a side-effecting definition; its result
                // unpacks into pure field reads that fold into their uses.
                let c_type = self.type_to_c(&ret);
                let expr = Expr::Call(name.to_string(), packed_args);
                let agg_result =
                    self.state()
                        .record_def(ret.clone(), c_type, expr, ValKind::CallRet);
                self.unpack_to_scalars(agg_result, &ret)
            }
            None => {
                let expr = Expr::Call(name.to_string(), packed_args);
                self.state().items.push(Item::Effect(expr));
                vec![]
            }
        }
    }

    // ---- Sibling (intra-module) calls ----------------------------------------

    /// Call a function defined in this same translation unit.
    ///
    /// Since C requires declaration-before-use, this forward-declares a
    /// plain (non-`extern`) prototype from the call-site's `arg_tys`/
    /// `ret_ty` — exactly like [`call_extern`](Self::call_extern), except
    /// rendered into `sibling_decls` instead of `extern_decls` so the
    /// eventual real definition (emitted later via `begin_function`/
    /// `end_function`) isn't mistaken for an external symbol.
    fn call(
        &mut self,
        name: &str,
        arg_tys: &[LirType],
        args: &[CValue],
        ret_ty: Option<LirType>,
    ) -> Vec<CValue> {
        let name = self.name_config.apply(name);
        let name = name.as_str();
        let expected: Vec<usize> = arg_tys.iter().map(|ty| self.lir_scalar_count(ty)).collect();
        let expected_total: usize = expected.iter().sum();
        if expected_total != args.len() {
            panic!(
                "call '{name}': arg_tys expect {expected_total} scalars ({expected:?} for {arg_tys:?}), flat args provided {}",
                args.len()
            );
        }
        let mut offset = 0usize;
        let packed_args: Vec<Expr> = arg_tys
            .iter()
            .map(|ty| self.pack_expr(ty, args, &mut offset))
            .collect();

        // Register typedefs first (same reasoning as `call_extern`).
        for ty in arg_tys {
            self.register_array_typedef(ty);
        }
        if let Some(ty) = &ret_ty {
            self.register_array_typedef(ty);
        }
        let arg_c_tys: Vec<String> = arg_tys.iter().map(|ty| self.type_to_c(ty)).collect();
        let ret_c_ty = ret_ty
            .as_ref()
            .map(|ty| self.type_to_c(ty))
            .unwrap_or_else(|| "void".to_string());
        let params_str = arg_c_tys.join(", ");
        let proto = format!("{ret_c_ty} {name}({params_str});\n");
        if !self.sibling_decls.contains(&proto) {
            self.sibling_decls.push(proto);
        }

        match ret_ty {
            Some(ret) => {
                let c_type = self.type_to_c(&ret);
                let expr = Expr::Call(name.to_string(), packed_args);
                let agg_result =
                    self.state()
                        .record_def(ret.clone(), c_type, expr, ValKind::CallRet);
                self.unpack_to_scalars(agg_result, &ret)
            }
            None => {
                let expr = Expr::Call(name.to_string(), packed_args);
                self.state().items.push(Item::Effect(expr));
                vec![]
            }
        }
    }

    // ---- Terminators --------------------------------------------------------

    fn jump(&mut self, target: CBlock, branch: BranchTarget<CValue>) {
        let args = branch.args.into_iter().map(|v| Expr::Name(v.0)).collect();
        self.state().items.push(Item::Jump {
            target: target.0,
            args,
        });
    }

    fn branch(
        &mut self,
        cond: CValue,
        then_block: CBlock,
        then_branch: BranchTarget<CValue>,
        else_block: CBlock,
        else_branch: BranchTarget<CValue>,
    ) {
        let then_args = then_branch
            .args
            .into_iter()
            .map(|v| Expr::Name(v.0))
            .collect();
        let else_args = else_branch
            .args
            .into_iter()
            .map(|v| Expr::Name(v.0))
            .collect();
        self.state().items.push(Item::Branch {
            cond: Expr::Name(cond.0),
            then_b: then_block.0,
            then_args,
            else_b: else_block.0,
            else_args,
        });
    }

    /// Emit a return.  `vals` is the flat scalar list.
    ///
    /// If the function's declared return type is an aggregate, the scalars
    /// are packed back into the C type as a pure compound-literal expression
    /// that folds directly into the `return`.
    fn ret(&mut self, vals: &[CValue]) {
        let item = if vals.is_empty() {
            Item::Ret(None)
        } else {
            let ret_ty = self.state().ret_ty.clone();
            match ret_ty {
                None => Item::Ret(None),
                Some(ty) if ty.is_scalar() => {
                    // Single scalar return — emit directly.
                    assert_eq!(
                        vals.len(),
                        1,
                        "ret: scalar return type but {} values",
                        vals.len()
                    );
                    Item::Ret(Some(Expr::Name(vals[0].0)))
                }
                Some(ty) => {
                    // Aggregate return — pack scalars back into the C type.
                    let expected = self.lir_scalar_count(&ty);
                    assert_eq!(
                        vals.len(), expected,
                        "ret in '{}': aggregate type {ty:?} needs {expected} scalars, got {}",
                        self.state().name,
                        vals.len()
                    );
                    let mut offset = 0usize;
                    let packed = self.pack_expr(&ty, vals, &mut offset);
                    assert_eq!(offset, vals.len(), "ret: aggregate packing left unused scalars");
                    Item::Ret(Some(packed))
                }
            }
        };
        self.state().items.push(item);
    }

    /// Native C `switch`, one `case` per entry plus a `default`. Case edges
    /// render through the same hazard-checked param assignment as jumps.
    fn switch(
        &mut self,
        index: CValue,
        cases: &[(i64, CBlock, BranchTarget<CValue>)],
        default_block: CBlock,
        default_branch: BranchTarget<CValue>,
    ) {
        let cases: Vec<(i64, u32, Vec<Expr>)> = cases
            .iter()
            .map(|(key, block, branch)| {
                (
                    *key,
                    block.0,
                    branch.args.iter().map(|v| Expr::Name(v.0)).collect(),
                )
            })
            .collect();
        self.state().items.push(Item::Switch {
            index: Expr::Name(index.0),
            cases,
            default_b: default_block.0,
            default_args: default_branch
                .args
                .iter()
                .map(|v| Expr::Name(v.0))
                .collect(),
        });
    }

    fn block_ordinal(&self, block: &CBlock) -> i64 {
        block.0 as i64
    }

    // ---- External access primitives ----------------------------------------

    fn oracle(
        &mut self,
        name: &str,
        arg_tys: &[LirType],
        args: &[CValue],
        ret_tys: &[LirType],
    ) -> Vec<CValue> {
        // Treat oracle as a plain extern call. Single-output for now.
        let ret_ty = ret_tys.first().cloned();
        self.call_extern(&format!("oracle_{name}"), arg_tys, args, ret_ty)
    }

    fn action(
        &mut self,
        name: &str,
        guard: CValue,
        arg_tys: &[LirType],
        args: &[CValue],
        fallbacks: &[CValue],
        ret_tys: &[LirType],
    ) -> Vec<CValue> {
        // Call the action unconditionally, then select between result and fallback.
        let ret_ty = ret_tys.first().cloned();
        let action_result = self.call_extern(&format!("action_{name}"), arg_tys, args, ret_ty);
        // For each result scalar: output = guard ? action_result : fallback
        action_result
            .iter()
            .zip(fallbacks.iter())
            .map(|(r, f)| self.select(guard.clone(), r.clone(), f.clone()))
            .collect()
    }

    fn rng(&mut self, ty: LirType) -> CValue {
        let c_ty = lir_type_to_c_free(&ty, &self.struct_names);
        // Record: `<c_ty> vN; <rng_fn>(&vN, sizeof(<c_ty>));` — the address
        // escapes, so the named local always exists and never folds.
        self.state()
            .record_def(ty, c_ty, Expr::Const(0), ValKind::RngVal)
    }

    fn stack_alloc_ext(&mut self) -> Option<&mut dyn StackAllocExt<Value = CValue>> {
        Some(self)
    }

    fn heap_alloc_ext(&mut self) -> Option<&mut dyn HeapAllocExt<Value = CValue>> {
        Some(self)
    }

    fn abi(&self) -> LirAbi {
        LirAbi::C_NATIVE
    }

    fn ptr_index_load(&mut self, ptr: CValue, idx: CValue, pointee_ty: &LirType) -> Vec<CValue> {
        // Record a memory-read definition `ptr[idx]`, then unpack the loaded
        // aggregate into pure field reads that fold into their uses.
        let c_type = self.type_to_c(pointee_ty);
        let expr = Expr::Index(Box::new(Expr::Name(ptr.0)), Box::new(Expr::Name(idx.0)));
        let loaded = self
            .state()
            .record_def(pointee_ty.clone(), c_type, expr, ValKind::MemRead);
        self.unpack_to_scalars(loaded, pointee_ty)
    }

    fn ptr_index_store(&mut self, ptr: CValue, idx: CValue, vals: &[CValue], pointee_ty: &LirType) {
        // Pack flat scalars into a compound-literal expression, then record:
        // ptr[idx] = packed;
        let mut offset = 0usize;
        let packed = self.pack_expr(pointee_ty, vals, &mut offset);
        let item = Item::Store {
            place: Expr::Index(Box::new(Expr::Name(ptr.0)), Box::new(Expr::Name(idx.0))),
            val: packed,
        };
        self.state().items.push(item);
    }
}

// ============================================================================
// StackAllocExt impl
// ============================================================================

impl StackAllocExt for CBackend {
    type Value = CValue;

    /// Allocate a stack region for `count` elements of `elem_ty`.
    ///
    /// Emits into the function preamble (so the array has function scope):
    /// ```c
    /// T slot_vN[count];
    /// T* vN = slot_vN;
    /// ```
    /// Returns the pointer value `vN` of type `LirType::Ptr(elem_ty)`.
    fn alloca(&mut self, elem_ty: LirType, count: usize) -> CValue {
        // The slot declaration renders `elem_c slot[count]` — register any
        // array typedefs inside the element type (e.g. a promoted slot of
        // nested-array type).
        self.register_array_typedef(&elem_ty);
        let elem_c = lir_type_to_c_free(&elem_ty, &self.struct_names);
        let ptr_c = format!("{elem_c}*");
        let ptr_ty = LirType::Ptr(Box::new(elem_ty));

        let state = self
            .current
            .as_mut()
            .expect("CBackend::alloca: not inside a function");
        let id = state.next_value;
        let slot_name = format!("slot_v{id}");
        let ptr_name = format!("v{id}");

        // Emit array declaration and pointer initialisation into the preamble
        // so the array lives for the entire function (C99 VLA-free version).
        writeln!(state.preamble, "  {elem_c} {slot_name}[{count}];").unwrap();
        writeln!(state.preamble, "  {ptr_c} {ptr_name} = {slot_name};").unwrap();

        state.alloc_value(ptr_ty, ptr_c, ptr_name)
    }

    /// Load through a typed pointer.
    ///
    /// Records a memory-read definition: `ty vN = *ptr;`.
    fn ptr_load(&mut self, ptr: CValue, ty: LirType) -> CValue {
        let c_type = self.type_to_c(&ty);
        let expr = Expr::Deref(Box::new(Expr::Name(ptr.0)));
        self.state().record_def(ty, c_type, expr, ValKind::MemRead)
    }

    /// Store `val` through `ptr`.
    ///
    /// Records: `*ptr = val;`
    fn ptr_store(&mut self, ptr: CValue, val: CValue) {
        let item = Item::Store {
            place: Expr::Deref(Box::new(Expr::Name(ptr.0))),
            val: Expr::Name(val.0),
        };
        self.state().items.push(item);
    }

    /// Element-wise pointer offset.
    ///
    /// Records a pure definition: `T* vN = ptr + idx;`.
    fn ptr_offset(&mut self, ptr: CValue, idx: CValue) -> CValue {
        let ty = self.state().type_of(ptr).clone();
        let c_type = self.type_to_c(&ty);
        let expr = Expr::Bin(
            "+",
            Box::new(Expr::Name(ptr.0)),
            Box::new(Expr::Name(idx.0)),
        );
        self.state().record_def(ty, c_type, expr, ValKind::Pure)
    }
}

// ============================================================================
// HeapAllocExt impl
// ============================================================================

impl HeapAllocExt for CBackend {
    type Value = CValue;

    /// Allocate heap storage for `count` elements of `elem_ty` via `malloc`,
    /// zero-initialized (matching `Box::new(core::array::from_fn(|_| T::default()))`'s
    /// own value-initialized semantics on the Rust side — `calloc` over
    /// `malloc`+manual zeroing since every current caller wants a
    /// zero/default-initialized region up front, not uninitialized memory).
    ///
    /// Emits into the function preamble (so the pointer has function scope,
    /// matching `StackAllocExt::alloca`'s own placement):
    /// ```c
    /// T* vN = (T*)calloc(count, sizeof(T));
    /// ```
    /// Returns the pointer value `vN` of type `LirType::Ptr(elem_ty)`.
    fn heap_alloc(&mut self, elem_ty: LirType, count: usize) -> CValue {
        self.register_array_typedef(&elem_ty);
        let elem_c = lir_type_to_c_free(&elem_ty, &self.struct_names);
        let ptr_c = format!("{elem_c}*");
        let ptr_ty = LirType::Ptr(Box::new(elem_ty));

        let state = self
            .current
            .as_mut()
            .expect("CBackend::heap_alloc: not inside a function");
        let id = state.next_value;
        let ptr_name = format!("v{id}");

        writeln!(
            state.preamble,
            "  {ptr_c} {ptr_name} = ({ptr_c})calloc({count}, sizeof({elem_c}));"
        )
        .unwrap();

        state.alloc_value(ptr_ty, ptr_c, ptr_name)
    }
}

// ============================================================================
// Helpers
// ============================================================================

/// Convert a `LirType` to its C type string, resolving struct names from the registry.
fn lir_type_to_c_free(ty: &LirType, struct_names: &[String]) -> String {
    match ty {
        LirType::Bool => "bool".to_string(),
        LirType::I8 => "int8_t".to_string(),
        LirType::U8 => "uint8_t".to_string(),
        LirType::I16 => "int16_t".to_string(),
        LirType::U16 => "uint16_t".to_string(),
        LirType::I32 => "int32_t".to_string(),
        LirType::U32 => "uint32_t".to_string(),
        LirType::I64 => "int64_t".to_string(),
        LirType::U64 => "uint64_t".to_string(),
        LirType::I128 => "__int128_t".to_string(),
        LirType::U128 => "__uint128_t".to_string(),
        LirType::Arr(elem, len) => arr_typedef_name(elem, *len),
        LirType::Struct(id) => struct_names[*id as usize].clone(),
        // Native field elements are exposed as their closest C integer type.
        LirType::Native(t) => native_type_to_c(*t).to_string(),
        LirType::ExtField { .. } => ext_field_c_type_name(ty),
        LirType::PrimeField { k, .. } => prime_field_c_type(*k).to_string(),
        // Pointer: emit as `inner_type*`.
        LirType::Ptr(inner) => format!("{}*", lir_type_to_c_free(inner, struct_names)),
        _ => panic!(
            "lir_type_to_c_free: unhandled LirType variant — add C type mapping for this variant"
        ),
    }
}

/// Unique suffix for a `LirType`, used in typedef names.
fn lir_type_suffix(ty: &LirType) -> String {
    match ty {
        LirType::Bool => "Bool".to_string(),
        LirType::I8 => "I8".to_string(),
        LirType::U8 => "U8".to_string(),
        LirType::I16 => "I16".to_string(),
        LirType::U16 => "U16".to_string(),
        LirType::I32 => "I32".to_string(),
        LirType::U32 => "U32".to_string(),
        LirType::I64 => "I64".to_string(),
        LirType::U64 => "U64".to_string(),
        LirType::I128 => "I128".to_string(),
        LirType::U128 => "U128".to_string(),
        LirType::Arr(elem, len) => format!("Arr_{}_{}", lir_type_suffix(elem), len),
        LirType::Struct(id) => format!("S{id}"),
        LirType::Native(t) => format!("Native_{t:?}"),
        LirType::ExtField {
            wrapped,
            degree,
            irreducible,
        } => {
            let packed = if **wrapped == LirType::Native(NativeType::Bit) {
                let mut words = vec![0u64; (*degree as usize + 1).div_ceil(64)];
                for (index, coefficient) in irreducible.iter().enumerate() {
                    if *coefficient != 0 && index / 64 < words.len() {
                        words[index / 64] |= 1u64 << (index % 64);
                    }
                }
                words
                    .iter()
                    .map(|word| format!("{word:x}"))
                    .collect::<Vec<_>>()
                    .join("_")
            } else {
                irreducible
                    .iter()
                    .map(|coefficient| format!("{coefficient:x}"))
                    .collect::<Vec<_>>()
                    .join("_")
            };
            format!("Ext_d{degree}_{}_p{packed}", lir_type_suffix(wrapped))
        }
        LirType::PrimeField { k, n } => format!(
            "Prime_k{k}_n{}",
            n.iter()
                .map(|limb| format!("{limb:x}"))
                .collect::<Vec<_>>()
                .join("_")
        ),
        LirType::Ptr(inner) => format!("Ptr_{}", lir_type_suffix(inner)),
        _ => panic!("lir_type_suffix: unhandled LirType variant — add suffix for this variant"),
    }
}

fn ext_field_c_type_name(ty: &LirType) -> String {
    format!("Volar_{}", lir_type_suffix(ty))
}

fn ext_field_helper_name(ty: &LirType, op: &str) -> String {
    format!("volar_{}_{}", op, lir_type_suffix(ty))
}

fn prime_field_helper_prefix(ty: &LirType) -> String {
    format!("volar_{}", lir_type_suffix(ty))
}

fn prime_field_c_type(k: u32) -> &'static str {
    match k {
        1..=8 => "uint8_t",
        9..=16 => "uint16_t",
        17..=32 => "uint32_t",
        33..=64 => "uint64_t",
        _ => panic!("CBackend supports prime-field widths up to 64 bits"),
    }
}

fn is_field_type(ty: &LirType) -> bool {
    matches!(ty, LirType::ExtField { .. } | LirType::PrimeField { .. })
}

fn prime_field_const_value(ty: &LirType, value: i64) -> i64 {
    let LirType::PrimeField { k, n } = ty else {
        unreachable!()
    };
    assert!(
        (2..=64).contains(k),
        "CBackend supports prime-field widths up to 64 bits"
    );
    assert_eq!(
        n.len(),
        1,
        "CBackend prime-field modulus offset must fit one limb"
    );
    let modulus = ((1u128) << k) - u128::from(n[0]);
    (i128::from(value).rem_euclid(modulus as i128) as u64) as i64
}

fn ext_field_constant_expr(ty: &LirType, value: i64, struct_names: &[String]) -> Expr {
    let LirType::ExtField { degree, .. } = ty else {
        unreachable!()
    };
    let words = (*degree as usize).div_ceil(64);
    let mut limbs = Vec::with_capacity(words);
    for index in 0..words {
        let mut limb = if index == 0 {
            value as u64
        } else if value < 0 {
            u64::MAX
        } else {
            0
        };
        if index + 1 == words && *degree % 64 != 0 {
            limb &= (1u64 << (*degree % 64)) - 1;
        }
        limbs.push(Expr::Const(limb as i64));
    }
    Expr::Compound {
        ty: lir_type_to_c_free(ty, struct_names),
        inits: vec![Init::List("limb".to_string(), limbs)],
    }
}

/// C typedef name for `Arr(elem, len)`, e.g. `Arr_U8_16`.
fn arr_typedef_name(elem: &LirType, len: usize) -> String {
    format!("Arr_{}_{}", lir_type_suffix(elem), len)
}

/// Signed C type for arithmetic-shift or signed-comparison casts.
/// Panics on aggregate types (should only be called for scalars).
fn signed_variant(ty: &LirType) -> &'static str {
    match ty {
        LirType::Bool | LirType::I8 | LirType::U8 => "int8_t",
        LirType::I16 | LirType::U16 => "int16_t",
        LirType::I32 | LirType::U32 => "int32_t",
        LirType::I64 | LirType::U64 => "int64_t",
        LirType::I128 | LirType::U128 => "__int128_t",
        LirType::Native(t) => native_type_signed(*t),
        LirType::ExtField { .. } | LirType::PrimeField { .. } => {
            panic!("signed_variant: field values have no signed integer representation")
        }
        LirType::Arr(_, _) | LirType::Struct(_) => panic!("signed_variant: aggregate type"),
        LirType::Ptr(_) => panic!("signed_variant: Ptr has no signed variant"),
        _ => {
            panic!("signed_variant: unhandled LirType variant — add signed C type for this variant")
        }
    }
}

/// Map a [`NativeType`] to its unsigned C integer type string.
fn native_type_to_c(t: volar_ir_common::Type) -> &'static str {
    use volar_ir_common::Type;
    match t {
        Type::Bit => "bool",
        Type::_8 => "uint8_t",
        Type::_16 => "uint16_t",
        Type::_32 => "uint32_t",
        Type::_64 => "uint64_t",
        Type::_128 => "__uint128_t",
        Type::_256 => "uint64_t", // no native 256-bit C integer; use u64 placeholder
        _ => "uint64_t",          // future primitive types: conservative fallback
    }
}

/// Signed C integer for a native type (used in arithmetic-shift casts).
fn native_type_signed(t: volar_ir_common::Type) -> &'static str {
    use volar_ir_common::Type;
    match t {
        Type::Bit => "int8_t",
        Type::_8 => "int8_t",
        Type::_16 => "int16_t",
        Type::_32 => "int32_t",
        Type::_64 => "int64_t",
        Type::_128 => "__int128_t",
        Type::_256 => "int64_t",
        _ => "int64_t",
    }
}
