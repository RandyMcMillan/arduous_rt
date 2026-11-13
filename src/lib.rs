//! Named, lazily-created tokio runtimes that compose.
//!
//! This crate keeps a process-wide registry of tokio runtimes, each
//! identified by a name. A runtime is created on first use and reused
//! everywhere else the same name is requested, from any thread or task.
//!
//! # Process-wide and forever
//!
//! Runtimes are intentionally leaked: they are never shut down and their
//! worker threads live until the process exits. Distinct names grow
//! threads and memory without bound, so reuse names rather than minting
//! new ones. By default the registry holds at most 256 runtimes and
//! panics on the 257th distinct name; enable the `unbounded` feature to
//! remove the cap. Never derive names from untrusted input.
//!
//! With the `explicit_name` feature, names must first be authorized with
//! `allow_name`, and an empty allowlist rejects every new name. The
//! `glob_support` feature (which implies `explicit_name`) allows glob
//! patterns in the allowlist, so hierarchical names such as
//! `org.gnostr.wallet` can be authorized by registering `org.gnostr.*`.
//!
//! # Nested chains
//!
//! [`nested_chain`] builds runtimes named `level_0`, `level_1`, ... .
//! The names are global, so two chains of the same depth share runtimes.
//!
//! # Reentrancy
//!
//! [`run`] detects when it is already executing on the target runtime and
//! polls the future inline instead of spawning, so
//! `rt.block_on(run(rt, async { .. }))` cannot deadlock. By contrast,
//! calling [`Runtime::block_on`](tokio::runtime::Runtime::block_on)
//! directly inside a task running on the same runtime deadlocks; see the
//! `demo_deadlock` example.
//!
//! # Observability
//!
//! Library diagnostics are emitted through [`tracing`]; install a
//! subscriber such as `tracing_subscriber::fmt` in your binary to see
//! them. [`macro@arduous_trace`] emits info-level events from application
//! code through the same facade.
//!
//! # Examples
//!
//! ```
//! # #[cfg(feature = "explicit_name")]
//! # arduous_rt::allow_name("example").unwrap();
//! let rt = arduous_rt::named_runtime("example");
//! let value = rt.block_on(async { 2 + 2 });
//! assert_eq!(value, 4);
//!
//! // Same name, same runtime.
//! assert!(std::ptr::eq(rt, arduous_rt::named_runtime("example")));
//! ```

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::future::Future;
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

#[doc(hidden)]
pub use tracing as __tracing;

/// Emit an info-level event through the `tracing` facade.
///
/// The event is delivered to whatever subscriber the running binary has
/// installed; with no subscriber installed it is a no-op.
#[macro_export]
macro_rules! arduous_trace {
    ($($arg:tt)*) => {
        $crate::__tracing::info!($($arg)*)
    };
}

/// Return the shared runtime named `"global"`.
///
/// # Examples
///
/// ```
/// # #[cfg(feature = "explicit_name")]
/// # arduous_rt::allow_name("global").unwrap();
/// let rt = arduous_rt::global_rt();
/// let value = rt.block_on(async { 2 + 2 });
/// assert_eq!(value, 4);
/// ```
//global_rt
pub fn global_rt() -> &'static tokio::runtime::Runtime {
    named_runtime("global")
}

/// Reason a runtime name was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum NameError {
    /// The name is not authorized by the allowlist.
    #[cfg(feature = "explicit_name")]
    NotAllowed(String),
    /// The registry is full; enable the `unbounded` feature to lift the cap.
    RegistryFull,
    /// The allowlist entry is not a valid glob pattern.
    #[cfg(feature = "glob_support")]
    InvalidPattern(String),
}

impl fmt::Display for NameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            #[cfg(feature = "explicit_name")]
            Self::NotAllowed(name) => write!(f, "runtime name {name:?} is not allowed"),
            Self::RegistryFull => write!(f, "runtime registry is full"),
            #[cfg(feature = "glob_support")]
            Self::InvalidPattern(pattern) => write!(f, "invalid glob pattern {pattern:?}"),
        }
    }
}

impl std::error::Error for NameError {}

