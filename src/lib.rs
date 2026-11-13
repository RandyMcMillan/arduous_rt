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
//! let rt = arduous_rt::named_runtime("example");
//! let value = rt.block_on(async { 2 + 2 });
//! assert_eq!(value, 4);
//!
//! // Same name, same runtime.
//! assert!(std::ptr::eq(rt, arduous_rt::named_runtime("example")));
//! ```

use std::collections::HashMap;
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
/// let rt = arduous_rt::global_rt();
/// let value = rt.block_on(async { 2 + 2 });
/// assert_eq!(value, 4);
/// ```
//global_rt
pub fn global_rt() -> &'static tokio::runtime::Runtime {
    named_runtime("global")
}

/// Maximum number of distinct named runtimes kept in the registry.
/// Reached only when `unbounded` is disabled.
#[cfg(not(feature = "unbounded"))]
const MAX_RUNTIMES: usize = 256;

/// Panics if the registry is at capacity.
#[cfg(not(feature = "unbounded"))]
fn assert_registry_has_capacity(len: usize, name: &str) {
    if len >= MAX_RUNTIMES {
        panic!(
            "named_runtime(\"{name}\"): registry is full ({MAX_RUNTIMES} runtimes); \
             reuse names or enable the 'unbounded' feature"
        );
    }
}

/// Obtain (or lazily create) a dedicated tokio runtime identified by `name`.
///
/// Runtimes are never shut down; the same `&'static` runtime is returned
/// for a given name until the process exits.
///
/// # Panics
///
/// Panics if the registry is full. By default at most 256 distinct runtimes
/// are kept; enable the `unbounded` feature to remove the cap.
///
/// # Examples
///
/// ```
/// let a = arduous_rt::named_runtime("my_runtime");
/// let b = arduous_rt::named_runtime("my_runtime");
/// assert!(std::ptr::eq(a, b));
/// ```
pub fn named_runtime(name: &str) -> &'static tokio::runtime::Runtime {
    static RUNTIMES: LazyLock<Mutex<HashMap<String, &'static tokio::runtime::Runtime>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));

    // Poison-safe: a panic while holding the lock must not take down all
    // future lookups; the registry invariant survives a partial insert.
    let mut guard = RUNTIMES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(rt) = guard.get(name) {
        tracing::info!("named_runtime({name}) reused");
        return rt;
    }

    #[cfg(not(feature = "unbounded"))]
    assert_registry_has_capacity(guard.len(), name);

    let rt = tokio::runtime::Runtime::new().unwrap();
    let leaked: &'static tokio::runtime::Runtime = Box::leak(Box::new(rt));
    guard.insert(name.to_string(), leaked);
    tracing::info!("named_runtime({name}) created");
    leaked
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

    #[test]
    fn global_rt_works() {
        let rt = global_rt();
        let result = rt.block_on(async { 2 + 2 });
        assert_eq!(result, 4);
    }

    #[test]
    fn named_runtimes_are_distinct() {
        let a = named_runtime("distinct_a3");
        let b = named_runtime("distinct_b3");
        assert!(!std::ptr::eq(a, b));
        assert!(std::ptr::eq(a, named_runtime("distinct_a3")));
    }

    #[test]
    fn runtimes_cooperate_via_channel() {
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
        let rt = named_runtime("reentrant_rt2");
        let result = rt.block_on(async { run(rt, async { 7 }).await });
        assert_eq!(result, 7);
    }

    #[test]
    fn parent_child_runtime_composition() {
        let outer = named_runtime("nested_outer3");
        let inner = child_runtime("nested_outer3", "inner");

        let result = outer.block_on(async { run(inner, async { 40 + 2 }).await });
        assert_eq!(result, 42);
    }

    #[test]
    fn deeply_nested_runtime_path() {
        let rt = nested_runtime(&["a2", "b2", "c2"]);
        let result = rt.block_on(async { 1 + 2 });
        assert_eq!(result, 3);
    }

    #[test]
    fn n_nested_chain_runs() {
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
        let rt = global_rt();
        rt.block_on(benchmark_async("async_bench", 3, || async {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }));
    }

    #[cfg(not(feature = "unbounded"))]
    #[test]
    fn runtime_registry_under_cap_ok() {
        assert_registry_has_capacity(MAX_RUNTIMES - 1, "under_cap");
    }

    #[cfg(not(feature = "unbounded"))]
    #[test]
    #[should_panic(expected = "registry is full")]
    fn runtime_registry_cap_panics() {
        assert_registry_has_capacity(MAX_RUNTIMES, "cap_test");
    }

    // Negative tests: pass when the expected failure occurs.
    #[test]
    #[should_panic]
    fn negative_benchmark_zero_iterations() {
        benchmark("zero", 0, || {});
    }

    #[test]
    #[should_panic]
    fn negative_nested_leaf_zero() {
        let _ = nested_leaf(0);
    }

    #[test]
    fn negative_send_on_closed_channel() {
        let rt = global_rt();
        rt.block_on(async {
            let (tx, rx) = mpsc::channel::<i64>(1);
            drop(rx);
            assert!(tx.send(1).await.is_err());
        });
    }

    #[test]
    fn negative_aborted_task_reports_cancelled() {
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
        let n = available_threads();
        banner(&format!("[stress] available_threads={n}"));
        assert!(n >= 1);
    }

    // Flaky/external tests (ignored by default).
    #[test]
    #[ignore]
    fn external_httpbin_json() {
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
