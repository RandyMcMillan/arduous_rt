use arduous_rt::{available_threads, banner, named_runtime, nested_chain, run};
use std::panic::PanicHookInfo;
use std::sync::{Arc, Mutex};
use tokio::sync::{Barrier, RwLock, Semaphore, broadcast, mpsc, watch};
use tokio::task::JoinSet;
use tokio::time::{Duration, timeout};

/// Frames every panic fired while the guard is alive with banner lines on
/// stderr, labelled with the owning test name. The previous hook is
/// chained (so the original panic message still prints) and restored on
/// drop. Process-global by nature of `std::panic::set_hook`; the frame is
/// cosmetic and safe under parallel tests — worst case a panic prints
/// unframed or double-framed during overlap.
struct PanicFrame {
    previous: Arc<dyn Fn(&PanicHookInfo<'_>) + Sync + Send>,
}

impl PanicFrame {
    fn new(test_name: &str) -> Self {
        let previous: Arc<dyn Fn(&PanicHookInfo<'_>) + Sync + Send> =
            Arc::from(std::panic::take_hook());
        let chained = previous.clone();
        let title = format!("EXPECTED PANIC (negative test — {test_name})");
        std::panic::set_hook(Box::new(move |info| {
            eprintln!("============================================================");
            eprintln!(" {title}");
            eprintln!("============================================================");
            chained(info);
            eprintln!("============================================================");
            eprintln!(" END EXPECTED PANIC");
            eprintln!("============================================================");
        }));
        Self { previous }
    }
}

impl Drop for PanicFrame {
    fn drop(&mut self) {
        let previous = self.previous.clone();
        std::panic::set_hook(Box::new(move |info| previous(info)));
    }
}

#[test]
fn arduous_deep_nesting() {
    let depth = 32usize;
    let chain = nested_chain(depth);
    assert_eq!(chain.len(), depth);

    let leaf = *chain.last().unwrap();
    let result = leaf.block_on(async {
        let mut total = 0i64;
        for (i, rt) in chain.iter().enumerate() {
            total += run(rt, async move { i as i64 }).await;
        }
        total
    });
    let expected: i64 = (0..depth as i64).sum();
    banner(&format!(
        "[arduous deep_nesting] depth={depth} result={result} expected={expected} {}",
        if result == expected { "OK" } else { "FAIL" }
    ));
    assert_eq!(result, expected);
}

#[test]
fn arduous_recursive_runtime_creation() {
    let outer = named_runtime("arduous_recursive_outer");
    let result = outer.block_on(async {
        // Create a child runtime from within a task and use it.
        let inner = arduous_rt::child_runtime("arduous_recursive_outer", "inner");
        run(inner, async { 7 * 6 }).await
    });
    banner(&format!("[arduous recursive] result={result}"));
    assert_eq!(result, 42);
}

#[test]
fn arduous_concurrent_runtime_creation() {
    let threads = available_threads().clamp(2, 8);
    let mut handles = Vec::new();
    for i in 0..threads {
        handles.push(std::thread::spawn(move || {
            let name = format!("arduous_concurrent_{i}");
            let rt = arduous_rt::named_runtime(&name);
            rt.block_on(async move { i as i64 * 10 })
        }));
    }
    let mut total = 0i64;
    for h in handles {
        total += h.join().unwrap();
    }
    let expected: i64 = (0..threads as i64).map(|i| i * 10).sum();
    banner(&format!(
        "[arduous concurrent_create] threads={threads} total={total} expected={expected} {}",
        if total == expected { "OK" } else { "FAIL" }
    ));
    assert_eq!(total, expected);
}

#[test]
fn arduous_massive_task_spawn() {
    let rt = named_runtime("arduous_massive");
    let n = 10_000usize;
    let result = rt.block_on(async {
        let mut handles = Vec::with_capacity(n);
        for i in 0..n {
            handles.push(rt.spawn(async move { i as i64 }));
        }
        let mut total = 0i64;
        for h in handles {
            total += h.await.unwrap();
        }
        total
    });
    let expected: i64 = (0..n as i64).sum();
    banner(&format!(
        "[arduous massive] n={n} result={result} expected={expected} {}",
        if result == expected { "OK" } else { "FAIL" }
    ));
    assert_eq!(result, expected);
}

#[test]
fn arduous_cross_runtime_channel_stress() {
    let depth = available_threads().clamp(2, 8);
    let runtimes = nested_chain(depth);
    let tasks_per = 100usize;
    let (tx, mut rx) = mpsc::channel::<i64>(1024);

    // Spawn producers on each runtime.
    let mut producer_handles = Vec::new();
    for (ri, rt) in runtimes.iter().enumerate() {
        for ti in 0..tasks_per {
            let tx = tx.clone();
            let rt = *rt;
            let value = (ri * tasks_per + ti) as i64;
            producer_handles.push(rt.spawn(async move {
                tx.send(value).await.unwrap();
            }));
        }
    }
    drop(tx);

    let consumer_rt = runtimes[0];
    let received = consumer_rt.block_on(async {
        let mut vals = Vec::new();
        while let Some(v) = rx.recv().await {
            vals.push(v);
        }
        vals
    });

    // Every send completed (the channel closed after all values were received).
    consumer_rt.block_on(async {
        for h in producer_handles {
            h.await.unwrap();
        }
    });

    let mut total = 0i64;
    for v in &received {
        total += *v;
    }
    let mut expected = 0i64;
    for ri in 0..depth {
        for ti in 0..tasks_per {
            expected += (ri * tasks_per + ti) as i64;
        }
    }
    banner(&format!(
        "[arduous channel_stress] depth={depth} tasks_per={tasks_per} received={} total={total} expected={expected} {}",
        received.len(),
        if total == expected && received.len() == depth * tasks_per {
            "OK"
        } else {
            "FAIL"
        }
    ));
    assert_eq!(received.len(), depth * tasks_per);
    assert_eq!(total, expected);
}

#[test]
fn arduous_shared_state_mutex() {
    let depth = available_threads().clamp(2, 8);
    let runtimes = nested_chain(depth);
    let counter = Arc::new(Mutex::new(0i64));
    let tasks_per = 50usize;
    let mut handles = Vec::new();
    for rt in &runtimes {
        for _ in 0..tasks_per {
            let c = Arc::clone(&counter);
            let rt = *rt;
            handles.push(rt.spawn(async move {
                let mut g = c.lock().unwrap();
                *g += 1;
            }));
        }
    }
    let rt0 = runtimes[0];
    rt0.block_on(async {
        for h in handles {
            h.await.unwrap();
        }
    });
    let final_val = *counter.lock().unwrap();
    let expected = (depth * tasks_per) as i64;
    banner(&format!(
        "[arduous shared_mutex] depth={depth} tasks_per={tasks_per} final={final_val} expected={expected} {}",
        if final_val == expected { "OK" } else { "FAIL" }
    ));
    assert_eq!(final_val, expected);
}

#[test]
fn arduous_barrier_sync() {
    let depth = available_threads().clamp(2, 8);
    let runtimes = nested_chain(depth);
    let barrier = Arc::new(Barrier::new(depth));
    let mut handles = Vec::new();
    for rt in &runtimes {
        let b = Arc::clone(&barrier);
        let rt = *rt;
        handles.push(rt.spawn(async move {
            b.wait().await;
            1i64
        }));
    }
    let rt0 = runtimes[0];
    let total = rt0.block_on(async {
        let mut t = 0i64;
        for h in handles {
            t += h.await.unwrap();
        }
        t
    });
    banner(&format!(
        "[arduous barrier] depth={depth} total={total} expected={depth} {}",
        if total == depth as i64 { "OK" } else { "FAIL" }
    ));
    assert_eq!(total, depth as i64);
}

#[test]
fn arduous_timeout_and_cancellation() {
    let rt = named_runtime("arduous_timeout");
    // Timeout on a future that never completes.
    let res = rt.block_on(async {
        timeout(Duration::from_millis(50), async {
            tokio::time::sleep(Duration::from_secs(10)).await;
            42i64
        })
        .await
    });
    banner(&format!("[arduous timeout] result={:?}", res));
    assert!(res.is_err());

    // Abort a spawned task.
    let handle = rt.spawn(async {
        tokio::time::sleep(Duration::from_secs(10)).await;
        99i64
    });
    handle.abort();
    let aborted = rt.block_on(handle);
    banner(&format!("[arduous abort] result={:?}", aborted));
    assert!(aborted.unwrap_err().is_cancelled());
}

#[test]
fn arduous_error_propagation() {
    let rt = named_runtime("arduous_error");
    banner("[negative test] arduous_error_propagation: intentional task panic expected below");
    let _panic_frame = PanicFrame::new("arduous_error_propagation");
    let handle = rt.spawn(async {
        panic!("boom");
    });
    let res = rt.block_on(handle);
    banner(&format!("[arduous error] result={:?}", res));
    assert!(res.is_err());
}

#[test]
fn arduous_spawn_blocking_cpu() {
    let rt = named_runtime("arduous_spawn_blocking");
    let result = rt.block_on(async {
        let h = rt.spawn_blocking(|| {
            // CPU-ish work
            (0..100_000i64).sum::<i64>()
        });
        h.await.unwrap()
    });
    let expected: i64 = (0..100_000i64).sum();
    banner(&format!(
        "[arduous spawn_blocking] result={result} expected={expected} {}",
        if result == expected { "OK" } else { "FAIL" }
    ));
    assert_eq!(result, expected);
}

fn reentrant_sum(
    rt: &'static tokio::runtime::Runtime,
    n: usize,
    acc: i64,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = i64> + Send>> {
    Box::pin(async move {
        if n == 0 {
            acc
        } else {
            let next = run(rt, async move { acc + 1 }).await;
            reentrant_sum(rt, n - 1, next).await
        }
    })
}

#[test]
fn arduous_reentrancy_under_load() {
    let rt = named_runtime("arduous_reentrancy3");
    let depth = 200usize;
    let result = rt.block_on(reentrant_sum(rt, depth, 0));
    banner(&format!(
        "[arduous reentrancy] depth={depth} result={result} expected={depth} {}",
        if result == depth as i64 { "OK" } else { "FAIL" }
    ));
    assert_eq!(result, depth as i64);
}

#[test]
fn arduous_high_fanout() {
    let depth = available_threads().clamp(2, 8);
    let runtimes = nested_chain(depth);
    let tasks_per = 64usize;
    let mut handles = Vec::new();
    for (ri, rt) in runtimes.iter().enumerate() {
        for ti in 0..tasks_per {
            let rt = *rt;
            let value = (ri * tasks_per + ti) as i64;
            handles.push(rt.spawn(async move { value }));
        }
    }
    let rt0 = runtimes[0];
    let total = rt0.block_on(async {
        let mut t = 0i64;
        for h in handles {
            t += h.await.unwrap();
        }
        t
    });
    let mut expected = 0i64;
    for ri in 0..depth {
        for ti in 0..tasks_per {
            expected += (ri * tasks_per + ti) as i64;
        }
    }
    banner(&format!(
        "[arduous high_fanout] depth={depth} tasks_per={tasks_per} total={total} expected={expected} {}",
        if total == expected { "OK" } else { "FAIL" }
    ));
    assert_eq!(total, expected);
}

#[test]
fn arduous_randomized_delays_deterministic() {
    let rt = named_runtime("arduous_rand_delays2");
    let n = 200usize;
    let result = rt.block_on(async {
        let mut handles = Vec::with_capacity(n);
        for i in 0..n {
            handles.push(rt.spawn(async move {
                let delay_ms = (i as u64 * 37 % 5) + 1;
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                i as i64
            }));
        }
        let mut total = 0i64;
        for h in handles {
            total += h.await.unwrap();
        }
        total
    });
    let expected: i64 = (0..n as i64).sum();
    banner(&format!(
        "[arduous rand_delays] n={n} result={result} expected={expected} {}",
        if result == expected { "OK" } else { "FAIL" }
    ));
    assert_eq!(result, expected);
}

#[test]
fn arduous_broadcast_across_runtimes() {
    let depth = available_threads().clamp(2, 8);
    let runtimes = nested_chain(depth);
    let (tx, _) = broadcast::channel::<i64>(1024);
    let mut rxs = Vec::new();
    for _ in 0..depth {
        rxs.push(tx.subscribe());
    }

    let msg = 42i64;
    let sender_rt = *runtimes.last().unwrap();
    sender_rt.block_on(async {
        tx.send(msg).unwrap();
    });

    let rt0 = runtimes[0];
    let received_sum = rt0.block_on(async {
        let mut total = 0i64;
        for mut rx in rxs {
            if let Ok(v) = rx.recv().await {
                total += v;
            }
        }
        total
    });
    let expected = msg * depth as i64;
    banner(&format!(
        "[arduous broadcast] depth={depth} received_sum={received_sum} expected={expected} {}",
        if received_sum == expected {
            "OK"
        } else {
            "FAIL"
        }
    ));
    assert_eq!(received_sum, expected);
}

#[test]
fn arduous_watch_coordination() {
    let rt = named_runtime("arduous_watch2");
    let (tx, rx) = watch::channel(0i64);
    let result = rt.block_on(async {
        let mut rx2 = rx.clone();
        let h = rt.spawn(async move {
            while *rx2.borrow() != 99 {
                rx2.changed().await.unwrap();
            }
            *rx2.borrow()
        });
        tx.send(99).unwrap();
        h.await.unwrap()
    });
    banner(&format!("[arduous watch] result={result}"));
    assert_eq!(result, 99);
}

#[test]
fn arduous_semaphore_backpressure() {
    let rt = named_runtime("arduous_semaphore2");
    let sem = Arc::new(Semaphore::new(4));
    let n = 50usize;
    let result = rt.block_on(async {
        let mut handles = Vec::with_capacity(n);
        for i in 0..n {
            let sem = Arc::clone(&sem);
            handles.push(rt.spawn(async move {
                let _permit = sem.acquire().await.unwrap();
                tokio::time::sleep(Duration::from_millis(1)).await;
                i as i64
            }));
        }
        let mut total = 0i64;
        for h in handles {
            total += h.await.unwrap();
        }
        total
    });
    let expected: i64 = (0..n as i64).sum();
    banner(&format!(
        "[arduous semaphore] n={n} result={result} expected={expected} {}",
        if result == expected { "OK" } else { "FAIL" }
    ));
    assert_eq!(result, expected);
}

#[test]
fn arduous_rwlock_stress() {
    let rt = named_runtime("arduous_rwlock2");
    let lock = Arc::new(RwLock::new(0i64));
    let n = 100usize;
    let result = rt.block_on(async {
        let mut handles = Vec::with_capacity(n);
        for i in 0..n {
            let lock = Arc::clone(&lock);
            if i % 2 == 0 {
                handles.push(rt.spawn(async move {
                    let mut g = lock.write().await;
                    *g += 1;
                }));
            } else {
                let lock = Arc::clone(&lock);
                handles.push(rt.spawn(async move {
                    let g = lock.read().await;
                    let _ = *g;
                }));
            }
        }
        for h in handles {
            let _ = h.await;
        }
        *lock.read().await
    });
    let expected = (n / 2) as i64;
    banner(&format!(
        "[arduous rwlock] final={} expected={} {}",
        result,
        expected,
        if result == expected { "OK" } else { "FAIL" }
    ));
    assert_eq!(result, expected);
}

#[test]
fn arduous_abort_storm() {
    let rt = named_runtime("arduous_abort_storm2");
    let n = 200usize;
    let result = rt.block_on(async {
        let mut handles = Vec::with_capacity(n);
        for _ in 0..n {
            handles.push(rt.spawn(async {
                tokio::time::sleep(Duration::from_secs(10)).await;
                1i64
            }));
        }
        for h in handles.iter().take(n / 2) {
            h.abort();
        }
        let mut cancelled = 0usize;
        let mut completed = 0usize;
        for h in handles {
            match h.await {
                Err(e) if e.is_cancelled() => cancelled += 1,
                Ok(_) => completed += 1,
                _ => {}
            }
        }
        (cancelled, completed)
    });
    banner(&format!(
        "[arduous abort_storm] cancelled={} completed={}",
        result.0, result.1
    ));
    assert_eq!(result.0, n / 2);
}

#[test]
fn arduous_nested_fanout_tree() {
    fn spawn_tree(
        rt: &'static tokio::runtime::Runtime,
        depth: usize,
        breadth: usize,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = i64> + Send>> {
        Box::pin(async move {
            if depth == 0 {
                return 1i64;
            }
            let mut handles = Vec::new();
            for _ in 0..breadth {
                let child = arduous_rt::child_runtime("tree2", &format!("d{depth}"));
                let h = rt.spawn(async move { spawn_tree(child, depth - 1, breadth).await });
                handles.push(h);
            }
            let mut total = 0i64;
            for h in handles {
                total += h.await.unwrap();
            }
            total
        })
    }

    let rt = named_runtime("arduous_tree_root2");
    let depth = 4usize;
    let breadth = 3usize;
    let result = rt.block_on(spawn_tree(rt, depth, breadth));
    let expected = breadth.pow(depth as u32) as i64;
    banner(&format!(
        "[arduous tree] depth={depth} breadth={breadth} result={result} expected={expected} {}",
        if result == expected { "OK" } else { "FAIL" }
    ));
    assert_eq!(result, expected);
}

#[test]
fn arduous_join_handle_cross_runtime() {
    let rt_a = named_runtime("arduous_cross_a2");
    let rt_b = named_runtime("arduous_cross_b2");
    let handle = rt_a.spawn(async { 123i64 });
    let result = rt_b.block_on(async { handle.await.unwrap() });
    banner(&format!("[arduous cross_join] result={result}"));
    assert_eq!(result, 123);
}

#[test]
fn arduous_channel_streaming_large() {
    let rt_a = named_runtime("arduous_stream_a2");
    let rt_b = named_runtime("arduous_stream_b2");
    let (tx, mut rx) = mpsc::channel::<i64>(1024);
    let n = 100_000usize;

    let producer = rt_a.spawn(async move {
        for i in 0..n as i64 {
            tx.send(i).await.unwrap();
        }
    });

    let result = rt_b.block_on(async {
        let mut total = 0i64;
        let mut count = 0usize;
        while let Some(v) = rx.recv().await {
            total += v;
            count += 1;
            if count == n {
                break;
            }
        }
        (count, total)
    });

    let _ = rt_a.block_on(producer);
    let expected: i64 = (0..n as i64).sum();
    banner(&format!(
        "[arduous stream] count={} total={} expected={} {}",
        result.0,
        result.1,
        expected,
        if result.0 == n && result.1 == expected {
            "OK"
        } else {
            "FAIL"
        }
    ));
    assert_eq!(result.0, n);
    assert_eq!(result.1, expected);
}

#[test]
fn arduous_joinset_fanout() {
    let rt = named_runtime("arduous_joinset");
    let n = 1_000usize;
    let result = rt.block_on(async {
        let mut set = JoinSet::new();
        for i in 0..n {
            set.spawn(async move { i as i64 });
        }
        let mut total = 0i64;
        while let Some(res) = set.join_next().await {
            total += res.unwrap();
        }
        total
    });
    let expected: i64 = (0..n as i64).sum();
    banner(&format!(
        "[arduous joinset] n={n} result={result} expected={expected} {}",
        if result == expected { "OK" } else { "FAIL" }
    ));
    assert_eq!(result, expected);
}

#[test]
fn arduous_nested_timeouts() {
    let rt = named_runtime("arduous_nested_timeouts");
    let result = rt.block_on(async {
        // Outer timeout wraps an inner timeout that fires first.

        timeout(Duration::from_millis(200), async {
            timeout(Duration::from_millis(50), async {
                tokio::time::sleep(Duration::from_secs(10)).await;
                1i64
            })
            .await
        })
        .await
    });
    banner(&format!("[arduous nested_timeouts] result={:?}", result));
    assert!(result.is_ok());
    assert!(result.unwrap().is_err());
}

#[test]
fn arduous_panic_recovery() {
    let rt = named_runtime("arduous_panic_recovery");
    banner(
        "[negative test] arduous_panic_recovery: intentional panics for i % 5 == 0 expected below",
    );
    let _panic_frame = PanicFrame::new("arduous_panic_recovery");
    let n = 50usize;
    let result = rt.block_on(async {
        let mut handles = Vec::with_capacity(n);
        for i in 0..n {
            handles.push(rt.spawn(async move {
                if i % 5 == 0 {
                    panic!("boom {i}");
                }
                i as i64
            }));
        }
        let mut ok_sum = 0i64;
        let mut panics = 0usize;
        for h in handles {
            match h.await {
                Ok(v) => ok_sum += v,
                Err(e) if e.is_panic() => panics += 1,
                _ => {}
            }
        }
        (ok_sum, panics)
    });
    let expected_panics = n / 5;
    let expected_sum: i64 = (0..n as i64).filter(|&i| i % 5 != 0).sum();
    banner(&format!(
        "[arduous panic_recovery] ok_sum={} expected_sum={} panics={} expected_panics={} {}",
        result.0,
        expected_sum,
        result.1,
        expected_panics,
        if result.0 == expected_sum && result.1 == expected_panics {
            "OK"
        } else {
            "FAIL"
        }
    ));
    assert_eq!(result.0, expected_sum);
    assert_eq!(result.1, expected_panics);
}

#[test]
fn arduous_channel_closed_detection() {
    let rt_a = named_runtime("arduous_closed_a");
    let rt_b = named_runtime("arduous_closed_b");
    let (tx, mut rx) = mpsc::channel::<i64>(8);

    let producer = rt_a.spawn(async move {
        for i in 0..10i64 {
            tx.send(i).await.unwrap();
        }
        // tx dropped here -> channel closes
    });

    let result = rt_b.block_on(async {
        let mut count = 0usize;
        let mut total = 0i64;
        while let Some(v) = rx.recv().await {
            total += v;
            count += 1;
        }
        (count, total)
    });

    let _ = rt_a.block_on(producer);
    banner(&format!(
        "[arduous closed] count={} total={}",
        result.0, result.1
    ));
    assert_eq!(result.0, 10);
    assert_eq!(result.1, 45);
}

#[test]
fn arduous_multi_thread_spawning() {
    let rt = named_runtime("arduous_multi_thread_spawn");
    let handle = rt.handle().clone();
    let threads = available_threads().clamp(2, 8);
    let per_thread = 100usize;
    let mut join_handles = Vec::new();
    for t in 0..threads {
        let h = handle.clone();
        join_handles.push(std::thread::spawn(move || {
            let mut local = Vec::with_capacity(per_thread);
            for i in 0..per_thread {
                let val = (t * per_thread + i) as i64;
                local.push(h.spawn(async move { val }));
            }
            local
        }));
    }

    let mut all = Vec::new();
    for jh in join_handles {
        all.extend(jh.join().unwrap());
    }

    let result = rt.block_on(async {
        let mut total = 0i64;
        for h in all {
            total += h.await.unwrap();
        }
        total
    });
    let expected: i64 = (0..(threads * per_thread) as i64).sum();
    banner(&format!(
        "[arduous multi_thread_spawn] threads={threads} per={per_thread} result={result} expected={expected} {}",
        if result == expected { "OK" } else { "FAIL" }
    ));
    assert_eq!(result, expected);
}

#[test]
fn arduous_large_payload_channel() {
    let rt_a = named_runtime("arduous_large_payload_a");
    let rt_b = named_runtime("arduous_large_payload_b");
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(4);
    let size = 1024 * 1024usize; // 1 MiB
    let count = 8usize;

    let producer = rt_a.spawn(async move {
        for _ in 0..count {
            let payload = vec![0xABu8; size];
            tx.send(payload).await.unwrap();
        }
    });

    let result = rt_b.block_on(async {
        let mut total_bytes = 0usize;
        let mut msgs = 0usize;
        while let Some(p) = rx.recv().await {
            total_bytes += p.len();
            msgs += 1;
            if msgs == count {
                break;
            }
        }
        (msgs, total_bytes)
    });

    let _ = rt_a.block_on(producer);
    banner(&format!(
        "[arduous large_payload] msgs={} bytes={} expected_msgs={} expected_bytes={} {}",
        result.0,
        result.1,
        count,
        size * count,
        if result.0 == count && result.1 == size * count {
            "OK"
        } else {
            "FAIL"
        }
    ));
    assert_eq!(result.0, count);
    assert_eq!(result.1, size * count);
}

#[test]
fn arduous_select_many_channels() {
    let rt = named_runtime("arduous_select_many");
    let n = 100usize;
    let result = rt.block_on(async {
        let mut senders = Vec::with_capacity(n);
        let mut receivers = Vec::with_capacity(n);
        for _ in 0..n {
            let (tx, rx) = mpsc::channel::<i64>(1);
            senders.push(tx);
            receivers.push(rx);
        }

        // Send one value on each channel.
        for (i, tx) in senders.into_iter().enumerate() {
            tx.send(i as i64).await.unwrap();
        }

        // Receive all via select loop using recv on each.
        let mut total = 0i64;
        for mut rx in receivers {
            if let Some(v) = rx.recv().await {
                total += v;
            }
        }
        total
    });
    let expected: i64 = (0..n as i64).sum();
    banner(&format!(
        "[arduous select_many] n={n} result={result} expected={expected} {}",
        if result == expected { "OK" } else { "FAIL" }
    ));
    assert_eq!(result, expected);
}

#[test]
fn arduous_timer_stress() {
    let rt = named_runtime("arduous_timer_stress");
    let n = 1_000usize;
    let result = rt.block_on(async {
        let mut handles = Vec::with_capacity(n);
        for i in 0..n {
            handles.push(rt.spawn(async move {
                tokio::time::sleep(Duration::from_millis(1)).await;
                i as i64
            }));
        }
        let mut total = 0i64;
        for h in handles {
            total += h.await.unwrap();
        }
        total
    });
    let expected: i64 = (0..n as i64).sum();
    banner(&format!(
        "[arduous timer_stress] n={n} result={result} expected={expected} {}",
        if result == expected { "OK" } else { "FAIL" }
    ));
    assert_eq!(result, expected);
}

#[test]
fn arduous_runtime_handle_clone() {
    let rt = named_runtime("arduous_handle_clone");
    let cloned = rt.handle().clone();
    let h1 = rt.spawn(async { 1i64 });
    let h2 = cloned.spawn(async { 2i64 });
    let result = rt.block_on(async { h1.await.unwrap() + h2.await.unwrap() });
    banner(&format!("[arduous handle_clone] result={result}"));
    assert_eq!(result, 3);
}

#[test]
fn arduous_deadline_cascade() {
    let rt = named_runtime("arduous_deadline_cascade");
    let result = rt.block_on(async {
        // Three nested timeouts; innermost completes in time, middle allows, outer allows.

        timeout(Duration::from_millis(200), async {
            timeout(Duration::from_millis(150), async {
                timeout(Duration::from_millis(100), async {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    7i64
                })
                .await
            })
            .await
        })
        .await
    });
    banner(&format!("[arduous deadline_cascade] result={:?}", result));
    assert_eq!(result.unwrap().unwrap().unwrap(), 7);
}

#[test]
fn arduous_idempotent_named_runtime() {
    let name = "arduous_idempotent_shared";
    let threads = available_threads().clamp(2, 8);
    let mut handles = Vec::new();
    for _ in 0..threads {
        handles.push(std::thread::spawn(move || {
            arduous_rt::named_runtime(name) as *const _ as usize
        }));
    }
    let mut ptrs = Vec::new();
    for h in handles {
        ptrs.push(h.join().unwrap());
    }
    let first = ptrs[0];
    banner(&format!(
        "[arduous idempotent] threads={threads} all_same={}",
        ptrs.iter().all(|&p| p == first)
    ));
    assert!(ptrs.iter().all(|&p| p == first));
}

#[test]
fn arduous_atomic_counter_cross_runtime() {
    use std::sync::atomic::{AtomicU64, Ordering};
    let depth = available_threads().clamp(2, 8);
    let runtimes = nested_chain(depth);
    let counter = Arc::new(AtomicU64::new(0));
    let tasks_per = 100usize;
    let mut handles = Vec::new();
    for rt in &runtimes {
        for _ in 0..tasks_per {
            let c = Arc::clone(&counter);
            let rt = *rt;
            handles.push(rt.spawn(async move {
                c.fetch_add(1, Ordering::SeqCst);
            }));
        }
    }
    let rt0 = runtimes[0];
    rt0.block_on(async {
        for h in handles {
            h.await.unwrap();
        }
    });
    let final_val = counter.load(Ordering::SeqCst);
    let expected = (depth * tasks_per) as u64;
    banner(&format!(
        "[arduous atomic] depth={depth} tasks_per={tasks_per} final={final_val} expected={expected} {}",
        if final_val == expected { "OK" } else { "FAIL" }
    ));
    assert_eq!(final_val, expected);
}

#[test]
fn arduous_fanin_bounded_channel() {
    let depth = available_threads().clamp(2, 8);
    let runtimes = nested_chain(depth);
    let tasks_per = 50usize;
    let (tx, mut rx) = mpsc::channel::<i64>(64);

    let mut producers = Vec::new();
    for (ri, rt) in runtimes.iter().enumerate() {
        for ti in 0..tasks_per {
            let tx = tx.clone();
            let rt = *rt;
            let value = (ri * tasks_per + ti) as i64;
            producers.push(rt.spawn(async move {
                tx.send(value).await.unwrap();
            }));
        }
    }
    drop(tx);

    let rt0 = runtimes[0];
    let (count, total) = rt0.block_on(async {
        let mut count = 0usize;
        let mut total = 0i64;
        while let Some(v) = rx.recv().await {
            total += v;
            count += 1;
            if count == depth * tasks_per {
                break;
            }
        }
        (count, total)
    });

    // Channel closed only after every producer finished sending.
    rt0.block_on(async {
        for p in producers {
            p.await.unwrap();
        }
    });

    let mut expected = 0i64;
    for ri in 0..depth {
        for ti in 0..tasks_per {
            expected += (ri * tasks_per + ti) as i64;
        }
    }
    banner(&format!(
        "[arduous fanin] depth={depth} tasks_per={tasks_per} count={count} total={total} expected={expected} {}",
        if count == depth * tasks_per && total == expected {
            "OK"
        } else {
            "FAIL"
        }
    ));
    assert_eq!(count, depth * tasks_per);
    assert_eq!(total, expected);
}

#[test]
fn arduous_broadcast_fanout() {
    let depth = available_threads().clamp(2, 8);
    let runtimes = nested_chain(depth);
    let (tx, _) = broadcast::channel::<i64>(1024);
    let mut rxs: Vec<_> = (0..depth).map(|_| tx.subscribe()).collect();

    let rt_last = *runtimes.last().unwrap();
    rt_last.block_on(async {
        tx.send(7).unwrap();
    });

    // Receive on the first runtime across all receivers.
    let rt0 = runtimes[0];
    let received = rt0.block_on(async {
        let mut total = 0i64;
        for rx in &mut rxs {
            if let Ok(v) = rx.recv().await {
                total += v;
            }
        }
        total
    });
    let expected = 7 * depth as i64;
    banner(&format!(
        "[arduous broadcast_fanout] depth={depth} received={received} expected={expected} {}",
        if received == expected { "OK" } else { "FAIL" }
    ));
    assert_eq!(received, expected);
}

#[test]
fn arduous_nested_task_cancellation() {
    let outer = named_runtime("arduous_nested_cancel_outer");
    let inner = arduous_rt::child_runtime("arduous_nested_cancel_outer", "inner");

    let result = outer.block_on(async {
        let mut handles = Vec::new();
        for _ in 0..10 {
            let h = inner.spawn(async {
                tokio::time::sleep(Duration::from_secs(10)).await;
                1i64
            });
            handles.push(h);
        }
        // Cancel all.
        for h in &handles {
            h.abort();
        }
        let mut cancelled = 0usize;
        for h in handles {
            if let Err(e) = h.await
                && e.is_cancelled()
            {
                cancelled += 1;
            }
        }
        cancelled
    });
    banner(&format!(
        "[arduous nested_cancel] cancelled={} expected=10 {}",
        result,
        if result == 10 { "OK" } else { "FAIL" }
    ));
    assert_eq!(result, 10);
}

#[test]
fn arduous_retry_backoff() {
    let rt = named_runtime("arduous_retry");
    let result = rt.block_on(async {
        let mut attempts = 0u32;
        loop {
            attempts += 1;
            // Fail first 3 attempts.
            if attempts < 4 {
                tokio::time::sleep(Duration::from_millis(1)).await;
                continue;
            }
            break attempts;
        }
    });
    banner(&format!("[arduous retry] attempts={result}"));
    assert_eq!(result, 4);
}

#[test]
fn arduous_ordered_sequence() {
    let rt_a = named_runtime("arduous_ordered_a");
    let rt_b = named_runtime("arduous_ordered_b");
    let (tx, mut rx) = mpsc::channel::<usize>(16);
    let n = 1_000usize;

    let producer = rt_a.spawn(async move {
        for i in 0..n {
            tx.send(i).await.unwrap();
        }
    });

    let (count, ordered) = rt_b.block_on(async {
        let mut count = 0usize;
        let mut ordered = true;
        let mut expected = 0usize;
        while let Some(v) = rx.recv().await {
            if v != expected {
                ordered = false;
            }
            expected += 1;
            count += 1;
            if count == n {
                break;
            }
        }
        (count, ordered)
    });

    let _ = rt_a.block_on(producer);
    banner(&format!(
        "[arduous ordered] count={count} ordered={ordered} {}",
        if count == n && ordered { "OK" } else { "FAIL" }
    ));
    assert_eq!(count, n);
    assert!(ordered);
}

#[test]
fn arduous_joinset_with_errors() {
    let rt = named_runtime("arduous_joinset_errors");
    banner(
        "[negative test] arduous_joinset_with_errors: intentional panics for i % 7 == 0 expected below",
    );
    let _panic_frame = PanicFrame::new("arduous_joinset_with_errors");
    let n = 100usize;
    let result = rt.block_on(async {
        let mut set = JoinSet::new();
        for i in 0..n {
            set.spawn(async move {
                if i % 7 == 0 {
                    panic!("fail {i}");
                }
                i as i64
            });
        }
        let mut ok_sum = 0i64;
        let mut errors = 0usize;
        while let Some(res) = set.join_next().await {
            match res {
                Ok(v) => ok_sum += v,
                Err(_) => errors += 1,
            }
        }
        (ok_sum, errors)
    });
    let expected_errors = (0..n).filter(|&i| i % 7 == 0).count();
    let expected_sum: i64 = (0..n as i64).filter(|&i| i % 7 != 0).sum();
    banner(&format!(
        "[arduous joinset_errors] ok_sum={} expected={} errors={} expected_errors={} {}",
        result.0,
        expected_sum,
        result.1,
        expected_errors,
        if result.0 == expected_sum && result.1 == expected_errors {
            "OK"
        } else {
            "FAIL"
        }
    ));
    assert_eq!(result.0, expected_sum);
    assert_eq!(result.1, expected_errors);
}

#[test]
fn arduous_deep_async_recursion() {
    fn recurse(
        rt: &'static tokio::runtime::Runtime,
        n: usize,
        acc: i64,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = i64> + Send>> {
        Box::pin(async move {
            if n == 0 {
                acc
            } else {
                let next = run(rt, async move { acc + 1 }).await;
                recurse(rt, n - 1, next).await
            }
        })
    }

    let rt = named_runtime("arduous_deep_recursion");
    let depth = 500usize;
    let result = rt.block_on(recurse(rt, depth, 0));
    banner(&format!(
        "[arduous deep_recursion] depth={depth} result={result} expected={depth} {}",
        if result == depth as i64 { "OK" } else { "FAIL" }
    ));
    assert_eq!(result, depth as i64);
}

#[test]
fn arduous_runtime_handle_clone_stress() {
    let rt = named_runtime("arduous_handle_clone_stress");
    let base = rt.handle().clone();
    let threads = available_threads().clamp(2, 8);
    let per_thread = 50usize;
    let mut join_handles = Vec::new();
    for t in 0..threads {
        let h = base.clone();
        join_handles.push(std::thread::spawn(move || {
            let mut locals = Vec::with_capacity(per_thread);
            for i in 0..per_thread {
                let val = (t * per_thread + i) as i64;
                locals.push(h.spawn(async move { val }));
            }
            locals
        }));
    }
    let mut all = Vec::new();
    for jh in join_handles {
        all.extend(jh.join().unwrap());
    }
    let result = rt.block_on(async {
        let mut total = 0i64;
        for h in all {
            total += h.await.unwrap();
        }
        total
    });
    let expected: i64 = (0..(threads * per_thread) as i64).sum();
    banner(&format!(
        "[arduous handle_clone_stress] threads={threads} per={per_thread} result={result} expected={expected} {}",
        if result == expected { "OK" } else { "FAIL" }
    ));
    assert_eq!(result, expected);
}
