#![forbid(unsafe_code)]

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
//! remove the cap. As with `explicit_name`, cargo features unify across
//! the dependency graph, so a dependency can enable `unbounded` for the
//! whole build. Never derive names from untrusted input.
//!
//! With the `explicit_name` feature, names must first be authorized with
//! `allow_name`, and an empty allowlist rejects every new name. The
//! `glob_support` feature (which implies `explicit_name`) allows glob
//! patterns in the allowlist, so hierarchical names such as
//! `org.gnostr.wallet` can be authorized by registering `org.gnostr.*`.
//! Because cargo features unify across the dependency graph, a dependency
//! can enable `explicit_name` for a build that never opted in; the
//! rejection message explains how to authorize a name or opt back out.
//! The bundled `nested_chain`/`stress_chain` helpers use fixed `level_N`
//! names, so allow those (or a matching glob) when relying on them under
//! `explicit_name`. The `console` feature assumes the `tokio_unstable`
//! cfg — set `RUSTFLAGS="--cfg tokio_unstable"` as this repository's
//! `.cargo/config.toml` does for its examples. The `governance` feature
//! (which implies both filters) adds deny rules that override allows,
//! per-pattern quotas, registry introspection, and structured audit
//! events — see `deny_name`, `set_quota`, `list_runtimes`.
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

use std::collections::HashMap;
#[cfg(all(feature = "explicit_name", not(feature = "governance")))]
use std::collections::HashSet;
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
    NotAllowed {
        /// The rejected name.
        name: String,
        /// How many patterns were registered at the time of rejection.
        registered: usize,
    },
    /// The name matched a deny rule (deny wins over allow).
    #[cfg(feature = "governance")]
    Denied {
        /// The rejected name.
        name: String,
        /// The deny pattern that matched.
        by: String,
    },
    /// The matched allow pattern is at its runtime quota.
    #[cfg(feature = "governance")]
    QuotaExceeded {
        /// The quota-limited pattern.
        pattern: String,
        /// The configured maximum.
        max: usize,
    },
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
            Self::NotAllowed { name, registered } => {
                if *registered == 0 {
                    write!(
                        f,
                        "runtime name {name:?} is not allowed: the explicit_name filter is active but the allowlist is empty\n\
                         hint: authorize names with arduous_rt::allow_name(...) during startup, or remove the explicit_name feature\n\
                         note: cargo features unify across the dependency graph, so a dependency may have enabled explicit_name for this build"
                    )
                } else {
                    write!(
                        f,
                        "runtime name {name:?} is not allowed: it matches none of the {registered} registered patterns\n\
                         hint: authorize it with arduous_rt::allow_name({name:?}) or remove the explicit_name feature"
                    )
                }
            }
            #[cfg(feature = "governance")]
            Self::Denied { name, by } => write!(
                f,
                "runtime name {name:?} is denied by pattern {by:?}\n\
                 hint: remove the deny rule with arduous_rt::undeny_name({by:?}) or narrow it"
            ),
            #[cfg(feature = "governance")]
            Self::QuotaExceeded { pattern, max } => write!(
                f,
                "pattern {pattern:?} is at its quota of {max} runtimes\n\
                 hint: raise the quota with arduous_rt::set_quota, or reuse names under that pattern"
            ),
            Self::RegistryFull => write!(
                f,
                "runtime registry is full\n\
                 hint: enable the `unbounded` feature to lift the cap"
            ),
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
    #[cfg(all(feature = "explicit_name", not(feature = "governance")))]
    allowed: HashSet<String>,
    #[cfg(all(feature = "glob_support", not(feature = "governance")))]
    compiled: globset::GlobSet,
    // Governance stores patterns as ordered (pattern, compiled matcher)
    // pairs: order gives deterministic first-match semantics, and the
    // matcher identifies which pattern authorized a name (for quotas,
    // audit, and introspection).
    #[cfg(feature = "governance")]
    allowed: Vec<(String, globset::GlobSet)>,
    #[cfg(feature = "governance")]
    denied: Vec<(String, globset::GlobSet)>,
    #[cfg(feature = "governance")]
    quotas: HashMap<String, usize>,
    #[cfg(feature = "governance")]
    counts: HashMap<String, usize>,
}

static REGISTRY: LazyLock<Mutex<Registry>> = LazyLock::new(|| {
    Mutex::new(Registry {
        runtimes: HashMap::new(),
        #[cfg(all(feature = "explicit_name", not(feature = "governance")))]
        allowed: HashSet::new(),
        #[cfg(all(feature = "glob_support", not(feature = "governance")))]
        compiled: globset::GlobSet::empty(),
        #[cfg(feature = "governance")]
        allowed: Vec::new(),
        #[cfg(feature = "governance")]
        denied: Vec::new(),
        #[cfg(feature = "governance")]
        quotas: HashMap::new(),
        #[cfg(feature = "governance")]
        counts: HashMap::new(),
    })
});

