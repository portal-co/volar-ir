# Noir as a Compilation Target

> Research note for planning a Noir backend emitter that writes reusable
> circuit source from Volar IR / Boolar IR. Sourced from official Noir docs
> (noir-lang.org) and the Noir compiler as of **2026-09-02**.
>
> Target toolchain: **Noir / Nargo `v1.0.0-beta.26`** (released 2026-07-30).
> Docs pages still show mixed labels (`v1.0.0-beta.21`–`beta.26`). Pin
> `compiler_version` in generated `Nargo.toml`.

Noir is a Rust-inspired DSL that compiles to ACIR (Abstract Circuit Intermediate
Representation) plus optional Brillig (unconstrained bytecode). It is proving-
backend agnostic; Barretenberg (`bb`) is the common default. All values are
ultimately `Field` elements of the backend curve (254-bit on BN254 / Grumpkin).

This is **not** a persistent-state language. Storage, maps, and Merkle trees
are either compile-time/unconstrained collections, third-party libraries, or
Aztec-network contract wrappers — not native Noir.

---

## 1. Language basics

### Types

| Kind | Types | Notes |
|---|---|---|
| Native field | `Field` | Backend prime field. `+ - * /`, `pow_32`, wraparound, no overflow check. Prefer this for gate count. |
| Boolean | `bool` | `true` / `false`. First-class. Used in `if`, `assert`. |
| Unsigned ints | `u8`, `u16`, `u32`, `u64`, `u128` | Range-constrained fields. Overflow is a proof failure. |
| Signed ints | `i8`, `i16`, `i32`, `i64` | No `i128`. Two's complement range. |
| Arrays | `[T; N]` | Fixed length. Homogeneous. Mutable via `let mut`. |
| Vectors | `[T]` literal `@[1, 2]` | Dynamically sized. **Cannot be returned from a circuit / `main`.** |
| Bounded vec | `BoundedVec<T, MaxLen>` | Array-backed, in prelude. Safe constrained growable storage. |
| Tuples | `(Field, bool)` | Fixed-size. |
| Structs | `struct Foo { x: Field }` | Named fields. Module-private by default. |
| Strings | `str<N>` | Compile-time length. Valid as `main` input. |
| Aliases | `type Id = u8;` | Can be generic / numeric (`type Double<let N: u32>: u32 = N * 2`). |

