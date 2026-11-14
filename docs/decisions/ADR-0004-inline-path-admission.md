# ADR-0004: Admission on the inline execution path

Status: proposed (awaiting ratification)

## Context

`run()` has two execution paths: spawn onto `rt` when called from
elsewhere, and inline polling when the caller is already running on
`rt`. `try_run` must decide whether admission gates the inline path
too.

## Options

**A. Admission gates both paths (proposed).** Admission decides whether
the work executes at all; the execution path afterward is an
implementation detail. A reentrant caller on a loaded system still
occupies a worker thread for the inline poll — exactly the resource
shedding exists to protect.

**B. Inline bypasses admission.** Claims the inline path is "free."
It is not: the future still consumes the current worker's time slice,
and a bypass makes the policy gameable — code that wants to escape
admission can arrange to call from within the target runtime, at which
point the priority classes protect nothing.

## Decision

A. `try_run` may return `Shed` even when the caller is already running
on the target runtime.

## Consequences

Callers of `try_run` must handle rejection regardless of context — the
docs need an explicit note, since "I was already on that runtime" feels
like a reason admission should pass. The gameability argument is the
decisive one: a policy with a context-based bypass is not a policy.
`run()` remains ungated forever (ADR scope: only `try_run` admits), so
code that genuinely cannot be shed keeps a documented escape hatch.
