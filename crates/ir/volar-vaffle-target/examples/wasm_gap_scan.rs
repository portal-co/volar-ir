//! Gap scanner (volar-ir ↔ ai-support gateway spike, `gateway-softfloat-spike`
//! branch): parse a `.wasm` file, run `lower_waffle_module`, and report which
//! functions fail the lowering, grouped by failing operator/terminator kind.
//!
//! Usage:
//!   cargo run -p volar-vaffle-target --example wasm_gap_scan -- <module.wasm> [memory_address_bits]

use std::collections::BTreeMap;

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: wasm_gap_scan <module.wasm> [memory_address_bits]");
    let bits: Option<usize> = std::env::args().nth(2).and_then(|s| s.parse().ok());
    let bytes = std::fs::read(&path).expect("read wasm");

    let module = portal_pc_waffle_frontend::from_wasm_bytes(&bytes, &Default::default())
        .expect("parse wasm");

    // Function count for the ok/failed tally (includes declarations/imports).
    let total = module.funcs.len();

    let mut target =
        volar_vaffle_target::VaffleTarget::with_pointer_width(vaffle::PointerWidth::Bits32);
    let mut config = volar_vaffle_target::WaffleImportConfig::new();
    if let Some(b) = bits {
        config = config.with_memory_address_bits(b);
    }
    let errors = volar_vaffle_target::lower_waffle_module(&module, &mut target, &config);

    // Histogram by error constructor (first token of the message).
    let mut hist: BTreeMap<String, (usize, Vec<String>, Vec<String>)> = BTreeMap::new();
    for (func, err) in &errors {
        let msg = err.to_string();
        // Messages look like "unsupported WAFFLE op: F64Add { .. }" — group by
        // the operator constructor (first token after the "op:" marker).
        let kind = msg
            .rsplit_once("op: ")
            .map(|(_, rest)| rest)
            .unwrap_or(&msg)
            .split(|c: char| c == '(' || c == ' ' || c == ':' || c == '{')
            .next()
            .unwrap_or("?")
            .to_string();
        let entry = hist.entry(kind).or_default();
        entry.0 += 1;
        if entry.1.len() < 4 {
            entry.1.push(func.clone());
        }
        if entry.2.len() < 3 {
            entry.2.push(msg);
        }
    }

    println!("== wasm gap scan: {path}");
    println!("functions: {} body decls, {} failed to lower", total, errors.len());
    println!();
    println!("{:>7}  {:<32} sample functions", "count", "gap");
    for (kind, (n, samples, msgs)) in &hist {
        println!("{n:>7}  {kind:<32} {}", samples.join(", "));
        for m in msgs {
            println!("         | {m}");
        }
    }
    if errors.is_empty() {
        println!("no gaps — module lowers cleanly");
    }
}
