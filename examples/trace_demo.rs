//! Demonstrates `arduous_trace!` events flowing through a `tracing` subscriber.
//!
//! Run:
//! cargo run --example trace_demo
//!
//! Expected: three `INFO` events on stderr — before, inside, and after the
//! global runtime — plus the registry lifecycle events from the library.

use arduous_rt::arduous_trace;

fn main() {
    tracing_subscriber::fmt::init();
    arduous_trace!("trace demo starting");
    let rt = arduous_rt::global_rt();
    rt.block_on(async {
        arduous_trace!("inside runtime");
    });
    arduous_trace!("trace demo done");
}
