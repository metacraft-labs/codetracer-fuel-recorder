// trivial_chain_noinline — Sway / FuelVM
// The simple_trivial_chain program with `compute` marked
// `#[inline(never)]`, so forc keeps it as a real function: its body keeps
// its own source-map entries and it runs in its own call frame.
script;

use std::logging::log;

#[inline(never)]
fn compute() -> u64 {
    let a: u64 = 10;
    let b: u64 = a;
    let c: u64 = b;
    c
}

fn main() {
    let result: u64 = compute();
    log(result);
}