Sources: [Data Types](https://noir-lang.org/docs/language/data_types),
[Fields](https://noir-lang.org/docs/language/data_types/fields),
[Integers](https://noir-lang.org/docs/language/data_types/integers),
[Arrays](https://noir-lang.org/docs/language/data_types/arrays),
[Vectors](https://noir-lang.org/docs/language/data_types/vectors),
[Booleans](https://noir-lang.org/docs/language/data_types/booleans).

Integer literals default to `Field`, except array/loop indices, bitwise and
comparison ops, `%`, and numeric-generic constants (those default to `u32`).
Suffixes: `1u8`, `256_u16`, `-1i8`, `100_Field`.

**Field vs integer (emitter rule):** emit `Field` for add/mul/select IR unless
the IR type is a bounded integer that needs overflow/range semantics. Casting
`Field as u8` is modular (`260 as u8 == 4`). Use
`field.assert_max_bit_size::<N>()` when a bit bound is required.

### Functions

```noir
pub fn add(x: Field, y: Field) -> Field {
    x + y  // last expression is the return; no early `return`
}

fn unused(_x: Field, y: Field) -> Field { y }
```

- All parameters are typed; types are known at compile time.
- Visibility: default crate-private; `pub` / `pub(crate)` to export.
- No early `return`. Last expression is the value. Use `assert` to constrain
  failure paths.
- Lambdas: `|a, b| a + b`.
- `main` is the circuit ABI for `type = "bin"` packages. Parameters must be
  **fixed-size** (`Field`, `[Field; N]`, tuples, `str<N>`). `[Field]` is illegal
  on `main`.
- `main` return, if present, must be `pub`.

Source: [Functions](https://noir-lang.org/docs/language/functions).

### Modules

Rust-2018 style. The compiler does **not** auto-scan files.

```noir
// src/lib.nr  (crate root for a library)
pub mod gates;
pub use gates::and2;

// src/gates.nr  OR  src/gates/mod.nr  (not both)
pub fn and2(a: bool, b: bool) -> bool { a & b }
```

Paths: `crate::`, `super::`, `::` (absolute), or a dependency name.
`use crate::foo::{bar, baz as my_baz};`. Public `use` re-exports.
Prelude (no import needed): `BoundedVec`, `Option`, `Eq`, `Ord`, `From`,
`Into`, `Default`, `derive`, `assert_constant`, `println`, `panic`.

Source: [Modules](https://noir-lang.org/docs/project_structure/modules).

### Constrained vs unconstrained

| | Constrained (`fn`) | Unconstrained (`unconstrained fn`) |
|---|---|---|
| Compiles to | ACIR constraints | Brillig bytecode |
| Loops | Bounds known at compile time (`for i in 0..N`) | `loop` / `while` / `break` / `continue` OK |
| Purpose | What is proven | Witness generation, inversion, I/O |
| Call from constrained | Normal | Must be `unsafe { ... }` + later `assert` |

Calling unconstrained from constrained requires `unsafe { ... }` and a
`// Safety: ...` comment. The compiler checks that every result (including
every array element) is later constrained against an argument or a constant.

```noir
unconstrained fn sqrt_hint(x: Field) -> Field { /* Brillig */ }

fn main(x: Field) -> pub Field {
    // Safety: r*r == x constrains the hint
    let r = unsafe { sqrt_hint(x) };
    assert(r * r == x);
    r
}
```

Source: [Unconstrained Functions](https://noir-lang.org/docs/language/unconstrained).

---

## 2. Boolean circuit support

**Yes — `bool` is first-class in circuits.** Conditionals, `assert`, and
bitwise ops on bools compile to ACIR. AND/XOR/NOT/MUX are all writable.

### Operators on `bool`

| Gate | Noir | Notes |
|---|---|---|
| AND | `a & b` | No `&&`. Short-circuit is not possible in a circuit. |
| OR | `a \| b` | No `\|\|`. |
| XOR | `a != b` (safe) | Ops table lists `^` as integer-only. Prefer `!=` for bools. |
| NOT | `!a` | Documented for integer **or** boolean. |
| MUX | `if c { a } else { b }` | Flattened to `c * a + (1 - c) * b` (field form). |
| EQ | `a == b` | |

There are **no** logical `&&` / `||`. Use bitwise `&` / `|` on bools.

Source: [Logical Operations](https://noir-lang.org/docs/language/ops),
[Thinking in Circuits](https://noir-lang.org/docs/guides/thinking_in_circuits).

`&` / `|` / `^` on integers invoke ACIR black boxes (`AND`, `XOR`). `!` and
range checks similarly. These are more expensive than `Field` `+` / `*`.

### Flattening

Constrained `if` is **not** control flow. Both branches become gates; a
predicate selects. `#[no_predicates]` skips that mux-predication for functions
that cannot fail an assertion — only use on proven-pure helpers.

Constrained `for` unrolls. Iteration count must be statically known.
`break` / `continue` / `while` / `loop` are unconstrained-only.

### Emitter recommendation (Boolar → Noir)

Emit a straight-line `pub fn` over `[bool; N]` (or individual `bool` params if
the arity is small). Lower AND/XOR/NOT/MUX to `&` / `!=` / `!` / `if`. Do not
emit `&&` / `||`. Keep loops only when the bound is a numeric generic or
`global`.

---

## 3. Reusable libraries (Nargo, modules, `dep` packages)

An embedder consumes generated Noir as a **library crate**, then calls `pub`
functions from their own `main` (or another lib).

### Library package layout

```
volar_circuit/
├── Nargo.toml
└── src/
    ├── lib.nr          # crate root (required name under Nargo)
    ├── boolar.nr       # optional submodule
    └── field_ir.nr
```

```toml
# volar_circuit/Nargo.toml
[package]
name = "volar_circuit"
type = "lib"
authors = ["volar-ir"]
compiler_version = ">=1.0.0-beta.26"
```

```noir
// src/lib.nr
pub mod boolar;
pub mod field_ir;

pub use boolar::eval_bool_circuit;
pub use field_ir::eval_field_ir;
```

A package is **one crate**: either `lib` (`src/lib.nr`) or `bin` (`src/main.nr`),
not both. Override the entry with `entry = "..."`.

### Embedder dependency

```toml
# embedder/Nargo.toml
[package]
name = "my_app"
type = "bin"
compiler_version = ">=1.0.0-beta.26"

[dependencies]
volar_circuit = { path = "../volar_circuit" }
# or git:
# volar_circuit = { tag = "v0.1.0", git = "https://github.com/org/volar-circuit" }
# subdirectory:
# blob = { tag = "v1.2.1", git = "https://github.com/AztecProtocol/aztec-packages", directory = "noir-projects/..." }
```

```noir
// embedder/src/main.nr
use volar_circuit::eval_bool_circuit;

fn main(wires: [bool; 8]) -> pub bool {
    eval_bool_circuit(wires)
}
```

- Import path is the **Nargo.toml key**, not the package `name` (they usually
  match). `nargo add --path ../volar_circuit` writes the entry.
- No official package registry. Git + `tag` is the versioning story. A `tag`
  is required for git deps.
- Transitive deps are visible: `use dep::their_dep::item`.
- Workspaces: root `[workspace] members = [...]`; consume sibling libs as
  `{ path = "../to_lib" }` — not as external git deps from inside the workspace.

Sources: [Dependencies](https://noir-lang.org/docs/project_structure/dependencies),
[Crates and Packages](https://noir-lang.org/docs/project_structure/crates_and_packages),
[Workspaces](https://noir-lang.org/docs/project_structure/workspaces).

`#[export]` marks functions for `nargo export` so a host can compile library
functions to ACIR artifacts without a `main`. Useful if the embedder is not
another Noir crate (e.g. a JS/Rust prover host).

---

## 4. Witness / public / private inputs

Two different meanings of `pub`:

| Context | Meaning |
|---|---|
| `pub fn` / `pub struct` / `pub use` | Module visibility (Rust-like). |
| `y: pub Field` on **`main` only** | Prover/verifier visibility. |

- **Private (default):** known only to the prover. Docs also call these
  **witnesses**.
- **Public:** `pub` on a `main` parameter or `main` return. Revealed in the
  proof. Does not change the circuit.
- `pub` **types** can only be declared on `main` parameters (and the return).
  Library functions do not mark witness visibility — the embedder's `main`
  does.

```noir
fn main(x: Field, y: pub Field) -> pub Field {
    x + y  // x private, y and result public
}
```

Both private and public values are supplied by the prover in `Prover.toml`:

```toml
x = "1"
y = "2"
```

`nargo check` generates a skeleton `Prover.toml`. `nargo execute` runs the
circuit, writes `target/<name>.json` (ACIR) and `target/<name>.gz` (witness).
Custom input file: `nargo execute -p OtherProver bar`.

**Data bus** (recursion helper, incompatible with `pub` / `mut`):

```noir
fn main(mut x: u32, y: call_data(0) u32, z: call_data(0) [u32; 4]) -> return_data u32 {
    z[x] + y
}
```

`call_data(id)` groups private inputs into one read-only array; `return_data`
does the same for the return. Same `id` → same array.

Sources: [Data Types — pub](https://noir-lang.org/docs/language/data_types),
[Getting Started](https://noir-lang.org/docs/getting_started_manually),
[Data Bus](https://noir-lang.org/docs/language/data_bus).

---

## 5. Storage

**Noir has no native persistent storage, maps, or Merkle trees.**

| Mechanism | What it is | Use in generated circuits? |
|---|---|---|
| `[T; N]` | Fixed array. The real storage primitive. | Yes. |
| `BoundedVec<T, MaxLen>` | Array + length. Prelude. | Yes, if length is dynamic but bounded. |
| `[T]` / `@[...]` vectors | Runtime-resizable. Not a `main` / circuit return. | Avoid as ABI; OK internally if converted with `as_array::<N>()`. |
| `UHashMap<K,V,H>` | Growable map. **Comptime / unconstrained** (used by `derive`). | No — not a constrained circuit map. |
| `std::merkle` | **Removed** from stdlib (noir-lang/noir#7582, 2025-03). | No. |
| Third-party Merkle | e.g. [zk-kit.noir merkle-trees](https://github.com/privacy-scaling-explorations/zk-kit.noir), listed on awesome-noir | Optional dep if IR needs membership proofs. |
| Aztec.nr `Map` / note trees | Aztec **contract** state (`type = "contract"`). Not language-level. | Out of scope for a generic IR emitter. |

In-circuit "storage" for a Volar emitter is **arrays of `Field`/`bool`**,
optionally wrapped in a struct. Persistent / authenticated storage is a
library or host concern: pass the array (or a Merkle root + path) as
witnesses and constrain the update.

---

## 6. Generated Noir for a boolean circuit

Boolar-style netlist: inputs `w[0..n)`, gates AND/XOR/NOT/MUX, outputs a
prefix or explicit list.

```noir
/// Evaluates a fixed boolean circuit.
/// `w` is the input witness vector; return is the output bits.
pub fn eval_bool_circuit(w: [bool; 4]) -> [bool; 2] {
    let t0: bool = w[0] & w[1];       // AND
    let t1: bool = w[2] != w[3];      // XOR
    let t2: bool = !t0;               // NOT
    let t3: bool = if t1 { t2 } else { w[0] }; // MUX
    [t2, t3]
}
```

Numeric-generic form (one function per arity family):

```noir
pub fn eval_bool<let N: u32, let M: u32>(w: [bool; N]) -> [bool; M] {
    // body generated with compile-time-known N, M
    let mut out: [bool; M] = [false; M];
    // ... assigned temporaries ...
    out
}
```

Do **not** put this on `main` inside the library. The embedder wraps it:

```noir
use volar_circuit::eval_bool_circuit;

fn main(w: [bool; 4]) -> pub [bool; 2] {
    eval_bool_circuit(w)
}
```

If the IR is a **constraint system** (no outputs, only asserts):

```noir
pub fn constrain_bool_circuit(w: [bool; 4]) {
    let t0 = w[0] & w[1];
    assert(t0 == w[2]);
}
```

---

## 7. Generated Noir for field-level IR

Volar IR add / mul / select on `Field` or integers:

```noir
pub fn eval_field_ir(a: Field, b: Field, c: Field, sel: bool) -> Field {
    let s = a + b;                 // add
    let p = s * c;                 // mul
    let y = if sel { p } else { a }; // select / mux
    y
}

pub fn eval_u32_ir(a: u32, b: u32, sel: bool) -> u32 {
    let s = a + b;                 // overflow = proof fail
    if sel { s } else { a }
}

pub fn eval_u32_wrapping(a: u32, b: u32) -> u32 {
    use std::ops::WrappingAdd;
    a.wrapping_add(b)
}
```

Select on `Field` can also be written arithmetically (one constraint, no
branch predication on large bodies):

```noir
// sel is 0 or 1. Equivalent to if sel { p } else { a }.
let y = sel as Field * p + (1 - sel as Field) * a;
```

If the IR already booleanizes `sel`, emit this form. If `sel` is a `bool`
from a comparison, `if` is fine — the compiler flattens it.

Bit decomposition / reassembly (common after booleanization):

```noir
pub fn pack_le_bits<let N: u32>(bits: [bool; N]) -> Field {
    let mut acc: Field = 0;
    let mut pow: Field = 1;
    for i in 0..N {
        acc = acc + (bits[i] as Field) * pow;
        pow = pow * 2;
    }
    acc
}

pub fn unpack_le_bits<let N: u32>(x: Field) -> [bool; N] {
    x.to_le_bits::<N>()
}
```

`to_le_bits` / `to_be_bits` / `assert_max_bit_size` are stdlib `Field` methods.

---

## 8. Constraints vs unconstrained computation

### What is a constraint

- Every constrained expression (`+`, `*`, `==`, `if`, …) becomes ACIR.
- `assert(pred)` / `assert_eq(a, b)` **explicitly** constrain a predicate.
  `assert` only accepts predicate ops (`==`, `<`, …), not `assert(x + y)`.
- `static_assert(pred, "msg")` is compile-time only.
- Integer arithmetic inserts range / overflow constraints automatically.
- `Field` arithmetic does **not** range-check; it wraps at the modulus.

### When to emit unconstrained (Brillig)

Use a hint + inverse check when the forward circuit is expensive and the
inverse is cheap (sqrt, division, factorization, bit-decomposition via
rebuild). The emitter should only do this when it can generate the
reconstructing `assert`.

```noir
unconstrained fn div_hint(n: Field, d: Field) -> Field { n / d }

fn checked_div(n: Field, d: Field) -> Field {
    // Safety: q*d == n
    let q = unsafe { div_hint(n, d) };
    assert(q * d == n);
    q
}
```

`std::runtime::is_unconstrained()` (docs: `is_unconstrained()`) lets one
function body branch on context.

Security passes (on by default):

- `--skip-underconstrained-check` — independent subgraphs.
- `--skip-brillig-constraints-check` — Brillig results not covered by later
  constraints.
- `--enable-brillig-constraints-check-lookback` — fewer false positives.

Oracles (`#[oracle(name)] unconstrained fn ...`) are host RPC. Not for IR
lowering unless the host provides a resolver.

---

## 9. Current syntax (2025–2026)

Docs and compiler around `v1.0.0-beta.21`–`beta.26`.

### Structs + visibility

```noir
pub struct Animal {
    hands: Field,            // module-private
    pub(crate) legs: Field,  // crate-visible
    pub eyes: u8,            // public
}

let dog = Animal { eyes: 2, hands: 0, legs };
let Animal { hands, legs: feet, eyes } = dog;
```

Source: [Structs](https://noir-lang.org/docs/language/data_types/structs).

### Impls + methods

```noir
impl Animal {
    fn new(eyes: u8) -> Self { Animal { hands: 0, legs: 4, eyes } }
    fn sum(self) -> Field { self.hands + self.legs }
}

impl Animal {
    // inherent methods; overlapping names on overlapping impls error
}

let s = Animal::new(2);
assert(s.sum() == 4);
assert(Animal::sum(s) == 4); // UFCS
```

Generic specialization is allowed if impls do not overlap:

```noir
struct Foo<T> {}
impl Foo<u32> { fn tag(self) -> Field { 1 } }
impl Foo<u64> { fn tag(self) -> Field { 2 } }
// impl<T> Foo<T> { fn tag(...) }  // error if it overlaps
```

Source: [Functions — Methods](https://noir-lang.org/docs/language/functions).

### Traits

```noir
trait Area { fn area(self) -> Field; }

fn log_area<T>(shape: T) where T: Area {
    println(shape.area());
}

impl Area for Rect {
    fn area(self) -> Field { self.w * self.h }
}
```

Orphan/coherence rules match Rust: you cannot `impl StdTrait for ForeignType`
in your crate; use a newtype. `#[derive(Default, Eq, Ord)]` is comptime.

Source: [Traits](https://noir-lang.org/docs/language/traits).

### Generics

```noir
fn id<T>(x: T) -> T { x }

struct BigInt<let N: u32> { limbs: [u32; N] }

impl<let N: u32> BigInt<N> {
    fn first(self) -> u32 { self.limbs[0] }
}

fn foo<let A: u8, let B: u32, let C: i64>() {}
fn main() {
    foo::<0u8, 2, 3i64>(); // non-u32 numeric generics need a suffix
}

fn first_eq<T, let N: u32>(a: [T; N], b: [T; N]) -> bool where T: Eq {
    a[0] == b[0]
}

let array = vector.as_array::<2>(); // turbofish
```

Arithmetic generics: `N + M`, `N * M`, `N - 1` in type position. Underflow
fails type-check. Distributivity is **not** applied (`T*(N+M)` ≠ `T*N+T*M`
to the compiler).

Source: [Generics](https://noir-lang.org/docs/language/generics).

### Comptime / macros

```noir
comptime fn quote_one() -> Quoted { quote { 1 } }

comptime global N: u32 = 4;
comptime mut global COUNTER: u32 = 0;

fn main() {
    let x = comptime { 1 + 2 }; // lowered to literal 3
    comptime for i in 0..N { /* unrolled at compile time */ }
}

// unquote with $var inside quote { }; insert with foo!()
let y = quote_one!(); // => 1
```

Also: `comptime struct` / `comptime type` / `comptime { }` / `comptime let`.
`quote [ } ]` when braces would mismatch. `$` splices a **variable**, not an
expression. Attributes on items run top-down, submodule-first.

`#[derive(...)]`, `#[varargs]`, `#[use_callers_scope]` for attribute macros.
`UHashMap` is the comptime map used by derive handlers.

Source: [Comptime](https://noir-lang.org/docs/language/comptime).

### Other 2025–2026 syntax to know

- `global N: u32 = 2;` — not `const`. Used for array lengths.
- Attributes: `#[test]`, `#[fuzz]`, `#[fold]`, `#[no_predicates]`,
  `#[inline_always]` / `#[inline_never]` (unconstrained only; constrained
  calls are always inlined), `#[export]`, `#[field(bn254)]`, `#[oracle]`,
  `#[allow(dead_code)]`, inner `#![allow(...)]`.
- `enum` and `match` are **reserved keywords**. LSP in beta.26 mentions
  enum symbols — treat enums as unstable; do not emit them yet.
- Primitive type names (`bool`, `u32`, `Field`, `str`) are **not** keywords
  (noir#8470, 2025-05). Still do not reuse them as generated names.

---

## 10. Gotchas for generated code

### Identifiers

From the lexer (`compiler/noirc_frontend/src/lexer/lexer.rs` on `master`):

- Identifiers are **ASCII only**: start with `[A-Za-z_]`, continue
  `[A-Za-z0-9_]`. Non-ASCII is a hard `NonAsciiIdentifier` error.
- No raw identifiers (`r#fn` is not a thing to rely on).

**Reserved keywords** (`token.rs` `Keyword` enum):

```
as assert assert_eq break call_data comptime constrained continue
contract crate dual else enum fn for global if impl in let loop match
mod mut pub return return_data struct super trait type unchecked
unconstrained unsafe use where while
```

`self` is contextual (special in `impl`s only).

**Safe generated names:** `t0`, `w_3`, `eval_circuit`, `_unused`. Avoid
keywords, `main` (in bins), and prelude names you do not intend to shadow.

### File layout / Nargo.toml

```toml
[package]
name = "volar_circuit"          # non-empty CrateName; keep [a-z0-9_]
type = "lib"                    # lib | bin | contract
compiler_version = ">=1.0.0-beta.26"
# entry = "src/lib.nr"          # optional override
# expression_width = 4          # backend width; default 4
# compiler_unstable_features = []

[dependencies]
# name_used_in_use = { path = "..." } | { tag = "...", git = "...", directory = "..." }
```

- Filename: `Nargo.toml` (capital N).
- Lib root **must** be `src/lib.nr`; bin root `src/main.nr` (unless `entry`).
- `mod foo;` looks for `foo.nr` **or** `foo/mod.nr`, never both.
- One crate per package. Use a workspace for lib + example bin.
- `nargo new --lib volar_circuit` scaffolds this.
- `nargo fmt` exists; generated code should be rust-like for it.

### ABI / control-flow

- No `&&` / `||`, no early `return`, no `usize`, no `const` (use `global`).
- `main` args fixed-size; `main` return `pub`.
- Constrained loops need static bounds. Dynamic `N` from a witness cannot
  bound a `for`.
- Dynamic array index is legal but costs RAM vs ROM; indices on arrays of
  references must be constant.
- Index type is `u32`. `Field` index: `x.assert_max_bit_size::<32>(); array[x as u32]`.
- Constrained functions are **always inlined**. Do not rely on `#[inline_never]`
  to keep generated helpers outlined in ACIR. `#[fold]` emits a separately
  verified subcircuit if the backend supports folding.

### Numerics

- `Field` wraps silently. Do not use it for "balance cannot go negative."
- Integer overflow fails the proof (`attempt to add with overflow`), unless
  `wrapping_add` / `wrapping_sub` / `wrapping_mul`.
- Unused overflowing expressions may be DCE'd and never fail.
- Bitwise ops and shifts on integers are **expensive** vs `Field` `+` `*`.
  Shift RHS must be `< bitwidth`.
- `==` / `!=` docs say both sides "must not be constants" — constant folding
  usually handles `x == 0`; prefer `assert_eq` for clarity.

### Visibility / linking

- Items default to **module-private**. Generated entry points and types the
  embedder must name need `pub`.
- Struct fields default private — generated structs used outside the module
  need `pub` fields (or accessors).
- `pub` on a library function is **not** witness-public. Only `main` params.

### Testing vs proving

`#[test]` can call functions with slices; `nargo test` may pass while
`nargo check` rejects the same `main([Field])`. Do not treat tests as ABI
validation.

### Version pin

Docs and nightlies move quickly (beta.26 in July 2026; nightlies through
August 2026). Generated trees should set `compiler_version` and CI should
install that exact `noirup -v v1.0.0-beta.26`.

---

## Emitter checklist

1. Emit a `type = "lib"` package; keep `main` out of the generated crate.
2. Export `pub fn` over `[bool; N]` / `[Field; N]` / structs of those.
3. Boolar: `&`, `!=`, `!`, `if` — never `&&` / `||` / `^` on bools.
4. Field IR: `+`, `*`, `if` or arithmetic mux; integers only when overflow
   matters.
5. No maps, Merkle, or Aztec storage in the generated core. Pass arrays in.
6. Constrained loops only with `let N: u32` / `global` bounds.
7. ASCII snake_case names; keyword denylist; `pub` on exported items and
   struct fields.
8. Pin `compiler_version`. Optional `#[export]` if the host is not Noir.
9. Leave unconstrained hints as an opt-in pass (inverse + `assert`), not the
   default lowering.
10. Embedder adds `{ path = "..." }` and calls the `pub fn` from their `main`,
    marking `pub` on the values they want in the verifier ABI.

## Primary sources

- https://noir-lang.org — product home
- https://noir-lang.org/docs/language/data_types and child pages (fields,
  integers, arrays, vectors, structs, booleans)
- https://noir-lang.org/docs/language/ops
- https://noir-lang.org/docs/language/functions
- https://noir-lang.org/docs/language/unconstrained
- https://noir-lang.org/docs/language/control_flow
- https://noir-lang.org/docs/language/generics
- https://noir-lang.org/docs/language/traits
- https://noir-lang.org/docs/language/comptime
- https://noir-lang.org/docs/language/attributes
- https://noir-lang.org/docs/language/globals
- https://noir-lang.org/docs/language/data_bus
- https://noir-lang.org/docs/language/assert
- https://noir-lang.org/docs/guides/thinking_in_circuits
- https://noir-lang.org/docs/project_structure/modules
- https://noir-lang.org/docs/project_structure/crates_and_packages
- https://noir-lang.org/docs/project_structure/dependencies
- https://noir-lang.org/docs/project_structure/workspaces
- https://noir-lang.org/docs/getting_started_manually
- https://github.com/noir-lang/noir/releases/tag/v1.0.0-beta.26
- https://github.com/noir-lang/noir/blob/master/compiler/noirc_frontend/src/lexer/token.rs (keywords)
- https://github.com/noir-lang/noir/blob/master/compiler/noirc_frontend/src/lexer/lexer.rs (identifier rules)
- https://github.com/noir-lang/noir/pull/7582 (stdlib merkle removal)
- https://github.com/noir-lang/awesome-noir (third-party merkle / collections)
- https://docs.aztec.network/developers/docs/aztec-nr/framework-description/state_variables (Aztec storage, not Noir-the-language)
