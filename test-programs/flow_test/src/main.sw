script;

use std::logging::log;

// ---------------------------------------------------------------------------
// Structs
// ---------------------------------------------------------------------------

/// A 2D point with integer coordinates.
struct Point {
    x: u64,
    y: u64,
}

/// A rectangle defined by its origin point, width, and height.
struct Rectangle {
    origin: Point,
    width: u64,
    height: u64,
}

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

/// A simple Option-like enum for wrapping optional u64 values.
enum MaybeU64 {
    Some: u64,
    None: (),
}

/// Cardinal directions for pattern-matching tests.
enum Direction {
    North: (),
    South: (),
    East: (),
    West: (),
}

// ---------------------------------------------------------------------------
// Pure arithmetic helpers
// ---------------------------------------------------------------------------

/// Add two u64 values.
fn add(a: u64, b: u64) -> u64 {
    a + b
}

/// Multiply two u64 values.
fn multiply(a: u64, b: u64) -> u64 {
    a * b
}

/// Compute the area of a rectangle.
fn rect_area(r: Rectangle) -> u64 {
    multiply(r.width, r.height)
}

/// Compute the Manhattan distance from the origin (0,0) to a point.
fn manhattan_distance(p: Point) -> u64 {
    add(p.x, p.y)
}

// ---------------------------------------------------------------------------
// Nested / chained function calls
// ---------------------------------------------------------------------------

/// Apply a two-step transformation: multiply then add an offset.
fn transform(value: u64, factor: u64, offset: u64) -> u64 {
    let scaled: u64 = multiply(value, factor);
    add(scaled, offset)
}

/// Double a value via multiply.
fn double(n: u64) -> u64 {
    multiply(n, 2)
}

// ---------------------------------------------------------------------------
// Enum / pattern matching helpers
// ---------------------------------------------------------------------------

/// Unwrap a MaybeU64, returning the inner value or a provided default.
fn unwrap_or(opt: MaybeU64, default: u64) -> u64 {
    match opt {
        MaybeU64::Some(val) => val,
        MaybeU64::None => default,
    }
}

/// Map a Direction to a numeric code for testing pattern match results.
///   North => 1, South => 2, East => 3, West => 4
fn direction_code(dir: Direction) -> u64 {
    match dir {
        Direction::North => 1,
        Direction::South => 2,
        Direction::East => 3,
        Direction::West => 4,
    }
}

// ---------------------------------------------------------------------------
// Conditional helpers
// ---------------------------------------------------------------------------

/// Return the larger of two u64 values.
fn max_u64(a: u64, b: u64) -> u64 {
    if a > b { a } else { b }
}

/// Absolute difference between two u64 values.
fn abs_diff(a: u64, b: u64) -> u64 {
    if a >= b {
        a - b
    } else {
        b - a
    }
}

/// Classify a number into a category string-code:
///   0       => 0 (zero)
///   1..=10  => 1 (small)
///   11..=99 => 2 (medium)
///   100+    => 3 (large)
fn classify(n: u64) -> u64 {
    if n == 0 {
        0
    } else if n <= 10 {
        1
    } else if n <= 99 {
        2
    } else {
        3
    }
}

// ---------------------------------------------------------------------------
// Iterative algorithms
// ---------------------------------------------------------------------------

/// Compute the n-th Fibonacci number iteratively.
///   fib(0) = 0, fib(1) = 1, fib(n) = fib(n-1) + fib(n-2)
fn fibonacci(n: u64) -> u64 {
    if n == 0 {
        return 0;
    }
    if n == 1 {
        return 1;
    }

    let mut prev: u64 = 0;
    let mut curr: u64 = 1;
    let mut i: u64 = 2;
    while i <= n {
        let next: u64 = prev + curr;
        prev = curr;
        curr = next;
        i = i + 1;
    }
    curr
}

/// Sum integers from 1 to n using a while loop.
fn sum_to(n: u64) -> u64 {
    let mut total: u64 = 0;
    let mut counter: u64 = 1;
    while counter <= n {
        total = total + counter;
        counter = counter + 1;
    }
    total
}

/// Compute n factorial iteratively.
fn factorial(n: u64) -> u64 {
    let mut result: u64 = 1;
    let mut i: u64 = 2;
    while i <= n {
        result = result * i;
        i = i + 1;
    }
    result
}

// ---------------------------------------------------------------------------
// Bitwise helpers
// ---------------------------------------------------------------------------

