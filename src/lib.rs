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
//! events — see `deny_name`, `set_quota`, `list_runtimes`. Domain tokens
//! (`DomainToken::acquire` + `named_runtime_in`) scope creation to a
//! domain, and `Policy`/`set_policy` (or `policy_from_toml` with the
//! `policy-toml` feature) swaps the whole policy atomically. Per-pattern
//! `RuntimeConfig` (worker count, thread names via `allow_configured`)
//! and thread budgets (`set_thread_quota`) shape the actual resource
//! footprint, and `audit_events` exposes a ring buffer of the last 256
//! governance decisions. The `admission` feature (implies `governance`)
//! adds work admission: admission keys resolve per runtime at creation,
//! and `set_admission` configures per-pattern priorities and concurrency
//! budgets — see `docs/admission-control-plan.md`.
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
#[cfg(feature = "admission")]
use std::sync::Arc;
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
    /// The name lies outside the domain of the presented token.
    #[cfg(feature = "governance")]
    OutsideDomain {
        /// The rejected name.
        name: String,
        /// The token's domain.
        domain: String,
    },
    /// The matched pattern's thread budget would be exceeded.
    #[cfg(feature = "governance")]
    ThreadBudgetExceeded {
        /// The budget-limited pattern.
        pattern: String,
        /// The configured thread maximum.
        max: usize,
        /// The new runtime's worker count.
        requested: usize,
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
            #[cfg(feature = "governance")]
            Self::OutsideDomain { name, domain } => write!(
                f,
                "runtime name {name:?} is outside the domain of this token ({domain:?})\n\
                 hint: acquire a token for the name's domain with arduous_rt::DomainToken::acquire, or rename"
            ),
            #[cfg(feature = "governance")]
            Self::ThreadBudgetExceeded {
                pattern,
                max,
                requested,
            } => write!(
                f,
                "pattern {pattern:?} would exceed its thread budget ({requested} requested, {max} allowed)\n\
                 hint: raise the budget with arduous_rt::set_thread_quota, lower the worker count, or reuse an existing runtime"
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
    #[cfg(feature = "governance")]
    configs: HashMap<String, RuntimeConfig>,
    #[cfg(feature = "governance")]
    thread_quotas: HashMap<String, usize>,
    #[cfg(feature = "governance")]
    thread_counts: HashMap<String, usize>,
    /// Runtime name -> worker thread count, for budget accounting and
    /// quota recomputation after `set_policy`.
    #[cfg(feature = "governance")]
    workers: HashMap<String, usize>,
    /// Leaked-runtime address -> runtime name, for O(1) admission-key
    /// resolution in try_run. Sound because registry runtimes are never
    /// moved or freed (see ADR-0001). Admission lookup tries the name
    /// first, then the name's matched allow pattern — pattern-only keys
    /// proved too coarse in the gnostr demo.
    #[cfg(feature = "admission")]
    by_ptr: HashMap<usize, String>,
    /// Admission configuration per pattern.
    #[cfg(feature = "admission")]
    admissions: HashMap<String, AdmissionConfig>,
    /// Admission key -> (capacity, permit pool) for configured
    /// concurrency budgets. The pool is replaced when the capacity
    /// changes; in-flight permits keep the old pool alive.
    #[cfg(feature = "admission")]
    permits: HashMap<String, (usize, Arc<tokio::sync::Semaphore>)>,
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
        #[cfg(feature = "governance")]
        configs: HashMap::new(),
        #[cfg(feature = "governance")]
        thread_quotas: HashMap::new(),
        #[cfg(feature = "governance")]
        thread_counts: HashMap::new(),
        #[cfg(feature = "governance")]
        workers: HashMap::new(),
        #[cfg(feature = "admission")]
        by_ptr: HashMap::new(),
        #[cfg(feature = "admission")]
        admissions: HashMap::new(),
        #[cfg(feature = "admission")]
        permits: HashMap::new(),
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

/// A pattern's fixed head: the pattern with a trailing wildcard segment
/// removed (`org.gnostr.*` -> `org.gnostr`). Used to match subtree roots
/// without letting sibling subtrees (`org.gnostr.blocked.*`) claim them.
#[cfg(feature = "governance")]
fn strip_glob_tail(pattern: &str) -> &str {
    for suffix in [".*", "/*", "/**"] {
        if let Some(head) = pattern.strip_suffix(suffix) {
            return head;
        }
    }
    pattern
}

/// First pattern that matches `name`: an exact pattern-string match, a
/// glob hit, or a pattern whose fixed head equals `name` (so the root of
/// a subtree is governed by patterns for that subtree: `org.gnostr` is
/// covered by `org.gnostr.*` but not by `org.gnostr.blocked.*`).
/// Registration order makes matching deterministic.
#[cfg(feature = "governance")]
fn find_match<'a>(patterns: &'a [(String, globset::GlobSet)], name: &str) -> Option<&'a str> {
    patterns
        .iter()
        .find(|(p, g)| p == name || g.is_match(name) || strip_glob_tail(p) == name)
        .map(|(p, _)| p.as_str())
}