#[cfg(not(feature = "unbounded"))]
fn check_capacity(len: usize) -> Result<(), NameError> {
    if len >= MAX_RUNTIMES {
        return Err(NameError::RegistryFull);
    }
    Ok(())
}

#[cfg(all(feature = "glob_support", not(feature = "governance")))]
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

/// Compile a pattern or reject it with `NameError::InvalidPattern`.
#[cfg(feature = "governance")]
fn validate_glob(pattern: &str) -> Result<globset::GlobSet, NameError> {
    let glob = globset::Glob::new(pattern)
        .map_err(|err| NameError::InvalidPattern(format!("{pattern}: {err}")))?;
    globset::GlobSetBuilder::new()
        .add(glob)
        .build()
        .map_err(|err| NameError::InvalidPattern(format!("{pattern}: {err}")))
}

/// First pattern that matches `name`: an exact pattern-string match or a
/// glob hit. Registration order makes matching deterministic.
#[cfg(feature = "governance")]
fn find_match<'a>(patterns: &'a [(String, globset::GlobSet)], name: &str) -> Option<&'a str> {
    patterns
        .iter()
        .find(|(p, g)| p == name || g.is_match(name))
        .map(|(p, _)| p.as_str())
}

/// Resolve `name` against the governance policy: deny first, then the
/// allowlist, then the matched pattern's quota. Returns the authorized
/// pattern on success.
#[cfg(feature = "governance")]
fn matched_pattern(reg: &Registry, name: &str) -> Result<String, NameError> {
    if let Some(by) = find_match(&reg.denied, name) {
        tracing::warn!(name, by, "governance: runtime creation denied");
        return Err(NameError::Denied {
            name: name.to_string(),
            by: by.to_string(),
        });
    }
    match find_match(&reg.allowed, name) {
        Some(pattern) => {
            if let Some(&max) = reg.quotas.get(pattern) {
                let current = reg.counts.get(pattern).copied().unwrap_or(0);
                if current >= max {
                    tracing::warn!(name, pattern, max, "governance: pattern quota exceeded");
                    return Err(NameError::QuotaExceeded {
                        pattern: pattern.to_string(),
                        max,
                    });
                }
            }
            Ok(pattern.to_string())
        }
        None => {
            tracing::warn!(
                name,
                registered = reg.allowed.len(),
                "governance: name not allowed"
            );
            Err(NameError::NotAllowed {
                name: name.to_string(),
                registered: reg.allowed.len(),
            })
        }
    }
}

/// Run the allowlist check for `name` against the registry.
/// (Governance builds on [`matched_pattern`] instead.)
#[cfg(all(feature = "explicit_name", not(feature = "governance")))]
fn check_allowed(reg: &Registry, name: &str) -> Result<(), NameError> {
    if reg.allowed.is_empty() {
        return Err(NameError::NotAllowed {
            name: name.to_string(),
            registered: 0,
        });
    }
    if !reg.allowed.contains(name) {
        #[cfg(feature = "glob_support")]
        if reg.compiled.is_match(name) {
            return Ok(());
        }
        return Err(NameError::NotAllowed {
            name: name.to_string(),
            registered: reg.allowed.len(),
        });
    }
    Ok(())
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
/// Drops a runtime that lost an insert race (or hit the cap after
/// creation). Dropping a runtime blocks on its worker threads, which
/// panics inside an async context, so hand it to a blocking thread
/// whenever a runtime context is available.
fn dispose_unused(rt: tokio::runtime::Runtime) {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn_blocking(move || drop(rt));
        }
        Err(_) => drop(rt),
    }
}

