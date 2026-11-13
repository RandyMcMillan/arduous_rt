#!/usr/bin/env bash
# Verify arduous_rt compiles against the tokio floor declared in Cargo.toml.
#
# Catches manifest/floor drift: if the crate ever starts using a tokio API
# newer than the declared minimum (the original `Handle::id` case, floor
# 1.49), this check fails instead of shipping an overpromising manifest.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

floor_line="$(grep -E '^tokio = ' "$ROOT/Cargo.toml" | head -n 1)"
floor_minor="$(echo "$floor_line" | sed -E 's/.*"1\.([0-9]+).*/\1/')"
if [ -z "$floor_minor" ] || [ "$floor_minor" = "$floor_line" ]; then
    echo "no explicit tokio 1.x floor declared in Cargo.toml; refusing to guess"
    exit 1
fi
echo "declared tokio floor: 1.$floor_minor"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
cd "$TMP"

cargo init -q --name tokio_floor_check
cargo add arduous_rt --path "$ROOT" -q
cargo add "tokio@=1.$floor_minor.0" --features full -q
cargo check -q

echo "tokio 1.$floor_minor.0 compiles arduous_rt: floor is accurate"