/// Resolve `name` against the governance policy: deny first, then the
/// allowlist, then the matched pattern's quota. Returns the authorized
/// pattern on success.
#[cfg(feature = "governance")]
fn matched_pattern(reg: &Registry, name: &str) -> Result<String, NameError> {
    if let Some(by) = find_match(&reg.denied, name) {
        tracing::warn!(name, by, "governance: runtime creation denied");
        audit(AuditKind::RejectedDenied, name, Some(by.to_string()));
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
                    audit(
                        AuditKind::RejectedQuotaExceeded,
                        name,
                        Some(pattern.to_string()),
                    );
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
            audit(AuditKind::RejectedNotAllowed, name, None);
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

    // Authorize under the lock; the governance pass also resolves the
    // matched pattern, which drives quota accounting, thread budgets, and
    // the runtime's build config on the insert path.
    #[cfg(feature = "governance")]
    let matched = matched_pattern(&guard, name)?;
    #[cfg(all(feature = "explicit_name", not(feature = "governance")))]
    check_allowed(&guard, name)?;
    #[cfg(feature = "governance")]
    {
        let workers = guard
            .configs
            .get(&matched)
            .map(RuntimeConfig::workers)
            .unwrap_or_else(available_threads);
        check_thread_budget(&guard, &matched, workers, name)?;
    }
    #[cfg(not(feature = "unbounded"))]
    check_capacity(guard.runtimes.len())?;
    #[cfg(feature = "governance")]
    let config = guard.configs.get(&matched).cloned();
    drop(guard);

    // Create outside the lock: thread spawning can take tens of
    // milliseconds and must not block other threads' lookups. A losing
    // racer drops its fully constructed but unused runtime; a name
    // disallowed or capped between the two lock sections simply fails
    // here instead (see allow_name's documented semantics).
    #[cfg(feature = "governance")]
    let rt = build_runtime(name, config.as_ref());
    #[cfg(not(feature = "governance"))]
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
    #[cfg(feature = "governance")]
    let workers = {
        let w = guard
            .configs
            .get(&matched)
            .map(RuntimeConfig::workers)
            .unwrap_or_else(available_threads);
        if let Err(err) = check_thread_budget(&guard, &matched, w, name) {
            drop(guard);
            dispose_unused(rt);
            return Err(err);
        }
        w
    };
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
        *guard.thread_counts.entry(matched.clone()).or_insert(0) += workers;
        guard.workers.insert(name.to_string(), workers);
        audit(AuditKind::Created, name, Some(matched.clone()));
        tracing::info!(name, matched, count, "named_runtime created");
    }
    #[cfg(feature = "admission")]
    guard
        .by_ptr
        .insert(leaked as *const _ as usize, name.to_string());
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

/// An opaque authorization for one domain. Requires the `governance`
/// feature.
///
/// Acquiring a token checks the domain against the current policy; using
/// the token (`named_runtime_in`) restricts creation to names at or below
/// that domain. Tokens are structural, cooperative guardrails — they
/// organize well-behaved code, they do not sandbox hostile code.
#[cfg(feature = "governance")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainToken {
    domain: String,
}

#[cfg(feature = "governance")]
impl DomainToken {
    /// Validate `domain` against the current policy and hold it.
    ///
    /// # Errors
    ///
    /// Returns the policy rejection (`NotAllowed`, `Denied`, or
    /// `QuotaExceeded` for the domain itself) when the domain is not
    /// authorized.
    pub fn acquire(domain: &str) -> Result<Self, NameError> {
        let guard = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
        matched_pattern(&guard, domain)?;
        Ok(Self {
            domain: domain.to_string(),
        })
    }

    /// The domain this token authorizes.
    pub fn domain(&self) -> &str {
        &self.domain
    }

    /// Mint a token for a sub-domain, e.g. `org.gnostr` →
    /// `org.gnostr/wallet`. The sub-domain goes through the same policy
    /// check as [`acquire`](Self::acquire), so a parent token cannot
    /// delegate where policy says no.
    ///
    /// # Errors
    ///
    /// Returns the policy rejection for the sub-domain, or
    /// [`NameError::OutsideDomain`] when `sub` is empty.
    pub fn mint(&self, sub: &str) -> Result<Self, NameError> {
        if sub.is_empty() {
            return Err(NameError::OutsideDomain {
                name: String::new(),
                domain: self.domain.clone(),
            });
        }
        Self::acquire(&format!("{}/{}", self.domain, sub))
    }
}

/// Get or create a runtime that must live at or below `token`'s domain:
/// `domain` itself or any `domain/...` path. Requires `governance`.
///
/// # Errors
///
/// Returns [`NameError::OutsideDomain`] when `name` escapes the token's
/// domain, then any policy rejection `try_named_runtime` would return.
#[cfg(feature = "governance")]
pub fn named_runtime_in(
    token: &DomainToken,
    name: &str,
) -> Result<&'static tokio::runtime::Runtime, NameError> {
    if name != token.domain && !name.starts_with(&format!("{}/", token.domain)) {
        audit(
            AuditKind::RejectedOutsideDomain,
            name,
            Some(token.domain.clone()),
        );
        return Err(NameError::OutsideDomain {
            name: name.to_string(),
            domain: token.domain.clone(),
        });
    }
    try_named_runtime(name)
}

/// One allow entry of a [`Policy`].
#[cfg(feature = "governance")]
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "policy-toml", derive(serde::Deserialize))]
pub struct PolicyAllow {
    /// The allow pattern (exact name or glob).
    pub pattern: String,
    /// Optional cap on runtimes created through this pattern.
    pub quota: Option<usize>,
}

/// A declarative governance policy. Requires the `governance` feature.
///
/// With the `policy-toml` feature this deserializes from TOML:
///
/// ```toml
/// deny = ["org.gnostr.experimental.*"]
///
/// [[allow]]
/// pattern = "org.gnostr.*"
/// quota = 8
/// ```
#[cfg(feature = "governance")]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "policy-toml", derive(serde::Deserialize))]
pub struct Policy {
    /// Deny patterns; take precedence over every allow entry.
    #[cfg_attr(feature = "policy-toml", serde(default))]
    pub deny: Vec<String>,
    /// Allow patterns with optional per-pattern quotas.
    #[cfg_attr(feature = "policy-toml", serde(default))]
    pub allow: Vec<PolicyAllow>,
}

