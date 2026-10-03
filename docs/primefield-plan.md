# Lowerable prime fields

`Type::Z3` was a unit tag with no modulus. `lower_ir_to_boolar` used to panic in `ir_type_bits` because GF(3) has no GF(2) width, and `lower_lir` panicked in `ir_type_to_lir` for the same reason. Boolar emission is a private `Emitter` of `BIrStmt::{And,Xor,Zero,One}`. The type-directed product in `crates/ir/volar-ir-common/src/field.rs` recurses for `Bit`, integer vectors, `Vec`, and `ExtField`.

`Block` and `Func` stay control types and still fail closed as data. Every value type (`Bit`, integer primitives, `Vec`, `Tuple`, `ExtField`, `PrimeField`) is lowerable.

## Prime field type

`IrType::PrimeField` sits next to `ExtField`:

```rust
PrimeField {
    k: u32,           // bit length, 2..=256
    n: Vec<u64>,      // little-endian, p = 2^k - n
}
```

`TypeTable::prime_field` interns only when `k` is in `2..=256`, `n > 0`, `n < 2^(k-1)` (so the top bit of `p` is set and `k` is the unique bit length), and `p` is prime. Primality is deterministic Miller-Rabin. `p = 2` is rejected; GF(2) remains `Bit`.

An element is an integer in `0..p`, stored in `k` bits, LSB first. That is the same width `ir_type_bits` already needs, so a prime field no longer has to pretend to be a bit vector or to have no width.

Canonical constructor, used everywhere `Z3` was a semantic choice:

- `z3()`: `PrimeField { k: 2, n: [1] }` because `2^2 - 1 = 3`.

`Z3` is removed from the schema unit enum. `raise_bits_to_z3` interns `types.z3()`.

Text form, parallel to `extfield`: `type i primefield <k> <n0> …`. Writer sugar prints `z3` when the payload matches `z3()`. The parser accepts that sugar and the general form.

`LirType::Native` stays a primitive hook and is not the prime-field carrier. The integer LIR path does not grow a `uint` encoding of `p`. Prime-field values lower through the field sink below.

## Poly over a prime field

`Poly` stays the only arithmetic statement. The `u8` coefficient remains a repetition count, not a field element. A non-trivial scalar is a `Const` factor. What changes is the sum:

- Characteristic 2 (`Bit`, integer primitives, `Vec` of those, `ExtField`): the sum is XOR, and `coeff & 1` cancels duplicates.
- `PrimeField`: the sum is addition mod `p` and the product is multiplication mod `p`. The stored repetition is `coeff mod p` when that residue fits in `u8` (always, for `p < 256`, which covers `z3()`). A residue that does not fit stays a `Const` factor, so `PolyCoeffs` stays `u8`. The empty product is `1`. `a * a` is the square. `mul_is_idempotent` is false.

Folds skip idempotent rewrites on `ExtField`. Prime fields join that gate. Parity cancellation does not run. Like monomials combine by addition mod `p`. Constant `0` still kills a monomial. A constant factor drops out only when it is `1` in that field. `raise_bits_to_z3` keeps the Möbius transform; its coefficients `0`, `1`, and `2` are residues mod 3, and evaluation uses the prime-field sum so `{0,1}` inputs still match the original bit polynomial.

`ExtField.wrapped` stays `Bit` or `ExtField`. A prime field is not a coefficient field. It lowers by the lift below, rather than by extending Rabin's test.

## One lowering seam

`FieldSink` lives in `volar-ir-common` (alloc, no_std). Wires are an associated type. The operations are the native field's addition, subtraction, multiplication, and a constant in `0..p` (for `Bit`, constants `0` and `1`).

- **Boolar sink.** Native field is `Bit`. `add` is `Xor`, `mul` is `And`, constants are `Zero` and `One`, using the hash-consed `Emitter` in `lower_ir_to_boolar`. `lower_ir_to_boolar` is this sink with the native type set to `Bit`.
- **Volar IR sink.** Native field is any other interned field (`PrimeField` or `ExtField`). Each wire is one SSA value of that type. `add(a, b)` emits `Poly` of two degree-1 monomials, `mul(a, b)` emits one two-factor monomial, and a constant emits `Const`. Evaluation of that `Poly` is the native field operation, so the producer does not reimplement schoolbook or Solinas.

The recursion in front of the sink is shared:

- **Same field as native.** One wire. `add` and `mul` are the sink. A `PrimeField` lowered to itself is not bit-blasted.
- **`Vec(n, E)` and integer primitives.** Unroll. Lane `i` recurses at `E`. A scalar of type `E` or `Bit` spreads. Length mismatch fails closed. `Tuple` is concatenation of its parts.
- **`ExtField`.** Unroll to `degree` coefficients and recurse at `wrapped`. Schoolbook multiplication uses the coefficient representation's own `add` and `mul`, which bottom out at the sink when a coefficient is one native wire.
- **Foreign `PrimeField` (`2^k - n`).** Solinas reduction on `k` boolean digits. A product's high half `h` and low half `l` satisfy `x ≡ l + h*n (mod 2^k - n)`, and `n < 2^(k-1)` keeps that correction inside a fixed circuit. Those digits are boolean, so carry chains use the embedded GF(2) operations of the native field, not the native `add` when the native characteristic is odd.
- **Foreign `Bit` into an odd prime.** Embed `0` and `1`. AND is native `mul`. XOR is `a + b - 2ab`. Into a characteristic-2 native field (`Bit` or `ExtField`), AND is `mul` and XOR is `add` after the same `0`/`1` embedding.
- **Foreign `ExtField`.** Already handled by unrolling; its coefficients then follow the rules above.

Boolean embedding is the only place characteristic matters inside the sink's caller:

- characteristic 2: embedded XOR is `add`
- odd prime: embedded XOR is `a + b - 2ab`

`lower_to_native(blocks, types, native)` returns Boolar blocks when `native` is `Bit`, and `(IRBlocks, IRTypes)` otherwise. Jump arguments flatten to the native wires of each source value, matching bit flattening in `flatten_bits`. Storage of a value that became `w` native wires expands to `w` cells on the Boolar sink the same way bits already expand.

## Tests

Semantics preservation, not statement-shape asserts. Evaluate the source IR and the lowered form.

- `z3()` interns; `2^4 - 1 = 15` is rejected; `n >= 2^(k-1)` is rejected; `2^3 - 3 = 5` interns.
- A GF(3) product, including a square, matches between the interpreter and Boolar.
- `raise_bits_to_z3` still agrees with the bit polynomial on `{0,1}` inputs.
- A `Vec` of `z3` maps per lane. An `aes8` product still matches Boolar through the same sink.
- Lowering two `z3` constants `2` and `2` with native `z3()` yields IR whose `Poly` evaluates to `1`.
- Lowering a bit AND/XOR into native `z3()` matches the embedded formulas on `{0,1}`.
