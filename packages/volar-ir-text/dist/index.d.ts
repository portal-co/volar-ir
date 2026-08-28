import { type PortableRootId } from "./generated/schema.js";
export * from "./generated/schema.js";
/** Every integer in the cross-language model is exact. */
export type IrInteger = bigint;
export declare class TextParseError extends Error {
    readonly line: number;
    readonly column: number;
    constructor(message: string, line?: number, column?: number);
}
export type PortableTextValue = IrInteger | string | boolean | null | readonly PortableTextValue[] | {
    readonly [field: string]: PortableTextValue;
};
export interface PortableDocument<Root extends PortableRootId = PortableRootId> {
    readonly root: Root;
    readonly value: PortableTextValue;
}
/**
 * Parses the schema-owned structural text family used by roots without a
 * legacy grammar. Integers use a mandatory `n` suffix so JavaScript never
 * silently loses precision.
 */
export declare function parsePortableText(text: string): PortableDocument;
/** Emits deterministic schema-owned structural text. */
export declare function emitPortableText(document: PortableDocument): string;
/**
 * The schema roots whose canonical v1 text is the structural portable family.
 * Other roots retain their dedicated Rust grammar while their TypeScript
 * grammar is introduced; callers can still use this codec for schema-native
 * interchange documents for every root.
 */
export declare function isStructuralTextRoot(root: PortableRootId): boolean;
