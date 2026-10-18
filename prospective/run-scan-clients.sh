#!/usr/bin/env bash
# Client scan (RESCAN.md, section 6). Results go to stdout only.
#
# sqlx 0.9.0's MySQL driver against MariaDB and lettre 0.11.23's pooled async
# SMTP transport against Mailpit, as released and with the fixes in
# scan-clients/patches/ applied to copies in $DROPWISE_RESCAN_WORK (default
# ~/dropwise-rescan-work). Requires Docker with local mariadb:11.4 and
# axllent/mailpit:v1.27 images and cached crates; the servers listen on
# loopback only and are stopped on exit.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
WORK=${DROPWISE_RESCAN_WORK:-$HOME/dropwise-rescan-work}
REPS=${REPS:-3}

run() { # manifest dir, label, cargo test args...
    local dir=$1 label=$2
    for repetition in $(seq "$REPS"); do
        printf '\n=== %s, repetition %s/%s ===\n' "$label" "$repetition" "$REPS"
        timeout 300s cargo test --offline "${@:3}" --manifest-path "$dir/Cargo.toml" -- --nocapture --test-threads=1
    done
}

patched() { # crate-version, patch file -> path of a patched copy
    local src
    src=$(find "${CARGO_HOME:-$HOME/.cargo}/registry/src" -maxdepth 2 -type d -name "$1" | head -n1)
    [[ -n "$src" ]] || { echo "$1 is not in the cargo registry cache" >&2; exit 1; }
    rm -rf "$WORK/$1-fixed"
    cp -r "$src" "$WORK/$1-fixed"
    patch -d "$WORK/$1-fixed" -p1 --quiet < "$ROOT/prospective/scan-clients/patches/$2"
    echo "$WORK/$1-fixed"
}

mkdir -p "$WORK"
rm -rf "$WORK/scan-clients-fixed" "$WORK/support"
sqlx_mysql=$(patched sqlx-mysql-0.9.0 sqlx-mysql-begin.diff)
lettre=$(patched lettre-0.11.23 lettre-send-in-progress.diff)
cp -r "$ROOT/prospective/support" "$WORK/support"
mkdir -p "$WORK/scan-clients-fixed"
tar -C "$ROOT/prospective/scan-clients" --exclude=./target -cf - . | tar -C "$WORK/scan-clients-fixed" -xf -
sed -i "s#path = \"../..\"#path = \"$ROOT\"#" "$WORK/scan-clients-fixed/Cargo.toml"
printf '\n[patch.crates-io]\nsqlx-mysql = { path = "%s" }\nlettre = { path = "%s" }\n' "$sqlx_mysql" "$lettre" \
    >> "$WORK/scan-clients-fixed/Cargo.toml"

containers=()
cleanup() {
    if ((${#containers[@]})); then
        docker stop --timeout 3 "${containers[@]}" >/dev/null ||
            echo "Could not stop temporary containers: ${containers[*]}" >&2
    fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

containers+=("$(docker run --pull never --rm -d -p 127.0.0.1::3306 \
    -e MARIADB_ROOT_PASSWORD=dropwise -e MARIADB_DATABASE=dropwise \
    -e MARIADB_USER=dropwise -e MARIADB_PASSWORD=dropwise mariadb:11.4)")
containers+=("$(docker run --pull never --rm -d -p 127.0.0.1::1025 -p 127.0.0.1::8025 axllent/mailpit:v1.27)")
container=${containers[0]}
ready=0
for ((attempt=0; attempt<120; attempt++)); do
    if docker exec "$container" healthcheck.sh --connect --innodb_initialized >/dev/null 2>&1; then
        ready=1
        break
    fi
    sleep 0.5
done
[[ "$ready" == 1 ]] || { echo 'Temporary MariaDB did not become ready' >&2; exit 1; }
port=$(docker port "$container" 3306/tcp | awk -F: '/^127\.0\.0\.1:/{print $NF; exit}')
export DW_MYSQL_URL="mysql://dropwise:dropwise@127.0.0.1:$port/dropwise"
export DW_SMTP_PORT DW_SMTP_API_PORT
DW_SMTP_PORT=$(docker port "${containers[1]}" 1025/tcp | awk -F: '/^127\.0\.0\.1:/{print $NF; exit}')
DW_SMTP_API_PORT=$(docker port "${containers[1]}" 8025/tcp | awk -F: '/^127\.0\.0\.1:/{print $NF; exit}')

SCAN_VARIANT=release run "$ROOT/prospective/scan-clients" 'release' --locked --test sqlx_mysql --test lettre
SCAN_VARIANT=fixed run "$WORK/scan-clients-fixed" 'fixed' --test sqlx_mysql --test lettre
