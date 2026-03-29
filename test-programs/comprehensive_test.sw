// Comprehensive Sway test program for the codetracer-fuel-recorder.
//
// This file documents the language constructs that the corresponding Rust
// integration tests (tests/test_comprehensive.rs) exercise at the FuelVM
// bytecode level.  Because we cannot invoke `forc` inside the Nix build
// environment, each scenario is built programmatically from fuel-asm
// instructions in the Rust tests.  This file serves as a human-readable
// reference for what each test represents in Sway-level semantics.
//
// The tests cover:
//
// 1. Arithmetic operations
//    let a: u64 = 10;
//    let b: u64 = 20;
//    let sum = a + b;       // ADD
//    let diff = b - a;      // SUB
//    let prod = a * b;      // MUL
//    let quot = b / a;      // DIV
//    let rem  = b % 3;      // MOD
//    // Immediate variants: ADDI, SUBI, MULI, DIVI, MODI
//
// 2. Comparison and branching
//    let x: u64 = 42;
//    let y: u64 = 42;
//    let is_eq  = x == y;   // EQ
//    let is_gt  = x > 10;   // GT
//    let is_lt  = x < 100;  // LT
//    if x > 10 { ... }      // JNEI / JNZI conditional jump
//
// 3. Register manipulation
//    let val: u64 = 0x1234; // MOVI
//    let copy = val;        // MOVE
//    let root = sqrt(val);  // MROO
//
// 4. Memory operations
//    // Store and load words to/from memory
//    // ALOC, SW, LW, MCL, MCP
//
// 5. Logging
//    log(a, b, c, d);       // LOG (4 register values)
//    // LOGD (data logging from memory)
//
// 6. Control flow patterns
//    // Simple branch: if value > threshold { ... }
//    // Loop: while counter > 0 { counter -= 1; }
//    // Nested branches
//    // Early return

script;

fn main() {
    // Arithmetic
    let a: u64 = 10;
    let b: u64 = 20;
    let sum: u64 = a + b;
    let diff: u64 = b - a;
    let prod: u64 = a * b;
    let quot: u64 = b / a;
    let rem: u64 = b % 3;

    // Comparison
    let is_eq: bool = (a == a);
    let is_gt: bool = (b > a);
    let is_lt: bool = (a < b);

    // Branching
    if b > 15 {
        log(1);  // taken branch
    } else {
        log(0);  // not taken
    }

    // Loop
    let mut counter: u64 = 5;
    while counter > 0 {
        counter = counter - 1;
    }

    // Memory: store and load
    let stored: u64 = 0xCAFE;
    // (store to memory, load back -- tested at bytecode level)

    // Logging
    log(sum);
    log(prod);

    // Return
    return;
}