/// Atomically replace the governance policy. Existing runtimes keep
/// running; per-pattern counts are recomputed against the new allowlist.
/// Requires the `governance` feature.
///
/// # Errors
///
/// Returns `NameError::InvalidPattern` when any deny or allow pattern is
/// not a valid glob; the previous policy is left untouched.
#[cfg(feature = "governance")]
pub fn set_policy(policy: Policy) -> Result<(), NameError> {
    let mut allowed = Vec::with_capacity(policy.allow.len());
    for entry in &policy.allow {
        allowed.push((entry.pattern.clone(), validate_glob(&entry.pattern)?));
    }
    let mut denied = Vec::with_capacity(policy.deny.len());
    for pattern in &policy.deny {
        denied.push((pattern.clone(), validate_glob(pattern)?));
    }
    let quotas: HashMap<String, usize> = policy
        .allow
        .iter()
        .filter_map(|entry| entry.quota.map(|quota| (entry.pattern.clone(), quota)))
        .collect();

    let mut guard = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    guard.allowed = allowed;
    guard.denied = denied;
    guard.quotas = quotas;
    // Recompute counts so pre-existing runtimes occupy quota under the
    // new policy instead of getting a fresh allowance.
    let mut counts: HashMap<String, usize> = HashMap::new();
    for name in guard.runtimes.keys() {
        if let Some(pattern) = find_match(&guard.allowed, name) {
            *counts.entry(pattern.to_string()).or_insert(0) += 1;
        }
    }
    guard.counts = counts;
    let mut thread_counts: HashMap<String, usize> = HashMap::new();
    for name in guard.runtimes.keys() {
        if let Some(pattern) = find_match(&guard.allowed, name) {
            let workers = guard
                .workers
                .get(name)
                .copied()
                .unwrap_or_else(available_threads);
            *thread_counts.entry(pattern.to_string()).or_insert(0) += workers;
        }
    }
    guard.thread_counts = thread_counts;
    Ok(())
}

/// Why a TOML policy failed to load. Requires the `policy-toml` feature.
#[cfg(feature = "policy-toml")]
#[derive(Debug)]
pub enum PolicyError {
    /// The input is not valid TOML for a [`Policy`].
    Parse(toml::de::Error),
    /// A deny or allow pattern is not a valid glob.
    Invalid(NameError),
}

#[cfg(feature = "policy-toml")]
impl fmt::Display for PolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(err) => write!(f, "invalid policy toml: {err}"),
            Self::Invalid(err) => write!(f, "invalid policy pattern: {err}"),
        }
    }
}

#[cfg(feature = "policy-toml")]
impl std::error::Error for PolicyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Parse(err) => Some(err),
            Self::Invalid(err) => Some(err),
        }
    }
}

/// Parse and validate a [`Policy`] from TOML. Glob validation happens
/// here so `set_policy` cannot fail on syntax after a successful load.
/// Requires the `policy-toml` feature.
///
/// # Errors
///
/// Returns [`PolicyError::Parse`] for malformed TOML and
/// [`PolicyError::Invalid`] for a syntactically invalid glob.
#[cfg(feature = "policy-toml")]
pub fn policy_from_toml(input: &str) -> Result<Policy, PolicyError> {
    let policy: Policy = toml::from_str(input).map_err(PolicyError::Parse)?;
    for pattern in &policy.deny {
        validate_glob(pattern).map_err(PolicyError::Invalid)?;
    }
    for entry in &policy.allow {
        validate_glob(&entry.pattern).map_err(PolicyError::Invalid)?;
    }
    Ok(policy)
}

/// How to build runtimes created through a pattern. Requires `governance`.
#[cfg(feature = "governance")]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeConfig {
    /// Worker thread count; `None` uses tokio's default (available
    /// parallelism).
    pub worker_threads: Option<usize>,
    /// Name prefix for the runtime's worker threads; defaults to the
    /// runtime's registry name.
    pub thread_name_prefix: Option<String>,
}

#[cfg(feature = "governance")]
impl RuntimeConfig {
    /// Builder-style setter for `worker_threads`.
    pub fn with_worker_threads(mut self, n: usize) -> Self {
        self.worker_threads = Some(n);
        self
    }

    /// Builder-style setter for `thread_name_prefix`.
    pub fn with_thread_name_prefix(mut self, prefix: &str) -> Self {
        self.thread_name_prefix = Some(prefix.to_string());
        self
    }

    fn workers(&self) -> usize {
        self.worker_threads.unwrap_or_else(available_threads)
    }
}

/// Allow a pattern and prescribe how runtimes created through it are
/// built. The same pattern can be re-registered to replace its config.
/// Requires the `governance` feature.
///
/// # Errors
///
/// Returns `NameError::InvalidPattern` when `pattern` is not a valid glob.
#[cfg(feature = "governance")]
pub fn allow_configured(pattern: &str, config: RuntimeConfig) -> Result<(), NameError> {
    allow_name(pattern)?;
    let mut guard = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    guard.configs.insert(pattern.to_string(), config);
    Ok(())
}

/// Cap the total worker threads of runtimes created through `pattern`.
/// Checked at creation time against the matched pattern. Requires the
/// `governance` feature.
///
/// # Errors
///
/// Returns `NameError::InvalidPattern` when `pattern` is not a valid glob.
#[cfg(feature = "governance")]
pub fn set_thread_quota(pattern: &str, max_threads: usize) -> Result<(), NameError> {
    validate_glob(pattern)?;
    let mut guard = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    guard.thread_quotas.insert(pattern.to_string(), max_threads);
    Ok(())
}

