# Typed IR, storage, and polynomial semantics

> Load when working on IR, lowering, evaluators, store-forward, or fuzzer generators.

## IR Type Taxonomy

When working with `IRType` (in `volar-ir`), use this taxonomy:

| Variant | Kind | Bit-width | FHE CFG support |
|---|---|---|---|
| `Primitive(Bit)` | 1-bit GF(2) | 1 | Full |
| `Vec(N, Bit)` | packed bitvector | N | Full (`[wire; N]`) |
| `Primitive(_8/_16/_32/_64)` | packed bitvector | 8/16/32/64 | Full (`[wire; W]`) |
| `Primitive(_128/_256)` | packed bitvector | 128/256 | LIR: `unimplemented!`; FHE: `[wire; W]` |
| `ExtField { wrapped, degree, irreducible }` | extension of `wrapped` | `degree * width(wrapped)` | Boolar: schoolbook product |
| `PrimeField { k, n }` | prime field `p = 2^k - n` | `k` | Boolar: Solinas on `k` digits |

`aes8()` is `ExtField(Bit, 8, x^8 + x^4 + x^3 + x + 1)`. `galois64()` is `ExtField(Bit, 64, x^64 + x^4 + x^3 + x + 1)`. An element is the LSB-first concatenation of `degree` coefficients of `wrapped`, the same layout as `Vec(degree, wrapped)`. `TypeTable::ext_field` accepts the polynomial only when it is a monic irreducible over `Bit` or another `ExtField`.

`z3()` is `PrimeField { k: 2, n: [1] }` (`p = 3`). `TypeTable::prime_field` accepts `k` in `2..=256` only when `n > 0`, `n < 2^(k-1)`, and `2^k - n` is prime. An element is an integer in `0..p`, stored in `k` bits, LSB first. GF(2) stays `Bit`. A prime field is not a coefficient field of `ExtField`.

- `ir_type_bit_width(ty_id, types)` computes the wire count for any supported type.
- `FheScheme::wire_type_for_ir` / `public_type_for_ir` convert an `IRTypeId` to the appropriate compiler `IrType` for generated code.
- `_128`/`_256` in LIR, and a prime field passed to the integer LIR path, panic with an explicit message — do not silently emit wrong code. Prime fields lower through `lower_to_native`.

## `Poly` Statement Semantics

`IRStmt::Poly { ty, coeffs, constant }` is a sum of monomials. The `u8` coefficient is a repetition count, not a field element. A non-trivial scalar is a `Const` factor inside the monomial. The product inside one monomial is type-directed and recurses:

- **`Bit`**: AND. The sum is XOR, and `coeff & 1` cancels duplicates. Repeated factors collapse (`a * a = a`). The empty product is 1.
- **Integer primitive**: the same product mapped across lanes. A 1-bit factor spreads to every lane. The sum is XOR.
- **`Vec(n, E)`**: for each lane, recurse at `E`. A `Vec(n, E)` factor contributes that lane; an `E` or `Bit` factor spreads. A length mismatch fails closed.
- **`ExtField`**: schoolbook polynomial multiplication modulo `irreducible`. The sum is XOR. Coefficient products recurse at `wrapped`. A `Bit` factor is a 0/1 selector. A `wrapped` factor embeds as the degree-0 coefficient. Repeated field factors stay, so `a * a` is the square. The empty product is the field one (only bit 0 set).
- **`PrimeField`**: the sum is addition modulo `p` and the product is multiplication modulo `p`. The stored repetition is `coeff mod p` when that residue fits in `u8` (always for `p < 256`). Parity cancellation does not run. `a * a` is the square. The empty product is 1. A `Tuple` of prime fields is the concatenation of those parts.

`Block` and `Func` as a `Poly` output fail closed. Folds that assume `a * a = a` or that all-ones is the multiplicative identity run only when `mul_is_idempotent` is true. Pack and unpack stay `Merge` and `Shuffle`.

`lower_to_native(blocks, types, native)` lowers every value to wires of `native`. `Bit` produces Boolar (`And` / `Xor` / `Zero` / `One`). Any other native field produces Volar IR whose `Const` and `Poly` evaluate as that field's addition and multiplication. A value of the native field is one wire. `Vec`, `Tuple`, and `ExtField` unroll. A foreign prime is Solinas reduction on `k` boolean digits (`2^k ≡ n`). A bit embedded in an odd prime uses multiplication for AND and `a + b - 2ab` for XOR.

When constructing `Poly` nodes, always supply the `ty` field explicitly. Do not use `ir_stmt_output_ty`'s old fallback (it now returns `Some(*ty)` for `Poly`).

## `IrLoweringConfig`

`volar_ir_config::IrLoweringConfig` configures target-specific parameters for `lower_ir`:

```rust
pub struct IrLoweringConfig {
    pub word_bits: usize,       // native word size (default 64)
    pub pointer_bits: usize,    // pointer size (default 64)
    pub aggregate_byval_limit: usize, // max struct size for by-value ABI (default 128)
    pub native_aggregates: bool,      // use struct-typed LIR values (default false)
}
```

`lower_ir` uses `IrLoweringConfig::default()` for backward compatibility. Pass a custom config via `lower_ir_with_handler` when targeting a different ABI.

## Storage Semantics (Type-Discriminated Slots)

Storage in Volar IR is keyed by `(StorageId, TypeId, address)`. Each such triple is an **independent slot**: writing `_8` to `(S1, addr=0)` does not affect a read of `Bit` from `(S1, addr=0)`, because different `TypeId`s are distinct namespaces within the same `StorageId`.