pub fn try_named_runtime(name: &str) -> Result<&'static tokio::runtime::Runtime, NameError> {
    // Poison-safe: a panic while holding the lock must not take down all
    // future lookups; the registry invariant survives a partial insert.
    let guard = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(rt) = guard.runtimes.get(name) {
        tracing::info!("named_runtime({name}) reused");
        return Ok(rt);
    }

    // Authorize under the lock. The governance pass also resolves the
    // matched pattern, which drives quota accounting on insert.
    #[cfg(feature = "governance")]
    matched_pattern(&guard, name)?;
    #[cfg(all(feature = "explicit_name", not(feature = "governance")))]
    check_allowed(&guard, name)?;
    #[cfg(not(feature = "unbounded"))]
    check_capacity(guard.runtimes.len())?;
    drop(guard);

    // Create outside the lock: thread spawning can take tens of
    // milliseconds and must not block other threads' lookups. A losing
    // racer drops its fully constructed but unused runtime; a name
    // disallowed or capped between the two lock sections simply fails
    // here instead (see allow_name's documented semantics).
    let rt = tokio::runtime::Runtime::new().unwrap();

    let mut guard = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(&existing) = guard.runtimes.get(name) {
        drop(guard);
        dispose_unused(rt);
        tracing::info!("named_runtime({name}) reused");
        return Ok(existing);
    }
    #[cfg(feature = "governance")]
    let matched = matched_pattern(&guard, name)?;
    #[cfg(not(feature = "unbounded"))]
    if let Err(err) = check_capacity(guard.runtimes.len()) {
        drop(guard);
        dispose_unused(rt);
        return Err(err);
    }
    let leaked: &'static tokio::runtime::Runtime = Box::leak(Box::new(rt));
    guard.runtimes.insert(name.to_string(), leaked);
    #[cfg(feature = "governance")]
    {
        let count = {
            let c = guard.counts.entry(matched.clone()).or_insert(0);
            *c += 1;
            *c
        };
        tracing::info!(name, matched, count, "named_runtime created");
    }
    #[cfg(not(feature = "governance"))]
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
    #[cfg(feature = "governance")]
    let glob = validate_glob(pattern)?;
    #[cfg(all(feature = "glob_support", not(feature = "governance")))]
    if let Err(err) = globset::Glob::new(pattern) {
        return Err(NameError::InvalidPattern(format!("{pattern}: {err}")));
    };
    let mut guard = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    #[cfg(feature = "governance")]
    {
        if guard.allowed.iter().any(|(p, _)| p == pattern) {
            return Ok(());
        }
        guard.allowed.push((pattern.to_string(), glob));
    }
    #[cfg(all(feature = "explicit_name", not(feature = "governance")))]
    {
        guard.allowed.insert(pattern.to_string());
        #[cfg(feature = "glob_support")]
        {
            guard.compiled = compile_allowed(&guard.allowed);
        }
    }
    Ok(())
}

/// Remove an allowlist entry. Returns whether the entry was registered.
///
/// Requires the `explicit_name` feature.
#[cfg(feature = "explicit_name")]
pub fn disallow_name(pattern: &str) -> bool {
    let mut guard = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    #[cfg(feature = "governance")]
    {
        let before = guard.allowed.len();
        guard.allowed.retain(|(p, _)| p != pattern);
        guard.allowed.len() < before
    }
    #[cfg(all(feature = "explicit_name", not(feature = "governance")))]
    {
        let removed = guard.allowed.remove(pattern);
        #[cfg(feature = "glob_support")]
        if removed {
            guard.compiled = compile_allowed(&guard.allowed);
        }
        removed
    }
}

/// Whether `name` would pass the allowlist (and, under `governance`,
/// the deny rules) right now. Quotas are not consulted.
///
/// Requires the `explicit_name` feature. An empty allowlist rejects all
/// names.
#[cfg(feature = "explicit_name")]
pub fn is_name_allowed(name: &str) -> bool {
    let guard = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    if guard.allowed.is_empty() {
        return false;
    }
    #[cfg(feature = "governance")]
    {
        find_match(&guard.denied, name).is_none() && find_match(&guard.allowed, name).is_some()
    }
    #[cfg(all(feature = "explicit_name", not(feature = "governance")))]
    {
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
}

/// Deny a name or glob pattern. Deny rules take precedence over every
/// allow rule, so `org.gnostr.*` plus `deny_name("org.gnostr.experimental.*")`
/// authorizes the subtree except the experimental branch.
///
/// Requires the `governance` feature. Deny governs new creations only;
/// runtimes created before the rule keep running.
///
/// # Errors
///
/// Returns `NameError::InvalidPattern` when `pattern` is not a valid glob.
#[cfg(feature = "governance")]
pub fn deny_name(pattern: &str) -> Result<(), NameError> {
    let glob = validate_glob(pattern)?;
    let mut guard = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    if guard.denied.iter().any(|(p, _)| p == pattern) {
        return Ok(());
    }
    guard.denied.push((pattern.to_string(), glob));
    Ok(())
}

/// Remove a deny rule. Returns whether it was registered.
///
/// Requires the `governance` feature.
#[cfg(feature = "governance")]
pub fn undeny_name(pattern: &str) -> bool {
    let mut guard = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    let before = guard.denied.len();
    guard.denied.retain(|(p, _)| p != pattern);
    guard.denied.len() < before
}

/// Cap how many runtimes may be created through `pattern`. Checked at
/// creation time; already-created runtimes are never reaped, and a quota
/// of 0 blocks new creations through the pattern.
///
/// Requires the `governance` feature.
///
/// # Errors
///
/// Returns `NameError::InvalidPattern` when `pattern` is not a valid glob.
#[cfg(feature = "governance")]
pub fn set_quota(pattern: &str, max: usize) -> Result<(), NameError> {
    validate_glob(pattern)?;
    let mut guard = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    guard.quotas.insert(pattern.to_string(), max);
    Ok(())
}

/// The registered allow patterns, in registration order.
///
/// Requires the `governance` feature.
#[cfg(feature = "governance")]
pub fn registered_patterns() -> Vec<String> {
    let guard = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    guard.allowed.iter().map(|(p, _)| p.clone()).collect()
}

/// One registered runtime, for introspection.
///
/// Requires the `governance` feature.
#[cfg(feature = "governance")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeInfo {
    /// The runtime's registry name.
    pub name: String,
    /// The allow pattern that would authorize this name today. `None` for
    /// runtimes whose authorizing pattern has since been removed.
    pub matched: Option<String>,
}

