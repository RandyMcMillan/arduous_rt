use arduous_rt::{nested_chain, run};

fn main() {
    for &n in &[1usize, 2, 3, 5, 8] {
        let chain = nested_chain(n);
        let leaf = *chain.last().unwrap();
        let proof = leaf.block_on(async {
            let mut total = 0i64;
            for (idx, rt) in chain.iter().enumerate() {
                total += run(*rt, async move { (idx as i64) + 1 }).await;
            }
            total
        });
        let expected: i64 = (1..=n as i64).sum();
        // Use a banner for visibility.
        println!("============================================================");
        println!(
            " [nested proof] n={n} levels={} proof={} expected={} {}",
            chain.len(),
            proof,
            expected,
            if proof == expected { "OK" } else { "FAIL" }
        );
        println!("============================================================");
        assert_eq!(proof, expected);
    }

    // Determinism proof: repeat and compare.
    let mut results = Vec::new();
    for _ in 0..10 {
        let chain = nested_chain(4);
        let outer = chain[0];
        let r = outer.block_on(async {
            let mut t = 0i64;
            for (i, rt) in chain.iter().enumerate() {
                t += run(*rt, async move { i as i64 }).await;
            }
            t
        });
        results.push(r);
    }
    let first = results[0];
    println!("============================================================");
    println!(
        " [determinism proof] runs={} all_equal={} value={}",
        results.len(),
        results.iter().all(|&r| r == first),
        first
    );
    println!("============================================================");
    assert!(results.iter().all(|&r| r == first));

    println!("============================================================");
    println!(" [nested proofs] ALL PROOFS VERIFIED");
    println!("============================================================");
}
