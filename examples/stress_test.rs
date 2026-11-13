//! Stress workload: spawns 64 tasks per runtime across a chain sized to the
//! machine's parallelism, then aggregates a deterministic checksum.
//!
//! Run:
//! cargo run --example stress_test
//!
//! Expected: completes with "ALL STRESS CHECKS PASSED" and a result/equal
//! proof banner.

use arduous_rt::{banner, stress, stress_chain};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    let runtimes = stress_chain();
    let depth = runtimes.len();
    let tasks_per = 64usize;

    banner(&format!(
        "[stress example] depth={depth} tasks_per={tasks_per} total_tasks={}",
        depth * tasks_per
    ));

    let result = stress(&runtimes, tasks_per).await;

    let mut expected = 0i64;
    for ri in 0..depth {
        for ti in 0..tasks_per {
            expected += (ri * tasks_per + ti) as i64;
        }
    }

    banner(&format!(
        "[stress proof] depth={depth} tasks_per={tasks_per} result={result} expected={expected} {}",
        if result == expected { "OK" } else { "FAIL" }
    ));
    assert_eq!(result, expected);
    banner("[stress] ALL STRESS CHECKS PASSED");
}
