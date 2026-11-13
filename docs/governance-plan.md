# Governance layer plan

Status: proposal. Tracks the evolution of the `explicit_name` /
`glob_support` name filters into a domain-based runtime governance
layer for `arduous_rt`.

## What "governance" means here

Process-wide, policy-driven control over **which components may create
runtime resources, how much, and in what configuration** — expressed in
hierarchical domain vocabulary (`org.gnostr.wallet`,
`org.gnostr.relay-debug`). Target: plugin hosts, multi-component
applications, the gnostr stack.

## Explicit non-goals

In-process rules are **cooperative, not a sandbox**. A hostile crate in
the same process can always `std::process`/its own threads; governance
here protects against accidents, unbounded growth, and ops policy
violations — not adversarial code. No cgroups, no seccomp, no process
isolation. Say so in docs wherever the feature is described.

## Phase 0 — v0.8: deny rules, quotas, audit ✅ implemented (unreleased)

Feature gate: `governance` (implies `explicit_name` + `glob_support`;
the two existing features keep working as aliases).

- **Deny with precedence.** `deny_name(pattern)`; evaluation order
  `deny > allow > reject-all (empty allowlist)`. Allows the common
  policy shape: allow `org.gnostr.*`, deny `org.gnostr.experimental.*`.
- **Per-pattern quotas.** `set_quota(pattern, max)`; creation counts the
  runtimes matched by each pattern. New error variant
  `NameError::QuotaExceeded { pattern, max }` with the same actionable
  `Display` style as `NotAllowed`.
- **Introspection.** `registered_patterns()`, `list_runtimes()`
  (name + matched pattern + thread count).
- **Audit events.** Structured `tracing` events on the governance paths:
  `runtime_created { name, matched }`, `runtime_rejected { name, reason }`,
  `quota_exceeded { pattern, max }`. (The `tracing` facade is already
  the crate's observability story.)

## Phase 1 — v0.9: capabilities + declarative policy ✅ implemented (unreleased)

Turns the naming convention into enforcement.

- **Domain tokens.** `DomainToken::acquire("org.gnostr")` validates
  against policy and returns an opaque token;
  `named_runtime_in(&token, "wallet")` requires `wallet` to live under
  the token's domain (strict prefix: `domain` or `domain/...`). Code
  without a token can only use names policy left open (or none).
- **Policy object.** `Policy { deny, allow: Vec<(pattern, Option<quota>)> }`
  with `set_policy(Policy)` atomic swap (hot reload without restart).
- **Declarative loading.** `policy-toml` feature: load a `Policy` from a
  TOML file at startup, so ops can ship/audit policy separately from
  code. Keeps serde/toml out of the base dependency tree.
- Sub-token delegation (a token minting narrower sub-domain tokens) is a
  candidate here but can be deferred without blocking the rest.

## Phase 2 — v1.0: resource shaping ✅ implemented (unreleased)

- **Per-pattern runtime configs.** `allow_configured(pattern, RuntimeConfig)`
  where `RuntimeConfig` covers worker thread count, thread name prefix,
  and driver enablement — so `org.gnostr.cpu-bound` can get a dedicated
  4-worker pool while `org.gnostr.io` rides the default.
- **Budget in threads, not runtime count.** Account the real resource
  (threads × configs) against per-domain budgets.
- **Audit log API.** Small in-memory ring buffer of governance events
  with an `audit_events()` accessor, complementing the tracing events.

## Compatibility

Nothing here breaks existing API until 1.0: Phase 0 adds variants and
functions; `governance` is a pure superset feature of the current two.
MSRV and dependency posture (globset optional, serde only behind
`policy-toml`) stay as they are.

## Decision points to resolve before Phase 0

1. Feature naming: introduce `governance` now, or keep shipping new
   items under `glob_support` and rename later?
2. Quota semantics: counted per matched pattern (a runtime matching two
   patterns counts against both) or first-match-wins?
3. `named_runtime_in` prefix rule: strict `domain/...` only, or should a
   token for `org.gnostr` also satisfy a bare `org.gnostr` name?
