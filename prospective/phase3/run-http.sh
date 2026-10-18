#!/usr/bin/env bash
# Phase 3 HTTP-client ladder (prospective/phase3/PLAN.md). Results go to
# stdout only.
#
# Runs the five assigned targets against the shared in-process HTTP/1.1 server:
# hyper-util's legacy Client, reqwest, pingora-core + pingora-pool, isahc and
# awc. Each target binary contains B1, B2, B3, M and (with --features net)
# M-residue. Three repetitions by default.
#
# Variants:
#   release  the crates as published, from the cargo registry;
#   fixed    the same crates with every patch listed in
#            prospective/phase3/http-clients/patches/MANIFEST applied to a copy
#            in $DROPWISE_PHASE3_WORK and wired in with [patch.crates-io].
# The fixed variant is skipped (with a message) when the manifest is empty.
#
# Environment: TARGETS (space-separated target ids), REPS (default 3),
# TIMEOUT (seconds per test process, default 900), DROPWISE_PHASE3_WORK
# (default ~/dropwise-phase3-work), VARIANTS (default "release fixed").
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
DIR=$ROOT/prospective/phase3/http-clients
WORK=${DROPWISE_PHASE3_WORK:-$HOME/dropwise-phase3-work}
REPS=${REPS:-3}
TARGETS=${TARGETS:-"hyper_util reqwest pingora isahc awc"}
VARIANTS=${VARIANTS:-"release fixed"}
MANIFEST=$DIR/patches/MANIFEST

run() { # manifest dir, label
    local dir=$1 label=$2
    local target
    for target in $TARGETS; do
        local i
        for i in $(seq "$REPS"); do
            printf '\n=== %s, %s, repetition %s/%s ===\n' "$target" "$label" "$i" "$REPS"
            date -u +START=%Y-%m-%dT%H:%M:%SZ
            SCAN_VARIANT=$label timeout "${TIMEOUT:-900}s" \
                cargo test --offline --features net --manifest-path "$dir/Cargo.toml" \
                --test "${target}_ladder" -- --nocapture --test-threads=1
            date -u +END=%Y-%m-%dT%H:%M:%SZ
        done
    done
}

patch_entries=()
if [[ -s "$MANIFEST" ]]; then
    while read -r crate version diff extra; do
        [[ -z "${crate:-}" || "$crate" == \#* ]] && continue
        src=$(find "${CARGO_HOME:-$HOME/.cargo}/registry/src" -maxdepth 2 -type d -name "$crate-$version" | head -n1)
        [[ -n "$src" ]] || { echo "$crate-$version is not in the cargo registry cache" >&2; exit 1; }
        dest=$WORK/"$crate-$version-fixed"
        rm -rf "$dest"
        cp -r "$src" "$dest"
        patch -d "$dest" -p1 --quiet < "$DIR/patches/$diff"
        patch_entries+=("$crate = { path = \"$dest\" }")
    done < "$MANIFEST"
fi

for variant in $VARIANTS; do
    if [[ "$variant" == release ]]; then
        run "$DIR" release
    elif [[ "$variant" == fixed ]]; then
        if ((${#patch_entries[@]} == 0)); then
            printf '\n=== fixed variant skipped: %s is empty, no patch to compare ===\n' "$MANIFEST"
            continue
        fi
        mkdir -p "$WORK"
        rm -rf "$WORK/http-clients-fixed"
        mkdir -p "$WORK/http-clients-fixed"
        tar -C "$DIR" --exclude=./target -cf - . | tar -C "$WORK/http-clients-fixed" -xf -
        sed -i "s#path = \"../../..\"#path = \"$ROOT\"#" "$WORK/http-clients-fixed/Cargo.toml"
        # The copied crate lives outside the repository, so the support module
        # has to be addressed absolutely in the copied test files.
        for test_file in "$WORK"/http-clients-fixed/tests/*.rs; do
            sed -i "s#\"../../../support/ladder.rs\"#\"$ROOT/prospective/support/ladder.rs\"#" "$test_file"
        done
        {
            printf '\n[patch.crates-io]\n'
            printf '%s\n' "${patch_entries[@]}"
        } >> "$WORK/http-clients-fixed/Cargo.toml"
        run "$WORK/http-clients-fixed" fixed
    else
        echo "unknown variant: $variant" >&2
        exit 1
    fi
done
