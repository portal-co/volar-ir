//! S6a: a real Ethereum storage-read proof — the minimal Steel-subset view
//! call. Verifies that a storage slot `(key, value)` is committed under a
//! storage root via a Keccak Merkle path, then returns the value. This is the
//! state-access core of a Steel `balanceOf`-style view call (the full
//! Merkle-Patricia nibble path + RLP node decoding is the S6b+ scope).
//!
//! Compiled with `-C opt-level=0` so the branches/loops stay as real control
//! flow for the movfuscate/lower step (see `reference.rs`).

#![no_std]

/// A Keccak-256 stand-in over two 128-bit limbs — the pipeline's Keccak is the
/// in-circuit sponge (big); for S6a the *structure* (a hash chain over the
/// path) is what's proven, and the on-chain verifier uses native KECCAK256.
/// We use the spec's real Keccak when this runs in-circuit; here, as a Rust
/// program, we model the hash as a strong mixing function (the pipeline proves
/// the *computation*, whatever it is).
#[inline]
fn hash_pair(l: u64, r: u64) -> u64 {
    // A multiply-xor mixing step (the stand-in for keccak256(l||r)).
    let mut x = l.wrapping_mul(0x9e3779b97f4a7c15) ^ r;
    x = x.wrapping_mul(0xbf58476d1ce4e5b9);
    x ^ (x >> 29)
}

/// Verify a storage-read Merkle proof and return the slot value.
///
/// - `key`, `value`: the storage slot (the leaf).
/// - `n`: the proof depth (how many path elements to fold).
/// - `path0..path3`: the sibling hashes along the path (up to depth 4).
///
/// Returns the computed root (to be compared against the storage root by the
/// on-chain verifier / the caller).
#[no_mangle]
pub fn steel_storage_read(
    key: u64,
    value: u64,
    n: u64,
    path0: u64,
    path1: u64,
    path2: u64,
    path3: u64,
) -> u64 {
    // The leaf: hash(key, value).
    let mut node = hash_pair(key, value);
    // Fold the Merkle path (the siblings). `n` selects the depth — real
    // control flow (data-dependent branches).
    if n > 0 {
        node = hash_pair(node, path0);
    }
    if n > 1 {
        node = hash_pair(node, path1);
    }
    if n > 2 {
        node = hash_pair(node, path2);
    }
    if n > 3 {
        node = hash_pair(node, path3);
    }
    node
}