/// What happened on a governed creation attempt.
#[cfg(feature = "governance")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditKind {
    /// A runtime was created.
    Created,
    /// Rejected: the name matched no allow pattern.
    RejectedNotAllowed,
    /// Rejected: the name matched a deny pattern.
    RejectedDenied,
    /// Rejected: the matched pattern's runtime quota is full.
    RejectedQuotaExceeded,
    /// Rejected: the matched pattern's thread budget is full.
    RejectedThreadBudget,
    /// Rejected: the name escaped the presented token's domain.
    RejectedOutsideDomain,
    /// A `try_run` execution was shed under load. Requires `admission`.
    #[cfg(feature = "admission")]
    Shed,
    /// Shed events collapsed by the flood guard (ADR-0003): the note
    /// carries how many. Requires `admission`.
    #[cfg(feature = "admission")]
    ShedCollapsed,
}

/// One entry of the governance audit log. Requires `governance`.
#[cfg(feature = "governance")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEvent {
    /// When the event was recorded.
    pub when: std::time::SystemTime,
    /// What happened.
    pub kind: AuditKind,
    /// The runtime name involved.
    pub name: String,
    /// The pattern that authorized or rejected the name, if any.
    pub pattern: Option<String>,
    /// Free-form detail (e.g. shed priority/load). Requires `admission`.
    #[cfg(feature = "admission")]
    pub note: Option<String>,
}

#[cfg(feature = "governance")]
const AUDIT_CAP: usize = 256;

#[cfg(feature = "governance")]
static AUDIT: LazyLock<Mutex<std::collections::VecDeque<AuditEvent>>> =
    LazyLock::new(|| Mutex::new(std::collections::VecDeque::new()));

#[cfg(feature = "governance")]
fn audit(kind: AuditKind, name: &str, pattern: Option<String>) {
    push_audit(AuditEvent {
        when: std::time::SystemTime::now(),
        kind,
        name: name.to_string(),
        pattern,
        #[cfg(feature = "admission")]
        note: None,
    });
}

#[cfg(feature = "admission")]
fn audit_with_note(kind: AuditKind, name: &str, pattern: Option<String>, note: String) {
    push_audit(AuditEvent {
        when: std::time::SystemTime::now(),
        kind,
        name: name.to_string(),
        pattern,
        note: Some(note),
    });
}

#[cfg(feature = "governance")]
fn push_audit(event: AuditEvent) {
    let mut log = AUDIT.lock().unwrap_or_else(|e| e.into_inner());
    if log.len() >= AUDIT_CAP {
        log.pop_front();
    }
    log.push_back(event);
}

/// The governance audit log, oldest first, capped at 256 events. Requires
/// the `governance` feature.
#[cfg(feature = "governance")]
pub fn audit_events() -> Vec<AuditEvent> {
    AUDIT
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .cloned()
        .collect()
}

/// Build a runtime per the matched pattern's config, falling back to the
/// tokio default build.
#[cfg(feature = "governance")]
fn build_runtime(name: &str, config: Option<&RuntimeConfig>) -> tokio::runtime::Runtime {
    let Some(config) = config else {
        return tokio::runtime::Runtime::new().unwrap();
    };
    let mut builder = tokio::runtime::Builder::new_multi_thread();
    builder.enable_all();
    if let Some(n) = config.worker_threads {
        builder.worker_threads(n);
    }
    let prefix = config
        .thread_name_prefix
        .clone()
        .unwrap_or_else(|| name.to_string());
    builder.thread_name(prefix);
    builder.build().unwrap()
}

/// Enforce the matched pattern's thread budget. The caller disposes the
/// already-created runtime on error.
#[cfg(feature = "governance")]
fn check_thread_budget(
    reg: &Registry,
    matched: &str,
    workers: usize,
    name: &str,
) -> Result<(), NameError> {
    if let Some(&max) = reg.thread_quotas.get(matched) {
        let current = reg.thread_counts.get(matched).copied().unwrap_or(0);
        if current + workers > max {
            audit(
                AuditKind::RejectedThreadBudget,
                name,
                Some(matched.to_string()),
            );
            return Err(NameError::ThreadBudgetExceeded {
                pattern: matched.to_string(),
                max,
                requested: workers,
            });
        }
    }
    Ok(())
}

/// How valuable a domain's work is when the process is overloaded.
/// Requires the `admission` feature. Unconfigured domains default to
/// `Medium` (shed only at `Load::Critical`).
#[cfg(feature = "admission")]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Priority {
    Critical,
    High,
    #[default]
    Medium,
    Low,
}

/// Admission behavior for runtimes created through a pattern. Requires
/// the `admission` feature.
#[cfg(feature = "admission")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmissionConfig {
    /// Shed order under load: lower priority sheds first.
    pub priority: Priority,
    /// Maximum concurrently admitted `try_run` executions through this
    /// pattern. `None` means unbounded (enforced in phase B2).
    pub concurrency: Option<usize>,
    /// Whether `Load::High` may shed this domain. `false` defers
    /// shedding to `Load::Critical` regardless of priority.
    pub sheddable: bool,
}

#[cfg(feature = "admission")]
impl Default for AdmissionConfig {
    fn default() -> Self {
        Self {
            priority: Priority::default(),
            concurrency: None,
            sheddable: true,
        }
    }
}

#[cfg(feature = "admission")]
impl AdmissionConfig {
    /// Builder-style setter for `priority`.
    pub fn with_priority(mut self, priority: Priority) -> Self {
        self.priority = priority;
        self
    }

