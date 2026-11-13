use arduous_rt::{banner, named_runtime};
use std::time::Duration;

fn main() {
    #[cfg(feature = "console")]
    console_subscriber::init();
    #[cfg(not(feature = "console"))]
    tracing_subscriber::fmt::init();

    let rt = named_runtime("demo_panic");
    banner("Triggering Panic & Abort Storm Demo...");

    rt.block_on(async {
        let handle = rt.spawn(async {
            println!("Task running normally...");
            tokio::time::sleep(Duration::from_millis(50)).await;
            panic!("Intentional demo worker panic!");
        });

        match handle.await {
            Ok(_) => println!("Task completed successfully"),
            Err(e) if e.is_panic() => println!("Caught expected task panic: {e:?}"),
            Err(e) => println!("Task failed/cancelled: {e:?}"),
        }
    });
}
