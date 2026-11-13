//! Demonstrates channel backpressure: a fast producer fills a bounded
//! (capacity 2) mpsc channel and blocks until the slow consumer catches up.
//!
//! Run:
//! cargo run --example demo_backpressure
//!
//! Expected: completes after ~2s; "Sending item N..." and "Received item N"
//! lines interleave once the buffer fills.

use arduous_rt::{banner, named_runtime};
use tokio::sync::mpsc;
use tokio::time::{Duration, sleep};

fn main() {
    #[cfg(feature = "console")]
    console_subscriber::init();
    #[cfg(not(feature = "console"))]
    tracing_subscriber::fmt::init();

    let rt = named_runtime("demo_backpressure");
    banner("Triggering Channel Backpressure Demo...");

    rt.block_on(async {
        let (tx, mut rx) = mpsc::channel::<usize>(2);

        rt.spawn(async move {
            for i in 1..=10 {
                println!("Sending item {i}...");
                if tx.send(i).await.is_err() {
                    println!("Receiver dropped! Send failed at item {i}");
                    break;
                }
            }
        });

        rt.spawn(async move {
            while let Some(val) = rx.recv().await {
                println!("Received item {val}");
                sleep(Duration::from_millis(200)).await;
            }
        })
        .await
        .unwrap();
    });
}
