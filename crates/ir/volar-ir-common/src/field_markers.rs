// @reliability: normal
//! @ai: assisted
//! Field markers: the single source of truth naming extension and prime
//! fields across the AST, LIR, and VAFFLE/Volar IR layers.
//!
//! A **field marker** is a zero-sized type (native side, see
//! `volar-spec`'s `field_marker` module) or a reserved type name (compiler
//! side, see `volar-compiler`'s parser) that resolves to one entry of this
//! registry. Following the house marker patterns (`volar-discipline`'s
//! `Zk`/`Transparent`, `volar-iop`'s `BetaOf` tower levels, `volar-ir-build`'s
//! pipeline stages), the marker carries field identity at compile time so
//! that native execution and IR compilation resolve the *same* field spec —
//! the anti-embedding-gap measure from
//! `docs/warp-vole-verifier-zksnark-plan.md` §5.2.
//!
//! # Reserved names
//!
//! Binary extension fields are named `Gf2_<degree>` and prime fields
//! `Fp<decimal p>`; the [`is_reserved_marker_name`] prefixes reserve those
//! namespaces so a typo'd marker name is a fail-closed error at type
//! resolution instead of a silently-unresolved type parameter.
//!
//! # Single source of truth
//!
//! Every irreducible polynomial and modulus is written exactly once, here.
//! Native marker impls in `volar-spec` re-export these constants, the
//! `volar-compiler` parser resolves names through [`resolve_ext_field_marker`]
//! / [`resolve_prime_field_marker`], and interning into a
//! [`TypeTable`](crate::TypeTable) re-validates through the existing
//! fail-closed `ext_field`/`prime_field` rules.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

/// An extension-field registry entry: `GF(2^degree)` in polynomial basis
/// over `Bit`, modulus `irreducible` (low coefficient first, monic, length
/// `degree + 1`).
///
/// Nested/tower wrappings (`wrapped` = another field) are representable in
/// `IrType::ExtField` but deliberately have no registry entries yet — the
/// WARP accumulation field decision
/// (`docs/warp-vole-verifier-zksnark-plan.md` §5.4) accumulates over the VOLE
/// field `Gf2_128` directly, so no tower markers are provided until a
/// reviewed need exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExtFieldMarkerSpec {
    /// Reserved type name, e.g. `Gf2_128`.
    pub name: &'static str,
    /// Extension degree over `Bit`.
    pub degree: u32,
    /// Irreducible polynomial, low coefficient first, including the leading
    /// 1 at index `degree`.
    pub irreducible: &'static [u64],
}

/// A prime-field registry entry: `p = 2^k - n`, canonical per
/// `prime.rs`'s interning rules (`k` in `2..=256`, `n < 2^(k-1)`, `p` prime).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrimeFieldMarkerSpec {
    /// Reserved type name, e.g. `Fp3`.
    pub name: &'static str,
    /// Bit length of `p`.
    pub k: u32,
    /// `n` as a little-endian `u64` limb slice; `p = 2^k - n`.
    pub n: &'static [u64],
}

/// Build a sparse binary irreducible: coefficient 1 at each listed exponent.
/// Const-compatible so registry entries can name `'static` arrays.
const fn sparse_binary<const LEN: usize>(exponents: &[u32]) -> [u64; LEN] {
    let mut out = [0u64; LEN];
    let mut i = 0;
    while i < exponents.len() {
        out[exponents[i] as usize] = 1;
        i += 1;
    }
    out
}

/// `x^8 + x^4 + x^3 + x + 1` — the AES field, `PrimitiveType::Galois`'s field.
pub const GF2_8_IRREDUCIBLE: [u64; 9] = sparse_binary(&[0, 1, 3, 4, 8]);
/// `x^64 + x^4 + x^3 + x + 1` — `PrimitiveType::Galois64`'s field.
pub const GF2_64_IRREDUCIBLE: [u64; 65] = sparse_binary(&[0, 1, 3, 4, 64]);
/// `x^128 + x^7 + x^2 + x + 1` — `PrimitiveType::Galois128`'s field and the
/// VOLE field (the WARP accumulation field per the plan's §5.4 decision).
pub const GF2_128_IRREDUCIBLE: [u64; 129] = sparse_binary(&[0, 1, 2, 7, 128]);
/// `x^256 + x^10 + x^5 + x^2 + 1` — `PrimitiveType::Galois256`'s field.
pub const GF2_256_IRREDUCIBLE: [u64; 257] = sparse_binary(&[0, 2, 5, 10, 256]);

/// The AES field `GF(2^8)`.
pub const GF2_8: ExtFieldMarkerSpec = ExtFieldMarkerSpec {
    name: "Gf2_8",
    degree: 8,
    irreducible: &GF2_8_IRREDUCIBLE,
};
/// `GF(2^64)`.
pub const GF2_64: ExtFieldMarkerSpec = ExtFieldMarkerSpec {
    name: "Gf2_64",
    degree: 64,
    irreducible: &GF2_64_IRREDUCIBLE,
};
/// The VOLE field `GF(2^128)` (`x^128 + x^7 + x^2 + x + 1`).
pub const GF2_128: ExtFieldMarkerSpec = ExtFieldMarkerSpec {
    name: "Gf2_128",
    degree: 128,
    irreducible: &GF2_128_IRREDUCIBLE,
};
/// `GF(2^256)`.
pub const GF2_256: ExtFieldMarkerSpec = ExtFieldMarkerSpec {
    name: "Gf2_256",
    degree: 256,
    irreducible: &GF2_256_IRREDUCIBLE,
};

/// GF(3) — `PrimitiveType::Z3`'s field, `p = 2^2 - 1`.
pub const FP3: PrimeFieldMarkerSpec = PrimeFieldMarkerSpec {
    name: "Fp3",
    k: 2,
    n: &[1],
};

