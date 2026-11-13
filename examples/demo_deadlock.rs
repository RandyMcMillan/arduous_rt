// Intentionally demonstrates a deadlock: block_on inside an async task on the same runtime.
// Run with a timeout, e.g. `timeout 5s cargo run --example demo_deadlock`.

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