This design enables:
- **Efficient stack lowering**: a single `StorageId` can represent a stack frame with typed fields at distinct type-slots, without requiring separate `StorageId`s for each field.
- **Storage remapping**: optimization passes (e.g. store-to-load forwarding) can safely forward within a `(StorageId, TypeId)` pair without cross-type interference.

### Storage-access sidecars

A `StorageId` has no access declaration in the persisted IR, text format, or
rkyv layout. Compatibility-sensitive producers instead carry a `StorageTable`
sidecar. An absent entry is conservatively `ReadWrite`; only an explicit
`ReadOnly` entry proves that the producing program has no IR-visible write to
that entire storage namespace (across every `TypeId`/`LaneId`). Pre-initialization
supplies an initial storage image, not an access guarantee.

`virtualize_ir` and `virtualize_bir` expose generated facts through
`VirtOutput::storage_access`. `VirtualizeConfig::storage_access` carries caller
facts into IR virtualization. Caller and generated tables merge conservatively:
conflicting declarations become `ReadWrite`, while absent declarations remain
read-write. Bytecode and handler-slot storage is read-only; register/key
storage remains read-write. Consumers carry the sidecar explicitly after their
own transform. `StorageTable::route_for` selects a read-only or read-write
consumer route but says nothing about visibility, authentication, bounds, or
cost. Validate facts with `validate_ir_storage_access` or
`validate_bir_storage_access` before relying on them. The read-only folding
passes replace only reads whose address resolves to a static storage image (or
the normal zero default); symbolic addresses and undeclared storage stay
unchanged.

### Invalidation policy

A `StorageWrite` to `(S, T, addr)` invalidates all cached reads for the same `(S, T)` pair regardless of address (conservative on address aliasing), but does NOT invalidate entries for `(S, T')` where `T' != T`. **Do not change this policy.**

### Evaluator conformance

All evaluators must key their storage maps by `(StorageId, TypeId, addr)` (IR, VAFFLE) or the equivalent `((StorageId, LaneId), addr)` (BIR, where every value — and therefore every cell — is exactly one bit). Writing with one type and reading with a different type at the same `StorageId + address` returns the default zero value, not the previously written data.

### BIR storage lanes (1-bit cells)

Every Boolar storage cell holds exactly **one bit**; the value's type is
disambiguated by the `LaneId`, not by a per-op width. `BIrStmt::StorageRead`
/ `StorageWrite` carry `lane: LaneId`, and the producer
(`lower_ir_to_boolar_with_lane_table`) returns a total `LaneId → IRTypeId`
side table allocated densely by first use over the source type table.
Multi-bit values are expanded one BIR op per bit, appending the bit index as
high-order address bits: element bit `i` of a value at element address `A`
lives in flat cell `base + A + (i << N)`, where `N` is the lane's fixed
element-address bit width (mixed widths within one `(StorageId, LaneId)`
space are rejected fail-closed, as are addresses where
`N + ceil(log2(value_bits)) > 64`). `pre_init` uses one strided
`BIrPreInitSegment { storage, lane, offset, data }` per bit index.

### BIR multi-bit addresses

`BIrStmt::StorageRead` and `StorageWrite` use `addr: Vec<IRVarId>` — each element is a single-bit BIR variable, and the Vec represents an N-bit *element* address giving 2^N distinct elements per `((StorageId, LaneId))` pair; appended bit-index bits select within the element (see above). Bit 0 (index 0) is the least-significant bit.

**IR->BIR lowering** (`lower_ir_to_boolar.rs`): all bits of the IR address variable are passed through to the BIR base address vec — `var_bits[&addr.0].iter().copied().collect()`. No truncation. The value's bit index is appended as constant high-order bits.

**BIR evaluator** (`interpreter/biir.rs`): collapses `Vec<IRVarId>` to `u64` via `bits_to_u64` (imported from `interpreter::ir`), then keys the `BIrStorageMap` by `((StorageId, LaneId), u64)`. This keeps the evaluator simple and supports up to 64-bit flat addresses (element address + appended index bits must fit).

**Store-forward optimizer** (`store_forward.rs`): `BiirStoreCache` is keyed by `(StorageId, LaneId, Vec<IRVarId>)`. `Vec<IRVarId>` implements `Ord` lexicographically, so it works as a `BTreeMap` key. Cross-block translation applies the pred->target arg map to **each element** of the addr Vec; if any bit fails to translate, the entire cache entry is dropped.

**FHE weaver** (`fhe.rs`): `emit_read` and `emit_write` take `addr_wires: &[&str]` (one wire name per address bit). `mux_tree_read` uses `addr_wires[level]` at each recursion level (not the same wire at every level). `emit_write` uses a full binary demux tree for N-bit addresses, mirroring `mux_tree_read`.

**Fuzzer generator** (`generators/biir.rs`): generates 1-bit addresses (`addr: vec![IRVarId(bv)]`) as the minimum, with optional 2-bit addresses for additional coverage.

### Relevant files

- `crates/fuzz/volar-fuzz/src/interpreter/ir.rs` — `StorageMap = BTreeMap<(StorageId, TypeId, u64), Vec<bool>>`
- `crates/fuzz/volar-fuzz/src/interpreter/vaffle.rs` — reuses `StorageMap` from `ir.rs`
- `crates/fuzz/volar-fuzz/src/interpreter/biir.rs` — `BIrStorageMap = BTreeMap<(StorageId, u64), bool>`; uses `bits_to_u64` to collapse multi-bit addr
- `crates/ir/volar-ir-opt/src/store_forward.rs` — cache types `IrStoreCache`, `BiirStoreCache` (keyed by `Vec<IRVarId>`), `VaffleCache`
- `crates/compiler/volar-weaver/src/fhe.rs` — `emit_read`/`emit_write` with `addr_wires: &[&str]`; `mux_tree_read` with per-level addr bits
