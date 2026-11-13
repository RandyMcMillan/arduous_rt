//! STUB — deferred work (concern #4, README "Design notes").
//!
//! What this will be: a loom model-check of the `named_runtime`
//! create-outside-the-lock / insert-if-absent race.
//!
//! TODO:
//! - add `loom = "0.7"` to `[dev-dependencies]`
//! - refactor `Registry` in `src/lib.rs` for injectable sync primitives
//!   (`#[cfg(loom)] use loom::sync::Mutex` in place of `std::sync`) and a
//!   fake runtime factory so models never spawn real tokio threads
//! - add a CI job running `RUSTFLAGS="--cfg loom" cargo test --test
//!   registry_race_loom`
//!
//! Invariants the model must prove:
//! - N threads racing `try_named_runtime("x")` all observe the same
//!   runtime pointer (exactly one winner)
//! - registry length never exceeds `MAX_RUNTIMES` under racing creators
//! - a name rejected by the allowlist never appears in the registry
//!
//! Revisit trigger: the registry gains more state (per-name locks,
//! refcounting, shutdown) or becomes load-bearing in a long-running
//! service. The concurrency-identity integration test
//! (`arduous_idempotent_named_runtime`) covers this probabilistically in
//! the meantime.

fn main() {
    println!(
        "stub: loom model for the registry race is not implemented yet; see TODOs in examples/deferred_loom_registry.rs"
    );
}
