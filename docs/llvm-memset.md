# LLVM `memset` / `memcpy` / `memmove` → storage operations

**Status: landed.** The structural LLVM importer lowers direct calls whose
callee name starts with `llvm.memset.`, `llvm.memcpy.`, or `llvm.memmove.`.
They do not become VAFFLE `Value::Call`s, declaration-only imported
functions, or worklist entries. This lets the rustc `-O0` `[N x i8]` stack
spill pattern unroll to a circuit.

## Supported contract

- The length must be a constant integer representable as `usize`, and the
  volatile operand must be the constant `false`. Symbolic or oversized lengths
  and volatile calls fail with a named `ImportError::Unsupported`.
- Each pointer must be either a direct global-storage base already supported by
  the importer, or a tracked constant-GEP pointer derived from one `alloca`.
  Stack pointers retain their allocation identity, current bit address, and
  allocation bounds; every intrinsic byte range is checked against those
  bounds.
- `memset` normalizes its fill operand to eight bits and emits one repeated
  byte sequence. Stack accesses use the existing bit-addressed `stack_load` /
  `stack_store` helpers; global accesses use the existing byte-addressed
  `mem_load` / `mem_store` helpers.
- `memcpy` reads its whole source before any destination write and rejects
  overlapping source/destination ranges. `memmove` has the same materialized
  read-before-write behavior, so constant-size overlapping moves within a
  single tracked alloca are supported.

The implementation intentionally does not reinterpret a nonzero global GEP
offset as byte zero: only the importer’s existing direct global-base path is
accepted. A pointer that leaves a tracked alloca’s provenance fails with a
named error rather than accessing a different allocation.

## Tests

Structural tests cover no residual intrinsic calls or declarations, symbolic
length and volatile rejection, disjoint `memcpy`, overlapping `memcpy`
rejection, same-alloca overlapping `memmove`, escaping GEP provenance
rejection. End-to-end fixtures verify that rustc-style stack `memset` unrolls
and evaluates `stack_spill(5)` as `5 ^ 6`, and that `memcpy` / `memmove`
produce their expected copied bytes.

## Out of scope

- Symbolic lengths or volatile memory intrinsics.
- Heap, pointer `phi` / `select`, symbolic GEP, and other unresolved pointer
  forms.
- Global pointer offsets beyond the pre-existing base-global support.
- Real exception handling; see [`llvm-landingpad.md`](llvm-landingpad.md).

The external site-canary expectation update is follow-up work and is not
modified in this repository.