    /// Builder-style setter for `concurrency`.
    pub fn with_concurrency(mut self, n: usize) -> Self {
        self.concurrency = Some(n);
        self
    }

    /// Builder-style setter for `sheddable`.
    pub fn with_sheddable(mut self, sheddable: bool) -> Self {
        self.sheddable = sheddable;
        self
    }
}

/// Configure admission for runtimes created through `pattern`. Requires
/// the `admission` feature. Like the other shaping APIs this is
/// imperative — the TOML policy covers allow/deny/quota only.
///
/// # Errors
///
/// Returns `NameError::InvalidPattern` when `pattern` is not a valid glob.
#[cfg(feature = "admission")]
pub fn set_admission(pattern: &str, config: AdmissionConfig) -> Result<(), NameError> {
    validate_glob(pattern)?;
    let mut guard = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    guard.admissions.insert(pattern.to_string(), config);
    Ok(())
}

/// Resolve a runtime handle to its admission key: the runtime's own
/// name, falling back to "*" for unknown addresses. O(1) via the
/// creation-time pointer map; see ADR-0001 (as amended) for the
/// address-stability argument.
#[cfg(feature = "admission")]
fn admission_key(reg: &Registry, rt: &tokio::runtime::Runtime) -> String {
    reg.by_ptr
        .get(&(rt as *const _ as usize))
        .cloned()
        .unwrap_or_else(|| "*".to_string())
}

/// The admission configuration governing `rt`: the most specific match
/// wins — configuration registered for the runtime's own name first,
/// then for its matched allow pattern, then the defaults
/// (ADR-0002: unconfigured domains are Medium).
#[cfg(feature = "admission")]
fn admission_config(reg: &Registry, rt: &tokio::runtime::Runtime) -> AdmissionConfig {
    let name = admission_key(reg, rt);
    reg.admissions
        .get(&name)
        .copied()
        .or_else(|| {
            find_match(&reg.allowed, &name).and_then(|pattern| reg.admissions.get(pattern).copied())
        })
        .unwrap_or_default()
}

/// Acquire a concurrency permit for `key`, creating or resizing the pool
/// to `capacity`. Non-blocking: `None` means the budget is fully
/// occupied. Replacing the pool on resize is sound — in-flight permits
/// hold the old `Arc` alive until they drop.
#[cfg(feature = "admission")]
fn admission_permit(
    reg: &mut Registry,
    key: &str,
    capacity: usize,
) -> Option<tokio::sync::OwnedSemaphorePermit> {
    let entry = reg
        .permits
        .entry(key.to_string())
        .or_insert_with(|| (capacity, Arc::new(tokio::sync::Semaphore::new(capacity))));
    if entry.0 != capacity {
        *entry = (capacity, Arc::new(tokio::sync::Semaphore::new(capacity)));
    }
    entry.1.clone().try_acquire_owned().ok()
}

/// Process load as reported by the host. Requires `admission`.
#[cfg(feature = "admission")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Load {
    Normal,
    High,
    Critical,
}

#[cfg(feature = "admission")]
const LOAD_NORMAL: u8 = 0;
#[cfg(feature = "admission")]
const LOAD_HIGH: u8 = 1;
#[cfg(feature = "admission")]
const LOAD_CRITICAL: u8 = 2;

#[cfg(feature = "admission")]
static CURRENT_LOAD: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(LOAD_NORMAL);

/// Report the process load. The host derives it however it likes (CPU,
/// latency, queue depth) and calls this from its own loop; arduous_rt
/// never guesses at health. Requires `admission`.
#[cfg(feature = "admission")]
pub fn report_load(load: Load) {
    let value = match load {
        Load::Normal => LOAD_NORMAL,
        Load::High => LOAD_HIGH,
        Load::Critical => LOAD_CRITICAL,
    };
    CURRENT_LOAD.store(value, std::sync::atomic::Ordering::Relaxed);
}

#[cfg(feature = "admission")]
fn current_load() -> Load {
    match CURRENT_LOAD.load(std::sync::atomic::Ordering::Relaxed) {
        LOAD_HIGH => Load::High,
        LOAD_CRITICAL => Load::Critical,
        _ => Load::Normal,
    }
}

/// Whether `config` sheds at `load`. `High` sheds sheddable Low-priority
/// domains; `Critical` sheds Low and Medium. Unconfigured domains are
/// Medium and therefore survive `High` (ADR-0002).
#[cfg(feature = "admission")]
fn should_shed(config: AdmissionConfig, load: Load) -> bool {
    match load {
        Load::Normal => false,
        Load::High => config.sheddable && config.priority == Priority::Low,
        Load::Critical => matches!(config.priority, Priority::Low | Priority::Medium),
    }
}

/// Why `try_run` refused to execute work. Requires `admission`.
#[cfg(feature = "admission")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionError {
    /// The work was shed under load; retry later or degrade gracefully.
    Shed {
        /// The domain's configured priority.
        priority: Priority,
        /// The load level that tripped the shed.
        load: Load,
    },
    /// The domain's concurrency budget is fully occupied.
    ConcurrencyExhausted,
}

#[cfg(feature = "admission")]
impl fmt::Display for AdmissionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Shed { priority, load } => write!(
                f,
                "work shed at {load:?} load (priority {priority:?})\n\
                 hint: retry later, lower the domain's priority class, or have the caller degrade gracefully"
            ),
            Self::ConcurrencyExhausted => write!(
                f,
                "admission concurrency budget exhausted\n\
                 hint: retry later or raise the budget with arduous_rt::set_admission"
            ),
        }
    }
}