/// Every runtime in the registry, sorted by name for determinism.
///
/// Requires the `governance` feature.
#[cfg(feature = "governance")]
pub fn list_runtimes() -> Vec<RuntimeInfo> {
    let guard = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    let mut out: Vec<RuntimeInfo> = guard
        .runtimes
        .keys()
        .map(|name| RuntimeInfo {
            name: name.clone(),
            matched: find_match(&guard.allowed, name).map(str::to_string),
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
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
    fn filter_empty_allowlist_message_is_actionable() {
        let err = NameError::NotAllowed {
            name: "foo".into(),
            registered: 0,
        };
        let msg = err.to_string();
        assert!(msg.contains("allowlist is empty"));
        assert!(msg.contains("allow_name"));
        assert!(msg.contains("unify"));
    }

    #[cfg(feature = "explicit_name")]
    #[test]
    fn filter_rejects_unregistered_name() {
        allow_test_names();
        assert!(!is_name_allowed("filter_probe_unregistered_xyz"));
        assert!(matches!(
            try_named_runtime("filter_probe_unregistered_xyz"),
            Err(NameError::NotAllowed { .. })
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

    #[cfg(feature = "governance")]
    #[test]
    fn governance_deny_overrides_allow() {
        allow_test_names();
        allow_name("gov_deny.*").unwrap();
        deny_name("gov_deny.blocked.*").unwrap();
        assert!(try_named_runtime("gov_deny.ok").is_ok());
        assert!(matches!(
            try_named_runtime("gov_deny.blocked.x"),
            Err(NameError::Denied { .. })
        ));
        assert!(!is_name_allowed("gov_deny.blocked.x"));
        assert!(is_name_allowed("gov_deny.ok"));
        assert!(undeny_name("gov_deny.blocked.*"));
    }

    #[cfg(feature = "governance")]
    #[test]
    fn governance_quota_exceeded() {
        allow_test_names();
        allow_name("gov_quota.*").unwrap();
        set_quota("gov_quota.*", 2).unwrap();
        assert!(try_named_runtime("gov_quota.a").is_ok());
        assert!(try_named_runtime("gov_quota.b").is_ok());
        assert!(matches!(
            try_named_runtime("gov_quota.c"),
            Err(NameError::QuotaExceeded { max: 2, .. })
        ));
    }

    #[cfg(feature = "governance")]
    #[test]
    fn governance_introspection_lists_patterns_and_runtimes() {
        allow_test_names();
        allow_name("gov_intro.*").unwrap();
        try_named_runtime("gov_intro.x").unwrap();
        assert!(registered_patterns().iter().any(|p| p == "gov_intro.*"));
        let runtimes = list_runtimes();
        let info = runtimes
            .iter()
            .find(|r| r.name == "gov_intro.x")
            .expect("gov_intro.x must be listed");
        assert_eq!(info.matched.as_deref(), Some("gov_intro.*"));
        let names: Vec<&str> = runtimes.iter().map(|r| r.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted, "list_runtimes must be sorted by name");
    }

    #[cfg(feature = "governance")]
    #[test]
    fn governance_rejects_invalid_deny_glob() {
        allow_test_names();
        assert!(matches!(
            deny_name("gov.[bad"),
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
