//! Intentionally demonstrates a deadlock: `block_on` called inside an async
//! task running on the same runtime blocks the worker thread forever.
//!
//! Run under a timeout:
//! timeout 5s cargo run --example demo_deadlock
//!
//! Expected: hangs until the timeout kills it. The fix is `arduous_rt::run`
//! (inline-polls when already on the target runtime) or `spawn`.

use arduous_rt::{banner, named_runtime};

fn main() {
    #[cfg(feature = "console")]
    console_subscriber::init();
    #[cfg(not(feature = "console"))]
    tracing_subscriber::fmt::init();

    let rt = named_runtime("demo_deadlock");
    banner("Triggering Deadlock Demo...");

    rt.block_on(async {
        let handle = rt.spawn(async {
            println!("Spawned task trying to block on its own runtime...");
            // Deadlocks: the worker thread executing this task is blocked,
            // so the inner future can never be polled.
            let _ = rt.block_on(async { 42 });
        });
        let _ = handle.await;
    });
}
