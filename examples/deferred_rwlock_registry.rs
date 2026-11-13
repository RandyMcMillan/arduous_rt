//! STUB — deferred work (concern #5, README "Design notes").
//!
//! What this will be: an `RwLock` lookup path for the runtime registry.
//!
//! TODO:
//! - swap `Mutex<Registry>` for `RwLock<Registry>` in `src/lib.rs`
//! - fast path: `read()` for the reuse lookup; `write()` only on insert
//! - `std::sync::RwLock` cannot upgrade in place: keep today's two-pass
//!   shape — read-check, create unlocked, re-lock exclusive,
//!   insert-if-absent — so `arduous_idempotent_named_runtime` keeps
//!   covering the race
//! - decide via profile whether `RwLock`, `DashMap`, or sharded locks
//!   fit the measured access pattern
//!
//! Revisit trigger: a profile shows the registry lock hot, or a real
//! consumer reports lookup contention. Do not implement preemptively —
//! the current critical section is a ~50ns `HashMap` lookup.

fn main() {
    println!(
        "stub: RwLock registry lookup path is not implemented yet; see TODOs in examples/deferred_rwlock_registry.rs"
    );
}
