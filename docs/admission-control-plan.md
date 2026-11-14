# Admission control plan (Phase B)

Status: proposal. Extends the governance layer from **resource
acquisition** (who may create runtimes) to **work admission** (whose
work runs when the process is loaded). Companion to
`governance-plan.md` (Phase A items: circuit breaker on creation, rate
caps, policy digests, token caveats are tracked there).

## What "admission control" means here

`run(rt, fut)` is called per unit of work, arbitrarily often, and
currently admits everyone. The plan: domain-aware admission on the
`run()` path — per-domain concurrency budgets, priority classes, and a
host-fed load signal that sheds low-priority domains first.

The architectural claim: arduous_rt is the only component that knows
both **entitlements** (domains, tokens, quotas) and **execution** (the
runtimes work lands on). A service mesh sees traffic but not
entitlements; the host sees health but not domains. Admission at this
layer is the novel combination — nobody else on crates.io has it.

## Explicit non-goals

- Not a service mesh: no proxying, no transport-level rate limiting of
  arbitrary host traffic.
- No built-in health heuristics: the load signal is host-fed (the host
  knows its own health; the crate must not guess).
- No in-process adversarial guarantees: admission is cooperative, same
  threat model as governance.
- `run()` itself never changes behavior; admission lives in a new
  `try_run` family. Existing callers are untouched forever.

## Phase B0 — foundations (v0.9)

Admission keys and configuration, no behavior change yet.

- **Admission key resolution.** At creation, record the runtime's
  stable address → matched pattern in the registry. `&'static Runtime`
  addresses are stable (leaked), so an internal `HashMap<usize, String>`
  resolves any runtime handle to its admission key in O(1). Names
  resolving to no pattern (non-governance builds) use a wildcard key.
- **Types.**
  ```rust
  pub enum Priority { Critical, High, Medium, Low }   // default Medium
  pub struct AdmissionConfig { priority, concurrency: Option<usize>, sheddable: bool }
  pub fn set_admission(pattern: &str, config: AdmissionConfig) -> Result<(), NameError>;
  ```
  Imperative API, consistent with the crate's split: the TOML policy
  covers allow/deny/quota; shaping (configs, budgets) is code-side.
- **Feature gate:** `admission = ["governance"]`. Off by default —
  this is a scope expansion and opts in explicitly.
- No `try_run` yet; B0 compiles and resolves keys only.

## Phase B1 — load signal and shedding (v0.9)

- **Host-fed load signal**, deliberately minimal:
  ```rust
  pub enum Load { Normal, High, Critical }
  pub fn report_load(load: Load); // atomic; host calls from its own loop
  ```
  A trait-based signal is deferred indefinitely: a host that derives
  load from exotic sources can do so in its own loop and report.
- **`try_run` family:**
  ```rust
  pub enum AdmissionError {
      Shed { priority: Priority, load: Load },
      ConcurrencyExhausted,
  }
  pub async fn try_run<R, F>(rt: &'static Runtime, fut: F)
      -> Result<R, AdmissionError>; // run() semantics otherwise
  ```
- **Shed rules:** `High` sheds `Low` (and anything `sheddable=false` is
  never shed below `Critical`); `Critical` sheds `Low` + `Medium`.
  Unconfigured domains are `Medium`. Shed decisions are one `try_run`
  call's `Err` — the caller decides what to do with the rejected work.
- **Audit:** `AuditKind::Shed` events in the existing ring buffer, with
  the admission key, priority, and load. Ring-flood guard: if more than
  64 sheds occur within one second, collapse to a single
  `ShedCollapsed { count }` event.

## Phase B2 — concurrency budgets (v0.9)

- Per admission key, a bounded permit pool (`tokio::sync::Semaphore`).
  `try_run` acquires a permit before executing; the spawn path moves
  the permit into the spawned task; the inline path holds a guard.
- Cancellation-safety is a hard requirement: a cancelled `try_run`
  must release its permit (guard drop) and a detached spawned task must
  carry its permit to completion.
- `ConcurrencyExhausted` when the pool is empty at admission time —
  never queues (queuing would hide load; the caller decides).

## Phase B3 — gnostr demo

- `org.gnostr/relay` = `High`, `org.gnostr/chat` = `High`,
  `org.gnostr/crawler` = `Low` + `sheddable`.
- Synthetic load generator + `report_load` loop; verify chat/relay keep
  flowing while crawler work errors with `Shed`; audit log shows the
  decisions.
- README section + comparison-table row ("domain-aware admission
  control").

## Decision points — resolved as ADRs (status: proposed)

1. Key resolution: runtime-pointer map primary, name-taking sugar
   later — `docs/decisions/ADR-0001-admission-key-resolution.md`.
2. Priority default for unconfigured domains: `Medium` —
   `docs/decisions/ADR-0002-default-priority.md`.
3. Shed audit flood threshold: 64/s with collapse, tuned from demo
   data — `docs/decisions/ADR-0003-shed-audit-flood-threshold.md`.
4. Admission on the inline path: yes, both paths —
   `docs/decisions/ADR-0004-inline-path-admission.md`.

## Compatibility

Everything is additive behind `admission`; `run()`, the registry, and
the governance API are untouched. MSRV and dependency posture unchanged
(`tokio::sync::Semaphore` is already in the tree).
