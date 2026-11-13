use arduous_rt::{banner, named_runtime};
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};

fn main() {
    #[cfg(feature = "console")]
    console_subscriber::init();

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
