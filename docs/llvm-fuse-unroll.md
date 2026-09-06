# Movfuscated LLVM workloads use typed step circuits

Linked LLVM workloads such as SHA-256 and HKDF must not be materialized by a
caller-selected repetition budget. The old bounded fusion route was removed.

The supported route is:

```text
LLVM / VAFFLE → Volar IR → movfuscate → VStepCircuit → BStepCircuit
```

`VStepCircuit` emits the movfuscated body once and names `terminated`,
`next_state`, and `return_values`. A host driver repeatedly feeds
`next_state` back into the step and stops when `terminated` is true. The
Boolar adapter retains the allocation tables used to translate typed watches
into LSB-first Boolean watches and then reversible-wire watches.

For a program whose control flow is fully concrete, use
`unroll_ir_everything` instead; that pass has a separate concrete-control
contract and is not a symbolic-loop fallback.
