//! Two named runtimes ("producer", "consumer") exchanging messages over a
//! bounded mpsc channel, with the consumer driving the receive loop.
//!
//! Run:
//! cargo run --example cooperating_runtimes
//!
//! Expected: completes; prints the global runtime pointer and the three
//! received messages.

use arduous_rt::{global_rt, named_runtime};
use tokio::sync::mpsc;

fn main() {
    let producer = named_runtime("producer");
    let consumer = named_runtime("consumer");
    let (tx, mut rx) = mpsc::channel::<String>(8);

    let producer_handle = producer.spawn(async move {
        for i in 0..3 {
            tx.send(format!("msg-{i}")).await.unwrap();
        }
    });

    let received = consumer.block_on(async {
        let mut out = Vec::new();
        while let Some(msg) = rx.recv().await {
            out.push(msg);
            if out.len() == 3 {
                break;
            }
        }
        out
    });

    producer.block_on(producer_handle).unwrap();
    println!("global runtime: {:?}", global_rt() as *const _);
    println!("received: {received:?}");
}
