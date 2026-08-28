# Portable structural text format

Roots marked `portable-v1` in the schema use this generated family:

```text
volar-portable-text v1 root=<schema-root>
<value>
```

Values are JSON-shaped, except every integer has a mandatory `n` suffix and
therefore maps to an exact JavaScript `bigint`. Objects have quoted keys,
duplicate keys are rejected, and emitters sort object keys. This format carries
only the `P = ()` portable view of provenance.

The TypeScript package exposes this structural codec for every schema root so
tools can exchange an exact, bigint-safe intermediate document while dedicated
legacy/sectioned emitters are introduced. It does not relabel the canonical
`.vlm` or `.vir` grammars: their profile and header remain listed in the schema.
