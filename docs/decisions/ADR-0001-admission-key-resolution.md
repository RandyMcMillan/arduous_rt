# ADR-0001: Admission key resolution

Status: accepted (ratified; implemented in B0)

## Context

`try_run(rt, fut)` receives a `&'static tokio::runtime::Runtime` handle.
Admission configuration (`set_admission`) is keyed by governance
pattern. Admission therefore needs to map a runtime handle to its key.

## Options

**A. Runtime-pointer map (proposed).** At creation, record the leaked
runtime's address (`usize`, identity only — never dereferenced) mapped
to its matched pattern. Lookup is O(1) and automatic; callers pass
handles as they already do. Soundness rests on one invariant: registry
runtimes are `Box::leak`ed, so their addresses are stable and unique for
the process lifetime. Runtimes created before any pattern match resolve
to a wildcard key.

**B. Name-taking admission (`try_run_named(name, fut)`).** Callers pass
the name; the registry resolves it. Explicit, no pointer map, no
address-stability argument — but callers overwhelmingly hold handles,
not names, so every call site pays a lookup and a new failure mode
(name exists but maps to a different runtime).

**C. Hybrid.** Pointer map primary, name-taking variants as sugar.

## Decision

A, with C's sugar deferred until a real caller asks for it.

## Amendment 1 (post-implementation, from the gnostr demo)

The map records the runtime's **name**, not its matched pattern.
Admission lookup tries the name first and falls back to the name's
matched allow pattern. Rationale: pattern-only keys proved too coarse —
one `org.gnostr/**` allow pattern covered every subsystem, making
per-subsystem priorities (core High, crawler Low) inexpressible. The
address-stability argument is unchanged.

## Consequences

The registry gains one `HashMap<usize, String>` written once per
creation. The address-stability invariant must be documented at the map
(once registry runtimes can be removed — which the leak-by-design
contract forbids — this ADR must be revisited). All existing runtimes,
including those created before `set_admission` existed, resolve cleanly
to the wildcard key.