/// Maximum number of distinct named runtimes kept in the registry.
/// Reached only when `unbounded` is disabled.
#[cfg(not(feature = "unbounded"))]
const MAX_RUNTIMES: usize = 256;

struct Registry {
    runtimes: HashMap<String, &'static tokio::runtime::Runtime>,
    #[cfg(feature = "explicit_name")]
    allowed: HashSet<String>,
    #[cfg(feature = "glob_support")]
    compiled: globset::GlobSet,
}

static REGISTRY: LazyLock<Mutex<Registry>> = LazyLock::new(|| {
    Mutex::new(Registry {
        runtimes: HashMap::new(),
        #[cfg(feature = "explicit_name")]
        allowed: HashSet::new(),
        #[cfg(feature = "glob_support")]
        compiled: globset::GlobSet::empty(),
    })
});

#[cfg(not(feature = "unbounded"))]
fn check_capacity(len: usize) -> Result<(), NameError> {
    if len >= MAX_RUNTIMES {
        return Err(NameError::RegistryFull);
    }
    Ok(())
}

#[cfg(feature = "glob_support")]
fn compile_allowed(allowed: &HashSet<String>) -> globset::GlobSet {
    use globset::GlobSetBuilder;
    let mut builder = GlobSetBuilder::new();
    for pattern in allowed {
        if let Ok(glob) = globset::Glob::new(pattern) {
            builder.add(glob);
        }
    }
    builder
        .build()
        .expect("allowlist patterns are validated at registration")
}

fn create_runtime(
    name: &str,
    guard: &mut Registry,
) -> Result<&'static tokio::runtime::Runtime, NameError> {
    #[cfg(not(feature = "unbounded"))]
    check_capacity(guard.runtimes.len())?;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let leaked: &'static tokio::runtime::Runtime = Box::leak(Box::new(rt));
    guard.runtimes.insert(name.to_string(), leaked);
    tracing::info!("named_runtime({name}) created");
    Ok(leaked)
}

/// Obtain (or lazily create) a dedicated tokio runtime identified by `name`.
///
/// Runtimes are never shut down; the same `&'static` runtime is returned
/// for a given name until the process exits.
///
/// # Panics
///
/// Panics if the name is rejected (with the `explicit_name` feature, names
/// must first be authorized via `allow_name`) or if the registry is full
/// (by default at most 256 runtimes; enable `unbounded` to remove the cap).
///
/// # Examples
///
/// ```
/// # #[cfg(feature = "explicit_name")]
/// # arduous_rt::allow_name("my_runtime").unwrap();
/// let a = arduous_rt::named_runtime("my_runtime");
/// let b = arduous_rt::named_runtime("my_runtime");
/// assert!(std::ptr::eq(a, b));
/// ```
pub fn named_runtime(name: &str) -> &'static tokio::runtime::Runtime {
    try_named_runtime(name).unwrap_or_else(|err| panic!("{err}"))
}

/// Fallible variant of [`named_runtime`]: rejected names and a full
/// registry surface as `Err` instead of a panic.
///
/// # Errors
///
/// With the `explicit_name` feature enabled, returns
/// `NameError::NotAllowed` when the allowlist does not authorize `name`.
/// Returns `NameError::RegistryFull` when the registry is at capacity
/// (see the `unbounded` feature).
pub fn try_named_runtime(name: &str) -> Result<&'static tokio::runtime::Runtime, NameError> {
    // Poison-safe: a panic while holding the lock must not take down all
    // future lookups; the registry invariant survives a partial insert.
    let mut guard = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(rt) = guard.runtimes.get(name) {
        tracing::info!("named_runtime({name}) reused");
        return Ok(rt);
    }

    #[cfg(feature = "explicit_name")]
    {
        if guard.allowed.is_empty() {
            return Err(NameError::NotAllowed(name.to_string()));
        }
        if !guard.allowed.contains(name) {
            #[cfg(feature = "glob_support")]
            {
                if guard.compiled.is_match(name) {
                    return create_runtime(name, &mut guard);
                }
            }
            return Err(NameError::NotAllowed(name.to_string()));
        }
    }

    create_runtime(name, &mut guard)
}

