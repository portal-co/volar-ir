# LLVM dead `landingpad` / `unreachable` (rustc `-O0`)

**Status: landed.** Structural import now computes reachability from a
function’s first LLVM block before translating its body. It follows only the
supported `br` and `switch` successors, after the existing pass that allocates
all LLVM block IDs and phi parameters. Consequently, a rustc `-O0` dead
`terminate` block containing a `landingpad`, `cant_unwind` call, and
`unreachable` is not translated or added to the called-function worklist.

All LLVM block IDs and phi allocation order remain stable. Unreachable blocks
are retained as unmapped placeholders; their typed fallback return can never
be reached from the imported function entry.

## Fail-closed boundary

Reachability is deliberately not exception-flow support. A reachable
`invoke`, `landingpad`, `resume`, `cleanupret`, `catchret`, `catchswitch`, or
`unreachable` still reaches the importer’s named unsupported error. Only
unreachable dead code is skipped.

## Tests

Structural tests prove that a dead landingpad is ignored and a reachable
`invoke` remains unsupported. The end-to-end `poll_fsm` fixture includes the
dead rustc-style terminate block and successfully reaches movfuscation,
Boolar lowering, and fusion; its state-0, state-1, and default transitions
are evaluated before and after lowering.

## Out of scope

- Real LLVM exception handling and unwind edges.
- Lowering a reachable `unreachable` terminator.
- Pointer joins and other unrelated LLVM importer gaps.

Site canary `llvm_vaffle_rustc_o0_poll_fsm_landingpad` now expects fuse.