/// Combine two u32-range values into a single u64 by shifting the high part
/// left by 32 bits and OR-ing with the low part.
fn pack_u32_pair(high: u64, low: u64) -> u64 {
    let shifted: u64 = high << 32;
    shifted | low
}

/// Check whether a specific bit (0-indexed from LSB) is set.
fn is_bit_set(value: u64, bit: u64) -> bool {
    let mask: u64 = 1 << bit;
    (value & mask) != 0
}

// ---------------------------------------------------------------------------
// Entry point — exercises every construct above
// ---------------------------------------------------------------------------
fn main() {
    // --- Section 1: Basic arithmetic (breakpoint target: line ~202) ---
    let a: u64 = 10;
    let b: u64 = 32;
    let sum_val: u64 = add(a, b);           // 42
    let doubled: u64 = double(sum_val);      // 84
    let final_result: u64 = add(doubled, a); // 94
    log(final_result);

    // --- Section 2: Struct creation and field access ---
    let origin: Point = Point { x: 3, y: 7 };
    let rect: Rectangle = Rectangle {
        origin: Point { x: 1, y: 2 },
        width: 10,
        height: 5,
    };
    let area: u64 = rect_area(rect);              // 50
    let dist: u64 = manhattan_distance(origin);    // 10
    let struct_sum: u64 = add(area, dist);         // 60
    log(struct_sum);

    // --- Section 3: Enum and pattern matching ---
    let some_val: MaybeU64 = MaybeU64::Some(99);
    let none_val: MaybeU64 = MaybeU64::None;
    let unwrapped_some: u64 = unwrap_or(some_val, 0);   // 99
    let unwrapped_none: u64 = unwrap_or(none_val, 42);  // 42
    let dir: Direction = Direction::East;
    let dir_code: u64 = direction_code(dir);             // 3
    let match_sum: u64 = add(unwrapped_some, dir_code);  // 102
    log(match_sum);

    // --- Section 4: While loop — sum 1..10 and factorial ---
    let loop_sum: u64 = sum_to(10);       // 55
    let fact_val: u64 = factorial(6);      // 720
    let loop_product: u64 = add(loop_sum, fact_val); // 775
    log(loop_product);

    // --- Section 5: Nested / chained function calls ---
    let transformed: u64 = transform(5, 3, 7); // 5*3 + 7 = 22
    let nested_result: u64 = add(
        double(transformed),                     // 44
        multiply(a, b),                          // 320
    );                                           // 364
    log(nested_result);

    // --- Section 6: Array and tuple operations ---
    let arr: [u64; 5] = [10, 20, 30, 40, 50];
    let arr_first: u64 = arr[0];                 // 10
    let arr_last: u64 = arr[4];                  // 50
    let arr_sum: u64 = add(arr_first, arr_last); // 60

    let tup: (u64, bool, u64) = (42, true, 99);
    let tup_first: u64 = tup.0;                 // 42
    let tup_third: u64 = tup.2;                 // 99
    let tup_sum: u64 = add(tup_first, tup_third); // 141
    log(tup_sum);

    // --- Section 7: Fibonacci computation ---
    let fib_10: u64 = fibonacci(10);             // 55
    let fib_20: u64 = fibonacci(20);             // 6765
    let fib_sum: u64 = add(fib_10, fib_20);      // 6820
    log(fib_sum);

    // --- Section 8: Conditionals and classification ---
    let max_ab: u64 = max_u64(a, b);             // 32
    let diff_ab: u64 = abs_diff(a, b);           // 22
    let class_zero: u64 = classify(0);           // 0
    let class_small: u64 = classify(5);          // 1
    let class_medium: u64 = classify(50);        // 2
    let class_large: u64 = classify(100);        // 3
    let class_sum: u64 = class_zero + class_small + class_medium + class_large; // 6
    log(class_sum);

    // --- Section 9: Bitwise operations ---
    let packed: u64 = pack_u32_pair(1, 255);     // (1 << 32) | 255 = 4294967551
    let bit_0_set: bool = is_bit_set(packed, 0); // true  (255 has bit 0)
    let bit_8_set: bool = is_bit_set(packed, 8); // false (255 < 256)
    let bit_32_set: bool = is_bit_set(packed, 32); // true (high part = 1)
    log(packed);

    // --- Final summary value (useful as a sentinel breakpoint) ---
    let grand_total: u64 = final_result
        + struct_sum
        + match_sum
        + loop_product
        + nested_result
        + arr_sum
        + tup_sum
        + fib_sum
        + class_sum
        + max_ab
        + diff_ab;
    log(grand_total);
}