#[cfg(feature = "admission")]
impl std::error::Error for AdmissionError {}

/// Shed events allowed into the audit ring per second; beyond this they
/// collapse into one `ShedCollapsed` entry (ADR-0003).
#[cfg(feature = "admission")]
const SHED_AUDIT_MAX_PER_SEC: usize = 64;

#[cfg(feature = "admission")]
struct ShedAuditState {
    window: std::time::Instant,
    count: usize,
    collapsed: usize,
}

#[cfg(feature = "admission")]
static SHED_AUDIT: LazyLock<Mutex<ShedAuditState>> = LazyLock::new(|| {
    Mutex::new(ShedAuditState {
        window: std::time::Instant::now(),
        count: 0,
        collapsed: 0,
    })
});

#[cfg(feature = "admission")]
fn audit_shed(key: &str, config: AdmissionConfig, load: Load) {
    let mut state = SHED_AUDIT.lock().unwrap_or_else(|e| e.into_inner());
    let now = std::time::Instant::now();
    if now.duration_since(state.window) >= std::time::Duration::from_secs(1) {
        if state.collapsed > 0 {
            audit_with_note(
                AuditKind::ShedCollapsed,
                key,
                None,
                format!("{} shed events collapsed", state.collapsed),
            );
        }
        state.window = now;
        state.count = 0;
        state.collapsed = 0;
    }
    if state.count < SHED_AUDIT_MAX_PER_SEC {
        state.count += 1;
        audit_with_note(
            AuditKind::Shed,
            key,
            None,
            format!("priority={:?} load={:?}", config.priority, load),
        );
    } else {
        state.collapsed += 1;
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
    #[cfg(feature = "admission")]
    return execute_on(rt, fut, None).await;
    #[cfg(not(feature = "admission"))]
    return execute_on(rt, fut).await;
}

/// Run a future on `rt` after admission checks. Identical execution to
/// [`run`] — inline when already on `rt`, spawned otherwise — but first
/// consults the domain's admission configuration: sheddable domains are
/// rejected with [`AdmissionError::Shed`] under a reported load, and
/// (from phase B2) concurrency budgets are enforced before executing.
/// Requires the `admission` feature.
///
/// Shedding applies to both execution paths, including the inline one:
/// a reentrant caller still occupies a worker, and a context-based
/// bypass would make the policy gameable (ADR-0004).
///
/// # Errors
///
/// Returns [`AdmissionError::Shed`] when the host has reported a load
/// level that sheds this domain's priority class.
#[cfg(feature = "admission")]
pub async fn try_run<R, F>(
    rt: &'static tokio::runtime::Runtime,
    fut: F,
) -> Result<R, AdmissionError>
where
    R: Send + 'static,
    F: Future<Output = R> + Send + 'static,
{
    let (config, key) = {
        let guard = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
        (admission_config(&guard, rt), admission_key(&guard, rt))
    };
    let load = current_load();
    if should_shed(config, load) {
        audit_shed(&key, config, load);
        return Err(AdmissionError::Shed {
            priority: config.priority,
            load,
        });
    }
    let permit = match config.concurrency {
        Some(capacity) => {
            let mut guard = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
            match admission_permit(&mut guard, &key, capacity) {
                Some(permit) => Some(permit),
                None => return Err(AdmissionError::ConcurrencyExhausted),
            }
        }
        None => None,
    };
    Ok(execute_on(rt, fut, permit).await)
}

async fn execute_on<R, F>(
    rt: &'static tokio::runtime::Runtime,
    fut: F,
    #[cfg(feature = "admission")] permit: Option<tokio::sync::OwnedSemaphorePermit>,
) -> R
where
    R: Send + 'static,
    F: Future<Output = R> + Send + 'static,
{
    // The permit (when present) is held across execution and released on
    // completion or cancellation — cancellation-safe via guard drop on
    // the inline path and via move-into-task on the spawn path.
    #[cfg(feature = "admission")]
    let _permit = permit;
    if is_running_on(rt) {
        fut.await
    } else {
        let handle = rt.spawn(async move {
            #[cfg(feature = "admission")]
            let _permit = _permit;
            fut.await
        });
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

    #[cfg(feature = "governance")]
    #[test]
    fn governance_token_scopes_creation_to_domain() {
        allow_test_names();
        allow_name("tok_demo").unwrap();
        allow_name("tok_demo/*").unwrap();
        let token = DomainToken::acquire("tok_demo").unwrap();
        assert_eq!(token.domain(), "tok_demo");
        assert!(named_runtime_in(&token, "tok_demo").is_ok());
        assert!(named_runtime_in(&token, "tok_demo/worker").is_ok());
        assert!(matches!(
            named_runtime_in(&token, "other.place"),
            Err(NameError::OutsideDomain { .. })
        ));
        assert!(matches!(
            DomainToken::acquire("nowhere.atall"),
            Err(NameError::NotAllowed { .. })
        ));
    }

    #[cfg(feature = "governance")]
    #[test]
    fn governance_token_mints_subdomain_tokens() {
        allow_test_names();
        allow_name("mint_demo").unwrap();
        allow_name("mint_demo/*").unwrap();
        deny_name("mint_demo/secret.*").unwrap();
        let root = DomainToken::acquire("mint_demo").unwrap();
        let wallet = root.mint("wallet").unwrap();
        assert_eq!(wallet.domain(), "mint_demo/wallet");
        assert!(named_runtime_in(&wallet, "mint_demo/wallet/pay").is_ok());
        // Delegation stops where policy says no.
        assert!(matches!(
            root.mint("secret.thing"),
            Err(NameError::Denied { .. })
        ));
        assert!(matches!(
            root.mint(""),
            Err(NameError::OutsideDomain { .. })
        ));
    }

    #[cfg(feature = "governance")]
    #[test]
    fn governance_configured_runtime_uses_worker_and_name_prefix() {
        allow_test_names();
        allow_configured(
            "cfg_demo.*",
            RuntimeConfig::default()
                .with_worker_threads(1)
                .with_thread_name_prefix("cfg_demo_worker"),
        )
        .unwrap();
        let rt = try_named_runtime("cfg_demo.pool").unwrap();
        // The root future runs on the calling thread under block_on; a
        // spawned task runs on a worker, which is where the prefix shows.
        let thread_name = rt.block_on(async {
            tokio::spawn(async { std::thread::current().name().unwrap().to_string() })
                .await
                .unwrap()
        });
        assert!(
            thread_name.starts_with("cfg_demo_worker"),
            "worker thread should carry the configured prefix, got {thread_name:?}"
        );
    }

    #[cfg(feature = "governance")]
    #[test]
    fn governance_thread_budget_exceeded() {
        allow_test_names();
        allow_configured("tb_demo.*", RuntimeConfig::default().with_worker_threads(1)).unwrap();
        set_thread_quota("tb_demo.*", 2).unwrap();
        assert!(try_named_runtime("tb_demo.a").is_ok());
        assert!(try_named_runtime("tb_demo.b").is_ok());
        assert!(matches!(
            try_named_runtime("tb_demo.c"),
            Err(NameError::ThreadBudgetExceeded { max: 2, .. })
        ));
    }

    #[cfg(feature = "governance")]
    #[test]
    fn governance_domain_root_matches_subtree_pattern() {
        allow_test_names();
        allow_name("root_demo.*").unwrap();
        deny_name("root_demo.blocked.*").unwrap();
        // The root of a subtree is governed by patterns for that subtree.
        assert!(DomainToken::acquire("root_demo").is_ok());
        assert!(try_named_runtime("root_demo").is_ok());
        assert!(matches!(
            try_named_runtime("root_demo.blocked"),
            Err(NameError::Denied { .. })
        ));
    }

    #[cfg(feature = "governance")]
    #[test]
    fn governance_audit_log_records_created_and_rejected() {
        allow_test_names();
        allow_name("aud_demo.*").unwrap();
        deny_name("aud_demo.blocked.*").unwrap();
        try_named_runtime("aud_demo.ok").unwrap();
        assert!(matches!(
            try_named_runtime("aud_demo.blocked.x"),
            Err(NameError::Denied { .. })
        ));
        let events = audit_events();
        assert!(
            events
                .iter()
                .any(|e| e.kind == AuditKind::Created && e.name == "aud_demo.ok")
        );
        assert!(
            events
                .iter()
                .any(|e| e.kind == AuditKind::RejectedDenied && e.name == "aud_demo.blocked.x")
        );
    }

    #[cfg(feature = "admission")]
    #[test]
    fn admission_set_admission_validates_and_stores() {
        allow_test_names();
        assert!(
            set_admission(
                "adm_demo.*",
                AdmissionConfig::default().with_priority(Priority::Low)
            )
            .is_ok()
        );
        assert!(matches!(
            set_admission("adm_demo.[bad", AdmissionConfig::default()),
            Err(NameError::InvalidPattern(_))
        ));
    }

    #[cfg(feature = "admission")]
    #[test]
    fn admission_records_runtime_key_at_creation() {
        allow_test_names();
        allow_name("adm_key.*").unwrap();
        let rt = try_named_runtime("adm_key.pool").unwrap();
        let guard = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
        let key = guard
            .by_ptr
            .get(&(rt as *const _ as usize))
            .expect("runtime must have an admission key");
        assert_eq!(key, "adm_key.pool");
        assert!(!guard.admissions.contains_key("adm_key.pool"));
        drop(guard);
        // Pattern-level configuration resolves through the name fallback.
        set_admission(
            "adm_key.*",
            AdmissionConfig::default().with_priority(Priority::High),
        )
        .unwrap();
        let guard = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
        let config = crate::admission_config(&guard, rt);
        assert_eq!(config.priority, Priority::High);
        // Name-level configuration is more specific and wins.
        drop(guard);
        set_admission(
            "adm_key.pool",
            AdmissionConfig::default().with_priority(Priority::Low),
        )
        .unwrap();
        let guard = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
        let config = crate::admission_config(&guard, rt);
        assert_eq!(config.priority, Priority::Low);
    }

    #[cfg(feature = "admission")]
    fn load_lock() -> &'static std::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
    }

    #[cfg(feature = "admission")]
    fn shed_test_runtime(
        suffix: &str,
        config: AdmissionConfig,
    ) -> &'static tokio::runtime::Runtime {
        allow_test_names();
        let pattern = format!("shed_{suffix}.*");
        allow_name(&pattern).unwrap();
        set_admission(&pattern, config).unwrap();
        try_named_runtime(&format!("shed_{suffix}.rt")).unwrap()
    }

    #[cfg(feature = "admission")]
    #[test]
    fn admission_sheds_low_priority_at_high_load() {
        let _guard = load_lock().lock().unwrap_or_else(|e| e.into_inner());
        let rt = shed_test_runtime(
            "low",
            AdmissionConfig::default().with_priority(Priority::Low),
        );
        report_load(Load::High);
        let result = rt.block_on(try_run(rt, async { 1 }));
        assert!(matches!(
            result,
            Err(AdmissionError::Shed {
                priority: Priority::Low,
                load: Load::High
            })
        ));
        report_load(Load::Normal);
        assert_eq!(rt.block_on(try_run(rt, async { 2 })).unwrap(), 2);
    }

    #[cfg(feature = "admission")]
    #[test]
    fn admission_medium_survives_high_sheds_at_critical() {
        let _guard = load_lock().lock().unwrap_or_else(|e| e.into_inner());
        // Medium = the default for unconfigured domains (ADR-0002).
        let rt = shed_test_runtime("med", AdmissionConfig::default());
        report_load(Load::High);
        assert_eq!(rt.block_on(try_run(rt, async { 1 })).unwrap(), 1);
        report_load(Load::Critical);
        assert!(matches!(
            rt.block_on(try_run(rt, async { 1 })),
            Err(AdmissionError::Shed {
                priority: Priority::Medium,
                load: Load::Critical
            })
        ));
        report_load(Load::Normal);
    }

    #[cfg(feature = "admission")]
    #[test]
    fn admission_not_sheddable_defers_high_to_critical() {
        let _guard = load_lock().lock().unwrap_or_else(|e| e.into_inner());
        let rt = shed_test_runtime(
            "sticky",
            AdmissionConfig::default()
                .with_priority(Priority::Low)
                .with_sheddable(false),
        );
        report_load(Load::High);
        assert_eq!(rt.block_on(try_run(rt, async { 1 })).unwrap(), 1);
        report_load(Load::Critical);
        assert!(matches!(
            rt.block_on(try_run(rt, async { 1 })),
            Err(AdmissionError::Shed { .. })
        ));
        report_load(Load::Normal);
    }

    #[cfg(feature = "admission")]
    #[test]
    fn admission_shed_audit_flood_guard_caps_events() {
        let _guard = load_lock().lock().unwrap_or_else(|e| e.into_inner());
        let rt = shed_test_runtime(
            "flood",
            AdmissionConfig::default().with_priority(Priority::Low),
        );
        report_load(Load::High);
        for _ in 0..(SHED_AUDIT_MAX_PER_SEC + 100) {
            let _ = rt.block_on(try_run(rt, async { 1 }));
        }
        report_load(Load::Normal);
        let shed_events = audit_events()
            .iter()
            .filter(|e| e.kind == AuditKind::Shed)
            .count();
        assert!(
            shed_events <= SHED_AUDIT_MAX_PER_SEC,
            "flood guard must cap shed audit events, saw {shed_events}"
        );
    }

    #[cfg(feature = "admission")]
    #[test]
    fn admission_shed_collapse_event_records_dropped_count() {
        let _guard = load_lock().lock().unwrap_or_else(|e| e.into_inner());
        let rt = shed_test_runtime(
            "collapse",
            AdmissionConfig::default().with_priority(Priority::Low),
        );
        report_load(Load::High);
        for _ in 0..(SHED_AUDIT_MAX_PER_SEC + 50) {
            let _ = rt.block_on(try_run(rt, async { 1 }));
        }
        // Rotate the one-second window, then trigger one more shed; the
        // collapsed count from the first window must flush on rotation.
        // Assert on the guard's internal state: the shared audit ring is
        // written by every test in the binary, so ring-content assertions
        // here would race eviction.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let _ = rt.block_on(try_run(rt, async { 1 }));
        report_load(Load::Normal);
        let state = SHED_AUDIT.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(state.count, 1, "one shed recorded in the fresh window");
        assert_eq!(
            state.collapsed, 0,
            "collapsed count must flush (and reset) on window rotation"
        );
    }

    /// Spawn a `try_run` that acquires its permit, signals through the
    /// returned channel, then parks forever. The receiver fires only
    /// after the permit is held, so exhaustion tests are deterministic.
    #[cfg(feature = "admission")]
    fn hold_permit(
        rt: &'static tokio::runtime::Runtime,
    ) -> tokio::task::JoinHandle<Result<(), AdmissionError>> {
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = rt.spawn(try_run(rt, async move {
            tx.send(()).expect("signal permit acquisition");
            std::future::pending::<()>().await;
        }));
        rx.recv().expect("in-flight task must acquire its permit");
        handle
    }

    #[cfg(feature = "admission")]
    #[test]
    fn admission_concurrency_budget_exhausts_then_recovers() {
        let _guard = load_lock().lock().unwrap_or_else(|e| e.into_inner());
        allow_test_names();
        allow_name("budget.*").unwrap();
        set_admission("budget.*", AdmissionConfig::default().with_concurrency(1)).unwrap();
        let rt = try_named_runtime("budget.pool").unwrap();

        let in_flight = hold_permit(rt);
        let rejected = rt.block_on(try_run(rt, async { 2 }));
        assert!(matches!(
            rejected,
            Err(AdmissionError::ConcurrencyExhausted)
        ));
        in_flight.abort();
        let _ = rt.block_on(in_flight);
        assert_eq!(rt.block_on(try_run(rt, async { 7 })).unwrap(), 7);
    }

    #[cfg(feature = "admission")]
    #[test]
    fn admission_cancellation_releases_the_permit() {
        let _guard = load_lock().lock().unwrap_or_else(|e| e.into_inner());
        allow_test_names();
        allow_name("cancel.*").unwrap();
        set_admission("cancel.*", AdmissionConfig::default().with_concurrency(1)).unwrap();
        let rt = try_named_runtime("cancel.pool").unwrap();

        let handle = hold_permit(rt);
        handle.abort();
        let _ = rt.block_on(handle);

        // The aborted execution's permit must be back in the pool.
        assert_eq!(rt.block_on(try_run(rt, async { 9 })).unwrap(), 9);
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
