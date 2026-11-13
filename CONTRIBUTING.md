# Contributing

Thanks for your interest in contributing to `arduous_rt`.

Start with [AGENTS.md](AGENTS.md) — it documents the commands, the
project conventions (one topic per commit, commit message form, no
pushes without an explicit order), and the gotchas that are not obvious
from the code.

Run the full gates before committing any change to `src/lib.rs` or
`tests/`:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo test --features explicit_name
cargo test --all-features
```

Notes for new code:

- Tests that create runtimes must call `allow_test_names()` first and
  register any new runtime names in the seed lists (`src/lib.rs` and
  `tests/arduous.rs` helpers). Remember `nested_chain` names are
  cumulative `/`-joined paths.
- Doctests that create runtimes self-seed the allowlist with hidden `#`
  lines.
- Intentional panics in tests use the `PanicFrame` framing — see the
  `negative_*` tests and `arduous_*_with_errors` / `arduous_panic_recovery`
  for the pattern.
- Deferred design decisions (loom model, `RwLock` lookup path) have stub
  examples under `examples/deferred_*.rs`; read them before re-opening
  those topics.
