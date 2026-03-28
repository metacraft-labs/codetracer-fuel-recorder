script;

use std::logging::log;

fn main() {
    let a: u64 = 10;
    let b: u64 = 32;
    let sum_val: u64 = a + b;       // 42
    let doubled: u64 = sum_val * 2; // 84
    let final_result: u64 = doubled + a; // 94
    log(final_result);
}
