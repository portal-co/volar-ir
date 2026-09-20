# Domain Docs

How engineering skills should consume this repo's domain documentation.

## Before exploring, read these

- **`CONTEXT.md`** at the repo root, or **`CONTEXT-MAP.md`** if it exists.
- **`docs/adr/`** — read ADRs that touch the area being explored.

If these files do not exist, proceed silently. The domain-modeling skill creates them lazily when terminology or architectural decisions are resolved.

## Layout

This is a **single-context** repository. Domain documentation uses one root `CONTEXT.md` and `docs/adr/` for repository-wide decisions.

## Vocabulary

When naming domain concepts in issues, refactor proposals, hypotheses, or tests, use terminology from `CONTEXT.md`. If a required concept is not defined there, treat that as a domain-modeling gap rather than silently inventing a synonym.

## ADR conflicts

If work contradicts an existing ADR, surface that conflict explicitly instead of silently overriding the decision.