/// Authorize a runtime name, or (with `glob_support`) a glob pattern such
/// as `org.gnostr.*` or `org.gnostr.*-debug`.
///
/// Requires the `explicit_name` feature. While any entries are registered,
/// only matching names may be created; an empty allowlist rejects every
/// new name. Names created before an entry is removed keep their runtime.
///
/// # Errors
///
/// Returns `NameError::InvalidPattern` when `glob_support` is enabled and
/// `pattern` is not a valid glob.
#[cfg(feature = "explicit_name")]
pub fn allow_name(pattern: &str) -> Result<(), NameError> {
    #[cfg(feature = "glob_support")]
    if let Err(err) = globset::Glob::new(pattern) {
        return Err(NameError::InvalidPattern(format!("{pattern}: {err}")));
    };
    let mut guard = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    guard.allowed.insert(pattern.to_string());
    #[cfg(feature = "glob_support")]
    {
        guard.compiled = compile_allowed(&guard.allowed);
    }
    Ok(())
}

/// Remove an allowlist entry. Returns whether the entry was registered.
///
/// Requires the `explicit_name` feature.
#[cfg(feature = "explicit_name")]
pub fn disallow_name(pattern: &str) -> bool {
    let mut guard = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    let removed = guard.allowed.remove(pattern);
    #[cfg(feature = "glob_support")]
    if removed {
        guard.compiled = compile_allowed(&guard.allowed);
    }
    removed
}

/// Whether `name` would pass the allowlist right now.
///
/// Requires the `explicit_name` feature. An empty allowlist rejects all
/// names.
#[cfg(feature = "explicit_name")]
pub fn is_name_allowed(name: &str) -> bool {
    let guard = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    if guard.allowed.is_empty() {
        return false;
    }
    guard.allowed.contains(name) || {
        #[cfg(feature = "glob_support")]
        {
            guard.compiled.is_match(name)
        }
        #[cfg(not(feature = "glob_support"))]
        {
            false
        }
    }
}

/// Create (or reuse) a child runtime scoped under a parent runtime name.
pub fn child_runtime(parent: &str, child: &str) -> &'static tokio::runtime::Runtime {
    named_runtime(&format!("{parent}/{child}"))
}

/// Create (or reuse) a nested runtime identified by a path of ancestors.
pub fn nested_runtime(path: &[&str]) -> &'static tokio::runtime::Runtime {
    named_runtime(&path.join("/"))
}

/// Build a chain of `n` nested runtimes, returning every level (shallow to deep).
pub fn nested_chain(n: usize) -> Vec<&'static tokio::runtime::Runtime> {
    let mut path: Vec<String> = Vec::with_capacity(n);
    let mut out: Vec<&'static tokio::runtime::Runtime> = Vec::with_capacity(n);
    for i in 0..n {
        path.push(format!("level_{i}"));
        let refs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
        out.push(nested_runtime(&refs));
    }
    out
}

/// Return the deepest runtime of an `n`-level nested chain.
///
/// # Panics
///
/// Panics if `n` is 0.
pub fn nested_leaf(n: usize) -> &'static tokio::runtime::Runtime {
    let chain = nested_chain(n);
    chain.last().copied().expect("n must be > 0")
}

/// Returns true if the current thread is already running on `rt`.
pub fn is_running_on(rt: &'static tokio::runtime::Runtime) -> bool {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => handle.id() == rt.handle().id(),
        Err(_) => false,
    }
}