/// Every registered binary extension-field marker.
pub const EXT_FIELD_MARKERS: &[ExtFieldMarkerSpec] = &[GF2_8, GF2_64, GF2_128, GF2_256];
/// Every registered prime-field marker.
pub const PRIME_FIELD_MARKERS: &[PrimeFieldMarkerSpec] = &[FP3];

/// Resolve a reserved extension-field marker name to its spec.
pub fn resolve_ext_field_marker(name: &str) -> Option<&'static ExtFieldMarkerSpec> {
    EXT_FIELD_MARKERS.iter().find(|spec| spec.name == name)
}

/// Resolve a reserved prime-field marker name to its spec.
pub fn resolve_prime_field_marker(name: &str) -> Option<&'static PrimeFieldMarkerSpec> {
    PRIME_FIELD_MARKERS.iter().find(|spec| spec.name == name)
}

/// Reverse lookup: the registered marker name for an extension-field spec,
/// if any. Used by Display impls so resolved types print as their marker.
pub fn ext_marker_name(degree: u32, irreducible: &[u64]) -> Option<&'static str> {
    EXT_FIELD_MARKERS
        .iter()
        .find(|spec| spec.degree == degree && spec.irreducible == irreducible)
        .map(|spec| spec.name)
}

/// Reverse lookup: the registered marker name for a prime-field spec, if any.
pub fn prime_marker_name(k: u32, n: &[u64]) -> Option<&'static str> {
    PRIME_FIELD_MARKERS
        .iter()
        .find(|spec| spec.k == k && spec.n == n)
        .map(|spec| spec.name)
}

/// Whether `name` sits in a reserved marker namespace (`Gf2_<…>` or `Fp<…>`).
///
/// Callers treat a reserved-but-unregistered name as a fail-closed error:
/// it names a field marker that does not exist, not a type parameter.
pub fn is_reserved_marker_name(name: &str) -> bool {
    name.starts_with("Gf2_") || name.starts_with("Fp")
}

/// Why [`validate_registry`] refused the registry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FieldMarkerError {
    /// A marker name is registered in neither table.
    UnregisteredName(String),
    /// A registry entry fails the interning rules of `field.rs`/`prime.rs`.
    InvalidSpec(String),
    /// A reserved-namespace collision or duplicate name.
    DuplicateName(String),
}

/// Validate the whole registry against the same fail-closed rules interning
/// uses: every extension entry must intern via
/// [`TypeTable::ext_field`](crate::TypeTable::ext_field) (monic, irreducible,
/// coefficients in range) over `Bit`, and every prime entry via
/// [`TypeTable::prime_field`](crate::TypeTable::prime_field) (canonical
/// `2^k - n`, prime).
pub fn validate_registry() -> Result<(), FieldMarkerError> {
    for spec in EXT_FIELD_MARKERS {
        let mut types = crate::TypeTable::new();
        let bit = types.bit();
        types
            .ext_field(bit, spec.degree, spec.irreducible.to_vec())
            .map_err(|e| FieldMarkerError::InvalidSpec(alloc::format!("{}: {e:?}", spec.name)))?;
    }
    for spec in PRIME_FIELD_MARKERS {
        let mut types = crate::TypeTable::new();
        types
            .prime_field(spec.k, spec.n.to_vec())
            .map_err(|e| FieldMarkerError::InvalidSpec(alloc::format!("{}: {e:?}", spec.name)))?;
    }
    let mut names: Vec<&str> = Vec::new();
    for name in EXT_FIELD_MARKERS
        .iter()
        .map(|s| s.name)
        .chain(PRIME_FIELD_MARKERS.iter().map(|s| s.name))
    {
        if !is_reserved_marker_name(name) {
            return Err(FieldMarkerError::InvalidSpec(alloc::format!(
                "{name}: registered marker must use a reserved prefix"
            )));
        }
        if names.contains(&name) {
            return Err(FieldMarkerError::DuplicateName(name.into()));
        }
        names.push(name);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_validates_against_interning_rules() {
        validate_registry().unwrap();
    }

    #[test]
    fn reserved_prefixes_are_fail_closed_namespaces() {
        assert!(is_reserved_marker_name("Gf2_128"));
        assert!(is_reserved_marker_name("Gf2_127")); // reserved, but unregistered
        assert!(is_reserved_marker_name("Fp3"));
        assert!(is_reserved_marker_name("Fp25519")); // reserved, but unregistered
        assert!(!is_reserved_marker_name("MyTypeParam"));
        assert!(!is_reserved_marker_name("Galois128")); // legacy primitive, not a marker
    }

    #[test]
    fn resolution_round_trips_registered_names() {
        assert_eq!(resolve_ext_field_marker("Gf2_128"), Some(&GF2_128));
        assert_eq!(resolve_ext_field_marker("Gf2_8"), Some(&GF2_8));
        assert_eq!(resolve_prime_field_marker("Fp3"), Some(&FP3));
        assert_eq!(resolve_ext_field_marker("Gf2_127"), None);
        assert_eq!(resolve_ext_field_marker("Galois128"), None);
        assert_eq!(resolve_prime_field_marker("Fp4"), None);
    }

    #[test]
    fn irreducibles_match_the_existing_field_rs_tables() {
        assert_eq!(GF2_8.irreducible, crate::aes8_irreducible().as_slice());
        assert_eq!(GF2_64.irreducible, crate::galois64_irreducible().as_slice());
        // The VOLE field: x^128 + x^7 + x^2 + x + 1.
        let mut expected = [0u64; 129];
        for e in [0usize, 1, 2, 7, 128] {
            expected[e] = 1;
        }
        assert_eq!(GF2_128.irreducible, expected.as_slice());
    }
}
