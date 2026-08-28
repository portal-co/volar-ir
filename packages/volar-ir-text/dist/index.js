import { ROOTS } from "./generated/schema.js";
export * from "./generated/schema.js";
export class TextParseError extends Error {
    line;
    column;
    constructor(message, line = 1, column = 1) {
        super(message);
        this.name = "TextParseError";
        this.line = line;
        this.column = column;
    }
}
/**
 * Parses the schema-owned structural text family used by roots without a
 * legacy grammar. Integers use a mandatory `n` suffix so JavaScript never
 * silently loses precision.
 */
export function parsePortableText(text) {
    const newline = text.indexOf("\n");
    if (newline < 0)
        throw new TextParseError("missing portable text header");
    const header = text.slice(0, newline).trim();
    const match = /^volar-portable-text v1 root=([a-z0-9-]+)$/.exec(header);
    if (!match || !(match[1] in ROOTS))
        throw new TextParseError("unknown portable text root");
    const parser = new StructuralParser(text.slice(newline + 1));
    const value = parser.value();
    parser.end();
    return { root: match[1], value };
}
/** Emits deterministic schema-owned structural text. */
export function emitPortableText(document) {
    if (!(document.root in ROOTS))
        throw new TextParseError("unknown portable text root");
    return `volar-portable-text v1 root=${document.root}\n${emitValue(document.value)}\n`;
}
/**
 * The schema roots whose canonical v1 text is the structural portable family.
 * Other roots retain their dedicated Rust grammar while their TypeScript
 * grammar is introduced; callers can still use this codec for schema-native
 * interchange documents for every root.
 */
export function isStructuralTextRoot(root) {
    return ROOTS[root].profile === "portable-v1";
}
class StructuralParser {
    input;
    at = 0;
    constructor(input) {
        this.input = input;
    }
    value() {
        this.space();
        const ch = this.input[this.at];
        if (ch === '"')
            return this.string();
        if (ch === '[')
            return this.list();
        if (ch === '{')
            return this.object();
        if (this.input.startsWith("true", this.at))
            return this.word("true", true);
        if (this.input.startsWith("false", this.at))
            return this.word("false", false);
        if (this.input.startsWith("null", this.at))
            return this.word("null", null);
        return this.integer();
    }
    end() { this.space(); if (this.at !== this.input.length)
        this.fail("trailing input"); }
    list() {
        this.at++;
        const values = [];
        this.space();
        while (this.input[this.at] !== ']') {
            values.push(this.value());
            this.space();
            if (this.input[this.at] !== ',')
                break;
            this.at++;
            this.space();
        }
        if (this.input[this.at++] !== ']')
            this.fail("expected ]");
        return values;
    }
    object() {
        this.at++;
        const result = {};
        this.space();
        while (this.input[this.at] !== '}') {
            const key = this.string();
            this.space();
            if (this.input[this.at++] !== ':')
                this.fail("expected :");
            const value = this.value();
            if (key in result)
                this.fail("duplicate object key");
            result[key] = value;
            this.space();
            if (this.input[this.at] !== ',')
                break;
            this.at++;
            this.space();
        }
        if (this.input[this.at++] !== '}')
            this.fail("expected }");
        return result;
    }
    string() { const start = this.at; let escaped = false; this.at++; while (this.at < this.input.length) {
        const ch = this.input[this.at++];
        if (!escaped && ch === '"') {
            try {
                return JSON.parse(this.input.slice(start, this.at));
            }
            catch {
                this.fail("invalid string");
            }
        }
        escaped = !escaped && ch === "\\";
        if (ch !== "\\")
            escaped = false;
    } this.fail("unterminated string"); }
    integer() { const rest = this.input.slice(this.at); const match = /^-?(?:0|[1-9][0-9]*)n/.exec(rest); if (!match)
        this.fail("expected value"); this.at += match[0].length; return BigInt(match[0].slice(0, -1)); }
    word(word, value) { this.at += word.length; return value; }
    space() { while (/\s/.test(this.input[this.at] ?? ""))
        this.at++; }
    fail(message) { const before = this.input.slice(0, this.at); throw new TextParseError(message, before.split("\n").length, this.at - before.lastIndexOf("\n")); }
}
function emitValue(value) {
    if (typeof value === "bigint")
        return `${value}n`;
    if (typeof value === "string")
        return JSON.stringify(value);
    if (typeof value === "boolean" || value === null)
        return String(value);
    if (Array.isArray(value))
        return `[${value.map(emitValue).join(",")}]`;
    const object = value;
    return `{${Object.keys(object).sort().map((key) => `${JSON.stringify(key)}:${emitValue(object[key])}`).join(",")}}`;
}
