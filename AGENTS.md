# AGENTS.md

Guidance for AI coding agents (and human contributors) working in this
repository. For the public API, see README.md; release history is in
CHANGELOG.md.

## What this crate is

`arduous_rt` is a process-global registry of named Tokio runtimes:
get-or-create by name, hierarchical nesting, a reentrancy-safe `run()`,
and optional name filters. Runtimes are intentionally leaked and never
shut down.

## Commands

Build, test, lint:

```bash
make build
make test
make test-all
make clippy
make fmt
```

Before committing any change to `src/lib.rs` or `tests/`, run the feature
combinations — they exercise different cfg paths:

```bash
cargo test
cargo test --features explicit_name
cargo test --features governance
cargo test --features policy-toml
cargo test --all-features
```

Demos (`demo_deadlock` and `demo_starvation` hang by design — always run
them under a timeout):

```bash
cargo run --example demo_backpressure
cargo run --example demo_panic
cargo run --example trace_demo
cargo run --example nested_proofs
cargo run --example stress_test
timeout 5s cargo run --example demo_deadlock
timeout 5s cargo run --example demo_starvation
```

Downstream governance smoke test — a throwaway consumer project against
the local crate, or a published version (`make consumer-smoke
VERSION=0.8.1`):

```bash
./scripts/governance_consumer_smoke.sh
./scripts/governance_consumer_smoke.sh 0.8.1
```

## Project conventions

- One topic per commit; never stack uncommitted edits; split hunks when
  two topics touch the same file.
- Commit message form: `<path>:<imperative summary>`.
- Never push without an explicit user order.

## API quick reference

- `global_rt()` — the shared runtime named `"global"`.
- `named_runtime(name)` — get or create; panics on rejected name or full
  registry.
- `try_named_runtime(name)` — fallible variant returning `NameError`.
- `run(rt, fut).await` — polls inline when already on `rt`, spawns
  otherwise. Never nest `Runtime::block_on` on the same runtime (deadlock;
  see `demo_deadlock`).
- `child_runtime(parent, child)`, `nested_runtime(&[...])`,
  `nested_chain(n)` — hierarchical names joined with `/`.
- Registry cap: 256 distinct runtimes by default; the `unbounded` feature
  removes it.
- With `explicit_name`, authorize names first via
  `arduous_rt::allow_name(...)`; an empty allowlist rejects every new
  name. With `glob_support`, entries may be globs such as `org.gnostr.*`.

## Gotchas

- `nested_chain` uses fixed global names (`level_0`, `level_0/level_1`,
  ...); two chains of the same depth return the same runtimes.
- Cargo features unify across the dependency graph: a dependency enabling
  `explicit_name` or `unbounded` activates them for the whole build.
  Rejection panics explain this inline.
- `run()`'s inner task detaches and runs to completion if the awaiting
  task is cancelled.
- Every test that creates runtimes must call `allow_test_names()` first
  (seeded via `Once`; a no-op without `explicit_name`). The name lists
  live in the helpers in `src/lib.rs` and `tests/arduous.rs` — update
  them when adding tests with new runtime names, and remember
  `nested_chain` names are cumulative `/`-joined paths.
- Doctests create runtimes in isolated processes; they self-seed the
  allowlist with hidden `#` lines. Keep that pattern when adding doctests.
- `fail N` / `boom N` panic lines during tests are intentional negative
  tests framed by the `PanicFrame` hook — expected output, not failures.
- External network tests are `#[ignore]`d by default; the `Rust` workflow
  runs them with `--include-ignored`.
- Deferred work has stub examples with TODO notes:
  `examples/deferred_loom_registry.rs` (loom model of the registry race)
  and `examples/deferred_rwlock_registry.rs` (RwLock lookup path). Read
  them before re-litigating those decisions.
- Policy-swap tests live in `tests/governance_policy.rs` in their own
  binary on purpose: `set_policy` replaces global registry state, and
  cargo runs test binaries sequentially. Do not move them into
  `tests/arduous.rs`.
- The governance design lives in `docs/governance-plan.md` (all three
  phases implemented); keep it in sync when changing governance
  behavior.