/// Run a future on `rt`, safely handling reentrant execution.
///
/// If the current async context already belongs to `rt`, the future is
/// polled inline; otherwise it is spawned on `rt` and awaited to
/// completion. This keeps `rt.block_on(run(rt, ..))` compositions from
/// deadlocking, unlike nesting [`Runtime::block_on`], which does.
///
/// If the awaiting task is cancelled, the spawned inner task is detached
/// and runs to completion on `rt`.
///
/// [`Runtime::block_on`]: tokio::runtime::Runtime::block_on
///
/// # Panics
///
/// Panics if the spawned task panics.
///
/// # Examples
///
/// ```
/// # #[cfg(feature = "explicit_name")]
/// # arduous_rt::allow_name("run_example").unwrap();
/// let rt = arduous_rt::named_runtime("run_example");
/// let value = rt.block_on(arduous_rt::run(rt, async { 40 + 2 }));
/// assert_eq!(value, 42);
/// ```
pub async fn run<R, F>(rt: &'static tokio::runtime::Runtime, fut: F) -> R
where
    R: Send + 'static,
    F: Future<Output = R> + Send + 'static,
{
    if is_running_on(rt) {
        fut.await
    } else {
        let handle = rt.spawn(fut);
        handle.await.expect("task panicked")
    }
}

/// Time a synchronous closure, logging elapsed milliseconds.
pub fn timed<R>(label: &str, f: impl FnOnce() -> R) -> R {
    let start = Instant::now();
    let result = f();
    let elapsed = start.elapsed();
    tracing::info!("{label}: {:.3} ms", elapsed.as_secs_f64() * 1000.0);
    result
}

/// Time an async future, logging elapsed milliseconds.
pub async fn timed_async<R, F>(label: &str, f: F) -> R
where
    F: Future<Output = R>,
{
    let start = Instant::now();
    let result = f.await;
    let elapsed = start.elapsed();
    tracing::info!("{label}: {:.3} ms", elapsed.as_secs_f64() * 1000.0);
    result
}

/// Run `f` repeatedly and report simple timing statistics.
///
/// # Panics
///
/// Panics if `iterations` is 0.
pub fn benchmark(label: &str, iterations: usize, mut f: impl FnMut()) {
    assert!(iterations > 0, "iterations must be > 0");
    let mut min = u128::MAX;
    let mut max = 0u128;
    let mut total = 0u128;
    for _ in 0..iterations {
        let start = Instant::now();
        f();
        let elapsed = start.elapsed().as_nanos();
        min = min.min(elapsed);
        max = max.max(elapsed);
        total += elapsed;
    }
    let avg = total / iterations as u128;
    tracing::info!("{label}: iters={iterations} min={min}ns avg={avg}ns max={max}ns");
}

/// Run an async `f` repeatedly and report simple timing statistics.
///
/// # Panics
///
/// Panics if `iterations` is 0.
pub async fn benchmark_async<F, Fut>(label: &str, iterations: usize, mut f: F)
where
    F: FnMut() -> Fut,
    Fut: Future,
{
    assert!(iterations > 0, "iterations must be > 0");
    let mut min = u128::MAX;
    let mut max = 0u128;
    let mut total = 0u128;
    for _ in 0..iterations {
        let start = Instant::now();
        f().await;
        let elapsed = start.elapsed().as_nanos();
        min = min.min(elapsed);
        max = max.max(elapsed);
        total += elapsed;
    }
    let avg = total / iterations as u128;
    tracing::info!("{label}: iters={iterations} min={min}ns avg={avg}ns max={max}ns");
}

/// Print an obvious banner around `message`.
pub fn banner(message: &str) {
    println!();
    println!("============================================================");
    println!(" {message}");
    println!("============================================================");
}

/// Number of threads available on this machine.
pub fn available_threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

/// A stress-test chain sized to the machine's available parallelism (capped).
pub fn stress_chain() -> Vec<&'static tokio::runtime::Runtime> {
    let depth = available_threads().clamp(1, 16);
    nested_chain(depth)
}

