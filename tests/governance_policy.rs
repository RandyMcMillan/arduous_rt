//! Policy-swap tests for the governance layer. Lives in its own test
//! binary on purpose: `set_policy` replaces global registry state, and
//! cargo runs test binaries sequentially while tests within one binary
//! run in parallel. Every test here takes `policy_lock()` and establishes
//! its own policy first, so no seeding is needed.
#![cfg(feature = "governance")]

#[cfg(feature = "policy-toml")]
use arduous_rt::policy_from_toml;
use arduous_rt::{
    DomainToken, NameError, Policy, PolicyAllow, named_runtime_in, set_policy, try_named_runtime,
};
use std::sync::{Mutex, OnceLock};

fn policy_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

#[test]
fn policy_swap_denies_new_creations_but_keeps_existing() {
    let _guard = policy_lock().lock().unwrap_or_else(|e| e.into_inner());
    set_policy(Policy {
        deny: vec![],
        allow: vec![PolicyAllow {
            pattern: "pol_a.*".into(),
            quota: None,
        }],
    })
    .unwrap();
    assert!(try_named_runtime("pol_a.1").is_ok());

    set_policy(Policy {
        deny: vec!["pol_a.*".into()],
        allow: vec![PolicyAllow {
            pattern: "pol_b.*".into(),
            quota: None,
        }],
    })
    .unwrap();

    // Existing runtimes stay reusable; new creations are denied.
    assert!(try_named_runtime("pol_a.1").is_ok());
    assert!(matches!(
        try_named_runtime("pol_a.2"),
        Err(NameError::Denied { .. })
    ));
    assert!(try_named_runtime("pol_b.1").is_ok());
}

#[test]
fn policy_swap_recomputes_quota_counts() {
    let _guard = policy_lock().lock().unwrap_or_else(|e| e.into_inner());
    set_policy(Policy {
        deny: vec![],
        allow: vec![PolicyAllow {
            pattern: "pol_b.*".into(),
            quota: Some(3),
        }],
    })
    .unwrap();
    assert!(try_named_runtime("pol_b.1").is_ok());
    assert!(try_named_runtime("pol_b.2").is_ok());

    // Tighten the quota: the two existing runtimes still count against it.
    set_policy(Policy {
        deny: vec![],
        allow: vec![PolicyAllow {
            pattern: "pol_b.*".into(),
            quota: Some(2),
        }],
    })
    .unwrap();
    assert!(matches!(
        try_named_runtime("pol_b.3"),
        Err(NameError::QuotaExceeded { max: 2, .. })
    ));
}

#[test]
fn policy_tokens_work_under_swapped_policy() {
    let _guard = policy_lock().lock().unwrap_or_else(|e| e.into_inner());
    set_policy(Policy {
        deny: vec![],
        allow: vec![PolicyAllow {
            pattern: "pol_tok*".into(),
            quota: None,
        }],
    })
    .unwrap();
    let token = DomainToken::acquire("pol_tok").unwrap();
    assert!(named_runtime_in(&token, "pol_tok/worker").is_ok());
    assert!(matches!(
        named_runtime_in(&token, "elsewhere"),
        Err(NameError::OutsideDomain { .. })
    ));
}

#[cfg(feature = "policy-toml")]
#[test]
fn policy_toml_parses_validates_and_applies() {
    let _guard = policy_lock().lock().unwrap_or_else(|e| e.into_inner());
    let policy = policy_from_toml(
        r#"
deny = ["toml_demo.blocked.*"]

[[allow]]
pattern = "toml_demo.*"
quota = 4
"#,
    )
    .unwrap();
    set_policy(policy).unwrap();
    assert!(try_named_runtime("toml_demo.x").is_ok());
    assert!(matches!(
        try_named_runtime("toml_demo.blocked.y"),
        Err(NameError::Denied { .. })
    ));

    assert!(matches!(
        policy_from_toml("deny = [\"toml_demo.[bad\"]"),
        Err(arduous_rt::PolicyError::Invalid(_))
    ));
    assert!(matches!(
        policy_from_toml("not = [valid"),
        Err(arduous_rt::PolicyError::Parse(_))
    ));
}
