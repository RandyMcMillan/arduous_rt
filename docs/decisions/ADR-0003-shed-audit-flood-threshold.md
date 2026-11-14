# ADR-0003: Shed-audit flood threshold

Status: accepted (ratified; implemented in B1)

## Context

Shed decisions are written to the governance audit ring (256 events).
A burst — say 10,000 crawler tasks per second hitting `Shed` — would
evict the entire ring's history within microseconds, defeating the
audit log's purpose.

## Options

**A. Fixed-rate collapse (proposed).** Count shed events per second;
beyond 64, collapse subsequent events in that second into a single
`ShedCollapsed { count }` entry. Simple, bounded, preserves the total
count.

**B. Per-key token bucket.** Smooths bursts per admission key rather
than globally. More faithful, but more code and the same arbitrariness
moved into a second dimension.

**C. Probabilistic sampling (record 1/N).** Statistically
representative at any sustained rate, but exact completeness is lost
and reasoning about "did this specific rejection happen?" gets harder.

**D. No guard.** The ring is FIFO; history erasure during bursts is
accepted. Simplest, but one flood erases exactly the evidence an
operator needs afterward.

## Decision

A, at 64 shed events per second globally, with the collapsed event
carrying the dropped count. The number is a placeholder: the gnostr
demo must measure realistic shed rates and the threshold gets tuned
from data before 1.0.

## Consequences

During a flood the ring retains at least ~4 seconds of shed history
plus all non-shed events, and total shed counts stay exact via
`ShedCollapsed`. If measurement shows legitimate bursts above 64/s are
common, the threshold moves — that is a tuning change, not a design
change.
