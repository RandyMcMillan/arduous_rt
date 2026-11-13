# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `governance` feature (implies `explicit_name` + `glob_support`):
  `deny_name`/`undeny_name` with deny-beats-allow precedence, per-pattern
  quotas via `set_quota` with `NameError::QuotaExceeded`, introspection
  via `registered_patterns`/`list_runtimes`, and structured tracing
  events on governance decisions.
- `DomainToken` + `named_runtime_in`: tokens authorize one domain and
  scope runtime creation to it (`NameError::OutsideDomain` otherwise).
- `Policy` + `set_policy`: atomic whole-policy swaps; per-pattern quota
  counts are recomputed against pre-existing runtimes.
- `policy-toml` feature: `policy_from_toml` parses and validates a TOML
  policy; `PolicyError::{Parse, Invalid}`.
- `RuntimeConfig` + `allow_configured`: prescribe worker thread counts
  and thread name prefixes per pattern.
- `set_thread_quota` caps total worker threads per pattern
  (`NameError::ThreadBudgetExceeded`); per-runtime worker counts feed
  quota recomputation on policy swaps.
- `audit_events`: ring buffer (256 entries) of governance decisions —
  creations and every rejection kind (`AuditKind`).

## [0.7.0] - 2026-11-13

### Changed

- MSRV raised to 1.88: let-chains in the integration tests and
  hdrhistogram 7.6.0 (via the `console` feature's console-subscriber)
  both require it.
- CI: the check job runs on Linux, macOS, and Windows; the MSRV job runs
  the full three-feature-combination test matrix on the MSRV toolchain.

## [0.6.2] - 2026-11-13

### Added

- `SECURITY.md` (reporting policy) and `CONTRIBUTING.md` (gates,
  test-seeding rules, pointers to AGENTS.md and the deferred-work stubs).
- README: zero-unsafe comparison row, concurrency-identity test
  description, deferred loom/RwLock stubs with TODOs
  (`examples/deferred_*.rs`).

### Changed

- `arduous_idempotent_named_runtime` strengthened: 10 fresh-name creator
  rounds asserting a single winning runtime, plus a pointer-stable lookup
  hammer.

## [0.6.1] - 2026-11-13

### Changed

- Runtimes are created outside the registry lock; racing losers are
  disposed on a blocking thread (dropping a runtime inside an async
  context panics).
- `#![forbid(unsafe_code)]`.
- README installation leads with the crates.io version; `stress_test`
  added to the CI example smoke run.

## [0.6.0] - 2026-11-13

### Added

- Registry cap: at most 256 distinct named runtimes by default; the 257th
  fails with `NameError::RegistryFull` (or panics via `named_runtime`).
- `unbounded` feature: removes the registry cap.
- `explicit_name` feature: names must be pre-authorized with `allow_name`;
  an empty allowlist rejects every new name. `disallow_name` and
  `is_name_allowed` round out management.
- `glob_support` feature (implies `explicit_name`): allowlist entries may
  be globset patterns such as `org.gnostr.*` or `org.gnostr.*-debug`.
- `try_named_runtime(name) -> Result<&'static Runtime, NameError>`:
  fallible variant of `named_runtime`; rejection messages carry hints
  (including the cargo feature-unification gotcha).
- `cargo audit` scheduled workflow; GitHub Actions pinned to commit SHAs.

### Security

- Registry lock ignores mutex poisoning: a panicking insert no longer
  takes down all future `named_runtime` calls.
- `stress()` guards `runtimes.len() * tasks_per` against `usize` overflow.
- `run()` docs note the spawned task detaches if the awaiter is cancelled.

### Changed

- README: comparison table against `trt`, the DIY `once_cell` pattern,
  raw tokio, and `async-global-executor`.

## [0.5.0] - 2026-11-13

### Added

- crates.io publish metadata (description, `license = "MIT"`, repository,
  keywords, categories, `rust-version = "1.85"`) and the MIT `LICENSE` file.

### Removed

- **Breaking:** `add()`, `spawn_on()`, and the `runtime`/`global_rt_named`
  aliases. `named_runtime` is the single entry point.

### Changed

- tokio features slimmed to `rt-multi-thread` + `tracing`
  (`macros`/`time`/`sync` moved to dev-dependencies).
- `once_cell` dependency dropped in favor of `std::sync::LazyLock`.

## [0.4.0] - 2026-11-13

### Added

- Negative-test output framing: `[negative test]` banners plus a scoped,
  drop-restored `PanicFrame` panic hook that wraps each intentional panic
  in `EXPECTED PANIC` / `END EXPECTED PANIC` banners on stderr.

### Changed

- **Breaking:** the exported `trace!` macro is renamed to `arduous_trace!`
  (the old name collided with `tracing::trace!` / `log::trace!`).
- **Breaking:** library diagnostics (`arduous_trace!`, runtime lifecycle,
  `timed`/`benchmark`, `stress`) now emit through `tracing` instead of
  `println!`. Binaries must install a subscriber (examples use
  `tracing_subscriber::fmt`, or `console_subscriber` with `--features console`).
- `banner()` starts on a fresh line so it no longer glues onto partial
  output (e.g. the test harness's `test name ...` prefix).

### Fixed

- All clippy warnings across tests and examples; `cargo fmt` normalized.

## [0.3.0] - 2026-11-13

### Added

- Crate-level docs covering the leak contract, fixed nested-chain names,
  reentrancy semantics, and observability; doctests for the main entry
  points; `# Panics` sections where applicable.
- CI workflow: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
  `cargo test --all-features`.

## [0.2.0] - 2026-11-13

### Changed

- **Breaking:** library diagnostics routed through the `tracing` facade
  (see 0.4.0 for the full subscriber-side migration notes).
- **Breaking:** `trace!` renamed to `arduous_trace!`.

## [0.1.0] - 2026-11-13

### Added

- Initial implementation: process-global named tokio runtime registry
  (`global_rt`, `named_runtime`, `child_runtime`, `nested_runtime`,
  `nested_chain`, `nested_leaf`), reentrancy-safe `run`, `is_running_on`,
  `stress`/`stress_chain`, `timed`/`timed_async`, `benchmark`/
  `benchmark_async`, `banner`, `available_threads`, and the `trace!`
  macro (later renamed).
