# ADR: Adaptive split & specialized regions (`volar-ir-virt`)

> ADR for [`virtualize_ir` adaptive split](../virt.md). Compiler context; see
> [reliability.md](../reliability.md) for the pinnedness/stability policy.

## Status

Accepted — v1 implements the narrow core; items below are explicitly deferred.

## Context

Whole-block virtualization deduplicates handlers but leaves two gaps:

1. **Cross-block** shared stmt cores with different prologues/epilogues.
2. **Intra-block** repeated bodies (unrolled loops) that pay outer-dispatcher
   cost per step.

v1 adds two mechanisms with different runtime models:

| Mechanism | Model |
|-----------|--------|
| **SharedCore** | Stepping sub-interpreter over appended opcode rows + `SUB_RETURN` |
| **RerollLoop** | Descriptor row + trip count; read operands N times; implicit return |

Both append rows to the **same** bytecode storage after outer program rows.
Appended rows are seeded via [`build_ir_storage_init_adaptive`](../../crates/ir/volar-ir-virt/src/preinit.rs)
into `pre_init` (same dense lane model as outer rows).

## v1 scope (shipped)

- `AdaptiveSplitConfig` on [`VirtualizeConfig`](../../crates/ir/volar-ir-virt/src/lib.rs),
  default **off**.
- Unified flat bytecode: outer rows `0..N-1`, appended regions after.
- Cross-block SharedCore **planner** + sub-interpreter *dispatch scaffolding*.
- Intra-block RerollLoop **planner** + reroll driver blocks.
- **CFG reentry hints** — `cfg_hints` planner uses `IRBranchTarget.reentry` on
  back-edges (including autoderived `BoundedLoop` hints) before structural
  reroll search. See [complexity-hints.md](complexity-hints.md).
- Whole-block composite handlers (opcodes-before / entry / resume /
  opcodes-after) for **segment-free plans** — i.e. the plain whole-block
  handler path, which is semantics-verified.
- Public dispatch, IR-only, no commitment extension.
- Equivalence tests with split enabled.

## Correction (portal-co/volar-ir#6)

The *appended-region emission* half of the two mechanisms above was found to
be **not implemented**, not merely unpolished: the sub-interpreter's
micro-handler blocks are never emitted (the sub-dispatch `JumpTable` targets
block ids that collide with the composite resume blocks), `SUB_RETURN`
terminates the program instead of stepping to the next region row, and the
composite part/resume handlers never remap canonical vars (the resume block
clones the raw terminator, so any block with a non-empty covered range and a
terminator referencing covered values emits dangling var refs). The planners
also let a single block's self-overlapping windows satisfy
`min_reuse_count`, so even single-block modules could select the broken path
(surfaced by volar-fuzz's `prop_virt_ir_adaptive_split_preserves_semantics`
once the Bit-only Type cutover made the generator produce all-Bit IR).

Until emission is implemented:

- `cross_block` / `loop_reroll` default to **off**; `enabled: true` alone
  selects only the correct whole-block path.
- [`virtualize_ir`](../../crates/ir/volar-ir-virt/src/ir.rs) **asserts** on
  plans containing appended regions rather than emitting a miscompile.
- The planners remain available (and unit-tested) for when emission lands;
  members are now at most one per block.

Unblock criteria for re-enabling: emit one handler block per region member
with static successor chaining (or next-pc threading), thread canonical
values across the part/region/resume boundary through the register file
(one register per canonical var of composite blocks), emit the resume
block's terminator through the same inline-arm-writes machinery as
`emit_handler_block`, and extend `emit_body_stmts`/`emit_prologue_stmts` to
all statement kinds instead of dropping them. Required equiv tests: the
volar-fuzz adaptive-split proptest at ≥512 cases plus a multi-block
SharedCore differential case.

## Deferring strategy

1. Ship the narrow correct core first; default-off toggle preserves today’s behavior.
2. Deferred items are **ADR-gated** — no ad-hoc integration without updating this doc.
3. Each deferred item lists unblock criteria and required equiv tests.
4. Extensibility hooks (e.g. `operand_mode` on reroll metadata) may land in v1 without implementing the optimization.

## Deferred items

| Item | Why deferred | Unblock criteria |
|------|--------------|------------------|
| `direct_dispatch` for sub-tier | Outer DD sufficient initially | Sub-interpreter equiv + block budget |
| `virtualize_ir_committed` + split | Hash must cover all global pcs + reroll descriptors | Committed equiv suite with split on |
| `DispatchMode::Oblivious` + split | `movfuscate_ir` lacks `JumpTable` | Movfuscate JumpTable support |
| BIR adaptive split | No register file / JumpTable on BIR path | BIR register-file mirroring |
| Backend `VirtBytecode` printers | `pre_init` is canonical; printers may consume either form | Separate printer ADR |
| **MUX-tree abstraction for reroll** | See motivation below | RAM↔SSA analysis + MUX cost model |

## Deferred motivation: MUX-tree + RAM for reroll

v1 reroll keeps loop-carried state in the SSA-style register file
([`RegAlloc`](../../crates/ir/volar-ir-virt/src/ir.rs)): each trip re-reads
operand slots. Correct and simple.

Future optimization (not v1):

- Represent reroll operand updates as a **MUX tree** over a small **RAM**
  bank instead of threading every carried value through registers.
- Loop body becomes read/compute/write on RAM indices; trip count unchanged.
- Moves state **from SSA back to RAM** for hot loops, reducing register-file
  pressure and backend spilling.

Requires: cross-trip liveness, RAM index assignment, MUX-tree construction.
Reroll descriptor metadata reserves `operand_mode: RegisterFile | RamMux`
(default `RegisterFile`) for this path.

## References

- [virt.md](../virt.md) — user-facing overview
- [PROGRESS.md](../../PROGRESS.md) — project status
