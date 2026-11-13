//! Nested composition through the reentrancy-safe `run()`: the global
//! runtime runs a future that hops onto a child runtime and back — no
//! `block_on` nesting, no deadlock.
//!
//! Run:
//! cargo run --example nested_runtimes
//!
//! Expected: completes; prints "nested result: 42".

use arduous_rt::{child_runtime, global_rt, run};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    let outer = global_rt();
    let inner = child_runtime("global", "inner");

    // Nested composition: run work on an inner runtime from within the outer one.
    let result = run(outer, async move { run(inner, async { 20 + 22 }).await }).await;

    println!("nested result: {result}");
}
