// Intentionally demonstrates task starvation: a CPU-bound loop never yields.
// Run with a timeout, e.g. `timeout 5s cargo run --example demo_starvation`.

use arduous_rt::{banner, named_runtime};
use tokio::time::Duration;

fn main() {
    #[cfg(feature = "console")]
    console_subscriber::init();

    let rt = named_runtime("demo_starvation");
    banner("Triggering Task Starvation Demo...");

    rt.block_on(async {
        rt.spawn(async {
            println!("Starting CPU-heavy loop without yielding...");
            let mut x = 0u64;
            loop {
                x = x.wrapping_add(1);
                // Intentionally missing: tokio::task::yield_now().await;
            }
        });

        let victim = rt.spawn(async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            println!("Victim task executed (you might not see this if starved).");
        });

        let _ = victim.await;
    });
}
