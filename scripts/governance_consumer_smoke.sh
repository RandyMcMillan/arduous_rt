#!/usr/bin/env bash
# Downstream consumer smoke test for the governance layer.
#
# Builds a throwaway project that depends on arduous_rt and runs a full
# governance flow: TOML policy loading, domain tokens with sub-token
# delegation, quota and deny enforcement, and the audit log.
#
# Usage:
#   scripts/governance_consumer_smoke.sh          # local crate (path dep)
#   scripts/governance_consumer_smoke.sh 0.8.1    # published version from crates.io
#
# Note: published versions before 0.8.1 fail the domain-root acquisition
# (acquire("org.gnostr") under pattern "org.gnostr.*") — that sharp edge
# is exactly what this script was written to catch (found against 0.8.0).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="${1:-}"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
cd "$TMP"

cargo init -q --name gov_consumer
if [ -n "$VERSION" ]; then
    # '=' pins the exact release; a bare version would be a caret range
    # and silently float to the newest patch (defeating the matrix).
    cargo add "arduous_rt@=$VERSION" --features governance,policy-toml -q
else
    cargo add arduous_rt --path "$ROOT" --features governance,policy-toml -q
fi

cat > src/main.rs <<'RUST'
fn main() {
    // Declarative policy: allow the gnostr domain with a runtime quota,
    // deny the experimental branch. `/` separates hierarchy levels, so
    // `org.gnostr/**` covers the whole subtree including its root.
    let policy = arduous_rt::policy_from_toml(
        r#"
deny = ["org.gnostr/experimental/**"]

[[allow]]
pattern = "org.gnostr/**"
quota = 3
"#,
    )
    .unwrap();
    arduous_rt::set_policy(policy).unwrap();

    // Capability delegation: root -> wallet sub-domain. The domain root
    // is authorized by the subtree pattern.
    let root = arduous_rt::DomainToken::acquire("org.gnostr").unwrap();
    let wallet = root.mint("wallet").unwrap();
    let rt = arduous_rt::named_runtime_in(&wallet, "org.gnostr/wallet").unwrap();
    let value = rt.block_on(async { 40 + 2 });
    assert_eq!(value, 42);

    // Quota enforcement and denial.
    assert!(arduous_rt::try_named_runtime("org.gnostr/relay").is_ok());
    assert!(arduous_rt::try_named_runtime("org.gnostr/indexer").is_ok());
    assert!(matches!(
        arduous_rt::try_named_runtime("org.gnostr/fourth"),
        Err(arduous_rt::NameError::QuotaExceeded { max: 3, .. })
    ));
    assert!(matches!(
        arduous_rt::try_named_runtime("org.gnostr/experimental/dev"),
        Err(arduous_rt::NameError::Denied { .. })
    ));

    // The audit trail recorded everything.
    let kinds: Vec<_> = arduous_rt::audit_events().iter().map(|e| e.kind).collect();
    assert!(kinds.contains(&arduous_rt::AuditKind::Created));
    assert!(kinds.contains(&arduous_rt::AuditKind::RejectedDenied));
    assert!(kinds.contains(&arduous_rt::AuditKind::RejectedQuotaExceeded));

    println!("governance consumer smoke OK: {value}");
}
RUST

cargo run -q
