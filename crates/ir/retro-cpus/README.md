# retro-cpus

Full CPU interpreters generated from the [retrop](../../../retrop) decoder +
semantics tables. Currently: **WDC 65C02** (204 documented opcodes incl.
Rockwell RMB/SMB/BBR/BBS).

Two independent engines consume the same generated tables
(`src/generated/m6502.rs`):

- **`cpu::Machine`** — a concrete software interpreter (`step()` executes one
  instruction from an internal 64 KiB RAM image). No dependencies beyond this
  workspace; usable as an embedded 65C02 execution model.
- **`bir::step_bir()`** — compiles the same tables into Boolar IR: one
  oblivious single-block circuit per CPU step (~100k statements, 54 state-bit
  params, RAM as bit-granular `StorageRead`/`StorageWrite` with
  indicator-guarded addresses and XOR one-hot state selection). The output is
  a standard `BIrBlocks<()>`: run it through `volar-ir-opt`, `movfuscate`,
  `lower_to_circuit`, or evaluate it directly with
  `volar_fuzz::interpreter::biir::eval_biir`.

## Regenerating

The generator stays in retrop (like volar's spec generators stay in volar):

```
cd ../../../retrop && cargo run -p retrop-emit-volar
```

This rewrites `src/generated/m6502.rs` and `tests/golden.rs`. Golden-vector
expected outcomes are computed by retrop itself — its decoder + semantics
interpreter are the ground truth both engines here are tested against.