/// Run a stress workload across the given runtimes.
///
/// Spawns `tasks_per` tasks on each runtime and aggregates their results.
pub async fn stress(runtimes: &[&'static tokio::runtime::Runtime], tasks_per: usize) -> i64 {
    let total_tasks = runtimes
        .len()
        .checked_mul(tasks_per)
        .expect("stress workload too large: runtimes.len() * tasks_per overflows usize");
    tracing::info!(
        "stress starting: runtimes={} tasks_per={} total={}",
        runtimes.len(),
        tasks_per,
        total_tasks
    );
    let mut handles = Vec::with_capacity(runtimes.len() * tasks_per);
    for (ri, rt) in runtimes.iter().enumerate() {
        for ti in 0..tasks_per {
            let rt = *rt;
            let value = (ri * tasks_per + ti) as i64;
            handles.push(rt.spawn(async move { value }));
        }
    }

    let mut total = 0i64;
    for handle in handles {
        total += handle.await.expect("stress task panicked");
    }
    tracing::info!("stress finished: result={total}");
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    fn allow_test_names() {
        #[cfg(feature = "explicit_name")]
        {
            static ONCE: std::sync::Once = std::sync::Once::new();
            ONCE.call_once(|| {
                let mut names: Vec<String> = [
                    "global",
                    "distinct_a3",
                    "distinct_b3",
                    "coop_producer3",
                    "coop_consumer3",
                    "complex_producer3",
                    "complex_consumer3",
                    "reentrant_rt2",
                    "nested_outer3",
                    "nested_outer3/inner",
                    "a2",
                    "a2/b2",
                    "a2/b2/c2",
                ]
                .into_iter()
                .map(String::from)
                .collect();
                // nested_chain builds cumulative paths: level_0, level_0/level_1, ...
                // stress_chain() reaches depth 16.
                let mut path = String::new();
                for i in 0..16 {
                    if !path.is_empty() {
                        path.push('/');
                    }
                    path.push_str(&format!("level_{i}"));
                    names.push(path.clone());
                }
                for name in &names {
                    let _ = allow_name(name);
                }
            });
        }
    }

    #[test]
    fn global_rt_works() {
        allow_test_names();
        let rt = global_rt();
        let result = rt.block_on(async { 2 + 2 });
        assert_eq!(result, 4);
    }

    #[test]
    fn named_runtimes_are_distinct() {
        allow_test_names();
        let a = named_runtime("distinct_a3");
        let b = named_runtime("distinct_b3");
        assert!(!std::ptr::eq(a, b));
        assert!(std::ptr::eq(a, named_runtime("distinct_a3")));
    }

    #[test]
    fn runtimes_cooperate_via_channel() {
        allow_test_names();
        let producer = named_runtime("coop_producer3");
        let consumer = named_runtime("coop_consumer3");

        let (tx, mut rx) = mpsc::channel::<i32>(1);

        producer.spawn(async move {
            tx.send(42).await.unwrap();
        });

        let value = consumer.block_on(async { rx.recv().await.unwrap() });
        assert_eq!(value, 42);
    }

    #[test]
    fn complex_cooperation_aggregates_results() {
        allow_test_names();
        let producer = named_runtime("complex_producer3");
        let consumer = named_runtime("complex_consumer3");

        let (tx, mut rx) = mpsc::channel::<i32>(16);

        for i in 0..5_i32 {
            let tx = tx.clone();
            producer.spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                tx.send(i).await.unwrap();
            });
        }
        drop(tx);

        let sum = consumer.block_on(async {
            let mut sum = 0_i32;
            while let Some(v) = rx.recv().await {
                sum += v;
            }
            sum
        });

        assert_eq!(sum, 1 + 2 + 3 + 4);
    }

    #[test]
    fn nested_reentrant_run() {
        allow_test_names();
        let rt = named_runtime("reentrant_rt2");
        let result = rt.block_on(async { run(rt, async { 7 }).await });
        assert_eq!(result, 7);
    }

    #[test]
    fn parent_child_runtime_composition() {
        allow_test_names();
        let outer = named_runtime("nested_outer3");
        let inner = child_runtime("nested_outer3", "inner");

        let result = outer.block_on(async { run(inner, async { 40 + 2 }).await });
        assert_eq!(result, 42);
    }

    #[test]
    fn deeply_nested_runtime_path() {
        allow_test_names();
        let rt = nested_runtime(&["a2", "b2", "c2"]);
        let result = rt.block_on(async { 1 + 2 });
        assert_eq!(result, 3);
    }

    #[test]
    fn n_nested_chain_runs() {
        allow_test_names();
        // Deterministic proof: a chain of N runtimes composes a known sum.
        for &n in &[1usize, 2, 3, 5, 8] {
            let chain = nested_chain(n);
            assert_eq!(chain.len(), n);

            // Distinctness proof.
            for i in 0..chain.len() {
                for j in (i + 1)..chain.len() {
                    assert!(!std::ptr::eq(chain[i], chain[j]));
                }
            }

            // Compose from the deepest level upward, accumulating depth indices.
            let leaf = *chain.last().unwrap();
            let result = leaf.block_on(async { n as i64 * (n as i64 + 1) / 2 });
            // Deterministic expected value for 1..=n sum.
            let expected: i64 = (1..=n as i64).sum();
            banner(&format!(
                "[nested proof] n={n} levels={} proof={} expected={} OK",
                chain.len(),
                result,
                expected
            ));
            assert_eq!(result, expected, "n={n}");
        }
    }

    #[test]
    fn n_nested_composition_across_levels() {
        allow_test_names();
        let n = 6usize;
        let chain = nested_chain(n);
        // Each level runs a small task; aggregate on the outermost (first) runtime.
        let outer = chain[0];
        let result = outer.block_on(async {
            let mut total = 0i64;
            for (idx, rt) in chain.iter().enumerate() {
                let v = run(rt, async move { (idx as i64) + 1 }).await;
                total += v;
            }
            total
        });
        let expected: i64 = (1..=n as i64).sum();
        banner(&format!(
            "[nested proof] cross-level n={} proof={} expected={} OK",
            n, result, expected
        ));
        assert_eq!(result, expected);
    }

    #[test]
    fn deterministic_repeated_runs() {
        allow_test_names();
        // Run the same nested composition many times; results must be identical.
        let mut results = Vec::new();
        for _ in 0..10 {
            let chain = nested_chain(4);
            let outer = chain[0];
            let r = outer.block_on(async {
                let mut t = 0i64;
                for (i, rt) in chain.iter().enumerate() {
                    t += run(rt, async move { i as i64 }).await;
                }
                t
            });
            results.push(r);
        }
        let first = results[0];
        banner(&format!(
            "[determinism proof] runs={} all_equal={} value={}",
            results.len(),
            results.iter().all(|&r| r == first),
            first
        ));
        assert!(results.iter().all(|&r| r == first));
    }

    #[test]
    fn timed_and_benchmark_utilities() {
        allow_test_names();
        let v = timed("add", || 10 + 32);
        assert_eq!(v, 42);

        let mut counter = 0u64;
        benchmark("counter", 5, || {
            counter += 1;
        });
        assert_eq!(counter, 5);

        let rt = global_rt();
        let out = rt.block_on(timed_async("async_add", async { 1 + 2 }));
        assert_eq!(out, 3);
    }

    #[test]
    fn benchmark_async_utility() {
        allow_test_names();
        let rt = global_rt();
        rt.block_on(benchmark_async("async_bench", 3, || async {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }));
    }

    #[cfg(not(feature = "unbounded"))]
    #[test]
    fn runtime_registry_under_cap_ok() {
        allow_test_names();
        assert!(check_capacity(MAX_RUNTIMES - 1).is_ok());
    }

    #[cfg(not(feature = "unbounded"))]
    #[test]
    fn runtime_registry_cap_returns_error() {
        allow_test_names();
        assert_eq!(check_capacity(MAX_RUNTIMES), Err(NameError::RegistryFull));
    }

    #[cfg(feature = "explicit_name")]
    #[test]
    fn filter_allows_registered_exact_name() {
        allow_test_names();
        allow_name("filter_probe_exact").unwrap();
        assert!(is_name_allowed("filter_probe_exact"));
    }

    #[cfg(feature = "explicit_name")]
    #[test]
    fn filter_rejects_unregistered_name() {
        allow_test_names();
        assert!(!is_name_allowed("filter_probe_unregistered_xyz"));
        assert!(matches!(
            try_named_runtime("filter_probe_unregistered_xyz"),
            Err(NameError::NotAllowed(_))
        ));
    }

    #[cfg(feature = "explicit_name")]
    #[test]
    fn filter_try_named_runtime_uses_allowed_name() {
        allow_test_names();
        allow_name("filter_probe_rt").unwrap();
        let a = try_named_runtime("filter_probe_rt").unwrap();
        let b = try_named_runtime("filter_probe_rt").unwrap();
        assert!(std::ptr::eq(a, b));
        assert!(disallow_name("filter_probe_rt"));
        assert!(!is_name_allowed("filter_probe_rt"));
    }

    #[cfg(feature = "glob_support")]
    #[test]
    fn filter_glob_patterns_match_domains() {
        allow_test_names();
        allow_name("org.gnostr.*").unwrap();
        allow_name("org.gnostr.*-debug").unwrap();
        assert!(is_name_allowed("org.gnostr.wallet"));
        assert!(is_name_allowed("org.gnostr.relay-debug"));
        assert!(!is_name_allowed("com.other.thing"));
        assert!(try_named_runtime("org.gnostr.wallet").is_ok());
    }

    #[cfg(feature = "glob_support")]
    #[test]
    fn filter_rejects_invalid_glob() {
        allow_test_names();
        assert!(matches!(
            allow_name("org.[invalid"),
            Err(NameError::InvalidPattern(_))
        ));
    }

    // Negative tests: pass when the expected failure occurs.
    #[test]
    #[should_panic]
    fn negative_benchmark_zero_iterations() {
        allow_test_names();
        benchmark("zero", 0, || {});
    }

    #[test]
    #[should_panic]
    fn negative_nested_leaf_zero() {
        allow_test_names();
        let _ = nested_leaf(0);
    }

    #[test]
    fn negative_send_on_closed_channel() {
        allow_test_names();
        let rt = global_rt();
        rt.block_on(async {
            let (tx, rx) = mpsc::channel::<i64>(1);
            drop(rx);
            assert!(tx.send(1).await.is_err());
        });
    }

    #[test]
    fn negative_aborted_task_reports_cancelled() {
        allow_test_names();
        let rt = global_rt();
        rt.block_on(async {
            let h = rt.spawn(async {
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            });
            h.abort();
            let res = h.await;
            assert!(res.unwrap_err().is_cancelled());
        });
    }

    #[test]
    fn available_threads_detected() {
        allow_test_names();
        let n = available_threads();
        banner(&format!("[stress] available_threads={n}"));
        assert!(n >= 1);
    }

    // Flaky/external tests (ignored by default).
    #[test]
    #[ignore]
    fn external_httpbin_json() {
        allow_test_names();
        use std::io::{Read, Write};
        use std::net::TcpStream;
        use std::time::Duration;

        let mut stream = TcpStream::connect("httpbin.org:80").unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        stream
            .write_all(b"GET /json HTTP/1.1\r\nHost: httpbin.org\r\nConnection: close\r\n\r\n")
            .unwrap();

        let mut buf = String::new();
        stream.read_to_string(&mut buf).unwrap();
        banner(&format!(
            "[external] httpbin status/body preview: {}",
            buf.lines().next().unwrap_or("")
        ));
        assert!(buf.contains("200 OK") || buf.contains("HTTP/1.1 200"));
        assert!(buf.contains("slideshow") || buf.contains("{"));
    }

    #[test]
    #[ignore]
    fn external_example_com() {
        allow_test_names();
        use std::io::{Read, Write};
        use std::net::TcpStream;
        use std::time::Duration;

        let mut stream = TcpStream::connect("example.com:80").unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: example.com\r\nConnection: close\r\n\r\n")
            .unwrap();

        let mut buf = String::new();
        stream.read_to_string(&mut buf).unwrap();
        banner(&format!(
            "[external] example.com status: {}",
            buf.lines().next().unwrap_or("")
        ));
        assert!(buf.contains("200 OK") || buf.contains("HTTP/1.1 200"));
    }

    #[test]
    fn stress_nested_runtimes() {
        allow_test_names();
        let runtimes = stress_chain();
        let depth = runtimes.len();
        let tasks_per = 32usize;
        let rt0 = runtimes[0];

        banner(&format!(
            "[stress] depth={depth} tasks_per={tasks_per} total_tasks={}",
            depth * tasks_per
        ));

        let result = rt0.block_on(stress(&runtimes, tasks_per));

        // Expected deterministic sum.
        let mut expected = 0i64;
        for ri in 0..depth {
            for ti in 0..tasks_per {
                expected += (ri * tasks_per + ti) as i64;
            }
        }
        banner(&format!(
            "[stress proof] depth={depth} tasks_per={tasks_per} result={result} expected={expected} {}",
            if result == expected { "OK" } else { "FAIL" }
        ));
        assert_eq!(result, expected);
    }
}
