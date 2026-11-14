# ADR-0002: Default priority for unconfigured domains

Status: accepted (ratified; implemented in B1)

## Context

Domains that are never passed to `set_admission` still need a priority
when the load signal trips. The default decides how admission treats
the long tail of unconfigured code.

## Options

**A. Medium (proposed).** Unconfigured work is presumed valuable: it
survives `Load::High` and is shed only at `Load::Critical`, after Low
(and any lower tiers) are gone.

**B. High.** Never shed until everything lower is gone — even at
Critical. Safest possible posture for unconfigured code, but it makes
admission nearly inert for adopters who enable the feature without
configuring anything: shedding would only trigger at the worst possible
moment, and callers would never learn the `try_run` rejection path
exists until then.

**C. Low.** Shed first. Hostile default: any adopter who enables
`admission` before configuring priorities watches their own work get
dropped at the first sign of load.

## Decision

A (Medium).

## Consequences

Unconfigured domains keep running through `High` and shed at
`Critical`. The gnostr demo must configure priorities explicitly to
demonstrate shedding at `High` — which is the point of a demo. The
default must be stated loudly in the `set_admission` and `try_run` docs,
because "unconfigured" will be the common case at adoption time.
