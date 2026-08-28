export type PortableRootId = "saved-lir" | "volar-ir" | "boolar-ir" | "vaffle-module" | "v-circuit" | "b-circuit" | "reversible-circuit" | "chunked-b-circuit";
export interface RootMetadata {
    readonly rust: string;
    readonly typescript: string;
    readonly profile: string;
    readonly header: string;
}
export declare const ROOTS: Readonly<Record<PortableRootId, RootMetadata>>;
export type PrimitiveType = "Bit" | "_8" | "_16" | "_32" | "_64" | "_128" | "_256" | "AES8" | "Galois64" | "Z3";
export interface Constant {
    readonly hi: bigint;
    readonly lo: bigint;
}
export interface OracleDecl {
    readonly name: string;
    readonly params: ReadonlyArray<TypeId>;
    readonly results: ReadonlyArray<TypeId>;
}
export interface ActionDecl {
    readonly name: string;
    readonly params: ReadonlyArray<TypeId>;
    readonly results: ReadonlyArray<TypeId>;
}
export interface RngDecl {
    readonly name: string;
    readonly ty: TypeId;
}
export interface PreInitSegment {
    readonly storage: StorageId;
    readonly ty: TypeId;
    readonly offset: bigint;
    readonly data: ReadonlyArray<Constant>;
}
export type SideId = bigint;
export type TypeId = bigint;
export type StorageId = bigint;
export type IRBlockId = bigint;
export type IRVarId = bigint;
export type LaneId = bigint;
export interface BIrPreInitSegment {
    readonly storage: StorageId;
    readonly lane: LaneId;
    readonly offset: bigint;
    readonly data: ReadonlyArray<boolean>;
}
export type SigId = bigint;
export type FuncId = bigint;
export type BlockId = bigint;
export type ValueId = bigint;
export type ReversibleExternalKind = "Oracle" | "Rng";
export interface RCircuit {
    readonly num_wires: bigint;
    readonly gates: ReadonlyArray<unknown>;
}
export type IcmpPred = "Eq" | "Ne" | "Ult" | "Ule" | "Ugt" | "Uge" | "Slt" | "Sle" | "Sgt" | "Sge";
export interface WireSchedule {
    readonly resident_wires: bigint;
    readonly temporary_wires: bigint;
    readonly last_use: ReadonlyArray<bigint>;
    readonly release_at: ReadonlyArray<ReadonlyArray<bigint>>;
}
