import { ROOTS, type PortableRootId } from "./generated/schema.js";

export * from "./generated/schema.js";

/** Every integer in the cross-language model is exact. */
export type IrInteger = bigint;

export class TextParseError extends Error {
  readonly line: number;
  readonly column: number;

  constructor(message: string, line = 1, column = 1) {
    super(message);
    this.name = "TextParseError";
    this.line = line;
    this.column = column;
  }
}

export type PortableTextValue =
  | IrInteger
  | string
  | boolean
  | null
  | readonly PortableTextValue[]
  | { readonly [field: string]: PortableTextValue };

export interface PortableDocument<Root extends PortableRootId = PortableRootId> {
  readonly root: Root;
  readonly value: PortableTextValue;
}

/**
 * Parses the schema-owned structural text family used by roots without a
 * legacy grammar. Integers use a mandatory `n` suffix so JavaScript never
 * silently loses precision.
 */
export function parsePortableText(text: string): PortableDocument {
  const newline = text.indexOf("\n");
  if (newline < 0) throw new TextParseError("missing portable text header");
  const header = text.slice(0, newline).trim();
  const match = /^volar-portable-text v1 root=([a-z0-9-]+)$/.exec(header);
  if (!match || !(match[1] in ROOTS)) throw new TextParseError("unknown portable text root");
  const parser = new StructuralParser(text.slice(newline + 1));
  const value = parser.value();
  parser.end();
  return { root: match[1] as PortableRootId, value };
}

/** Emits deterministic schema-owned structural text. */
export function emitPortableText(document: PortableDocument): string {
  if (!(document.root in ROOTS)) throw new TextParseError("unknown portable text root");
  return `volar-portable-text v1 root=${document.root}\n${emitValue(document.value)}\n`;
}

/**
 * The schema roots whose canonical v1 text is the structural portable family.
 * Other roots retain their dedicated Rust grammar while their TypeScript
 * grammar is introduced; callers can still use this codec for schema-native
 * interchange documents for every root.
 */
export function isStructuralTextRoot(root: PortableRootId): boolean {
  return ROOTS[root].profile === "portable-v1";
}

class StructuralParser {
  private at = 0;
  constructor(private readonly input: string) {}
  value(): PortableTextValue {
    this.space();
    const ch = this.input[this.at];
    if (ch === '"') return this.string();
    if (ch === '[') return this.list();
    if (ch === '{') return this.object();
    if (this.input.startsWith("true", this.at)) return this.word("true", true);
    if (this.input.startsWith("false", this.at)) return this.word("false", false);
    if (this.input.startsWith("null", this.at)) return this.word("null", null);
    return this.integer();
  }
  end(): void { this.space(); if (this.at !== this.input.length) this.fail("trailing input"); }
  private list(): readonly PortableTextValue[] {
    this.at++; const values: PortableTextValue[] = []; this.space();
    while (this.input[this.at] !== ']') { values.push(this.value()); this.space(); if (this.input[this.at] !== ',') break; this.at++; this.space(); }
    if (this.input[this.at++] !== ']') this.fail("expected ]"); return values;
  }
  private object(): { readonly [field: string]: PortableTextValue } {
    this.at++; const result: Record<string, PortableTextValue> = {}; this.space();
    while (this.input[this.at] !== '}') { const key = this.string(); this.space(); if (this.input[this.at++] !== ':') this.fail("expected :"); const value = this.value(); if (key in result) this.fail("duplicate object key"); result[key] = value; this.space(); if (this.input[this.at] !== ',') break; this.at++; this.space(); }
    if (this.input[this.at++] !== '}') this.fail("expected }"); return result;
  }
  private string(): string { const start = this.at; let escaped = false; this.at++; while (this.at < this.input.length) { const ch = this.input[this.at++]; if (!escaped && ch === '"') { try { return JSON.parse(this.input.slice(start, this.at)) as string; } catch { this.fail("invalid string"); } } escaped = !escaped && ch === "\\"; if (ch !== "\\") escaped = false; } this.fail("unterminated string"); }
  private integer(): bigint { const rest = this.input.slice(this.at); const match = /^-?(?:0|[1-9][0-9]*)n/.exec(rest); if (!match) this.fail("expected value"); this.at += match![0].length; return BigInt(match![0].slice(0, -1)); }
  private word<T extends boolean | null>(word: string, value: T): T { this.at += word.length; return value; }
  private space(): void { while (/\s/.test(this.input[this.at] ?? "")) this.at++; }
  private fail(message: string): never { const before = this.input.slice(0, this.at); throw new TextParseError(message, before.split("\n").length, this.at - before.lastIndexOf("\n")); }
}

function emitValue(value: PortableTextValue): string {
  if (typeof value === "bigint") return `${value}n`;
  if (typeof value === "string") return JSON.stringify(value);
  if (typeof value === "boolean" || value === null) return String(value);
  if (Array.isArray(value)) return `[${value.map(emitValue).join(",")}]`;
  const object = value as { readonly [field: string]: PortableTextValue };
  return `{${Object.keys(object).sort().map((key) => `${JSON.stringify(key)}:${emitValue(object[key]!)}`).join(",")}}`;
}
