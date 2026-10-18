#!/usr/bin/env python3
"""Candidate discovery and read-only source download for the cancellation scan.

This script only talks to the crates.io HTTP API and static.crates.io.  It
never runs, builds, or imports the downloaded crates.  Archives are extracted
with path validation, no links/devices, and a size bound.

Outputs (all outside the repository except universe.csv):
  ~/dropwise-scan-src/api-cache/       crates.io responses (resumable)
  ~/dropwise-scan-src/src/<crate>-<ver>/ extracted source
  prospective/candidate-scan/universe.csv

Ranking is by crates.io `recent_downloads` (the 90-day figure).
"""
import argparse
import concurrent.futures
import csv
import io
import json
import pathlib
import re
import tarfile
import time
import urllib.parse
import urllib.request

HERE = pathlib.Path(__file__).resolve()
OUT = HERE.parents[1]
UA = "Dropwise static cancellation research; read-only source survey"

# Already analysed in this repository (FOLLOWUP.md / RESCAN.md).  Kept in the
# universe and marked `covered`, but not deep-reviewed again.
COVERED = set("""
bb8 bb8-memcached deadpool-memcached sqlx-postgres sqlx-mysql lettre
tokio-postgres deadpool-postgres redis moka async-nats tokio-tungstenite
""".split())

# Async clients that own a pool or reuse a connection.  This is a deliberately
# broad seed list: the scan does not depend on all of them existing.
CLIENTS = """
mysql_async mysql diesel diesel-async async-imap async-smtp imap-client tokio-imap
sqlx sqlx-core sqlx-mysql sqlx-postgres sqlx-sqlite
tokio-postgres postgres postgres-native-tls rust-postgres
redis fred redis-async redis_pool redis-cluster-async async-redis rsmq_async
darkredis redis-cluster bb8-redis deadpool-redis
memcache-async async-memcached async-memcache bb8-memcached deadpool-memcached
lettre async-smtp imap-client async-imap tokio-imap async-pop async-pop3
lapin amqprs amiquip async-amqp async-nats nats rdkafka pulsar rumqttc
mqtt-async-client paho-mqtt tiberius mongodb scylla cdrs-tokio clickhouse
clickhouse-rs influxdb influxdb2 influx_db_client surrealdb neo4rs bolt-client
dgraph-tonic tikv-client etcd-client zookeeper-client zookeeper-async
rs_consul consulrs ldap3 bb8-ldap
tokio-tungstenite fastwebsockets soketto reqwest hyper hyper-util h2 tonic
tarpc jsonrpsee jsonrpsee-core jsonrpsee-http-client jsonrpsee-ws-client
jsonrpsee-client-transport opendal object_store aws-sdk-s3 aws-sdk-dynamodb
aws-sdk-sqs aws-sdk-sns aws-smithy-runtime aws-smithy-http azure_core
google-cloud-storage google-cloud-pubsub quinn h3 s2n-quic pingora
pingora-pool pingora-core isahc surf awc ntex libsql libsql-client turso
tokio-modbus edgedb-tokio gel-tokio sea-orm rbatis rbdc rbdc-mysql rbdc-pg
rbdc-sqlite rbdc-mssql async-bb8-diesel bb8-diesel deadpool-diesel
mobc-postgres mobc-tiberius bb8-tiberius bb8-lapin deadpool-lapin bb8-mongodb
bb8-oracle bb8-surrealdb deadpool-sqlite deadpool-sync deadpool-libsql
deadpool-ldap deadpool-tiberius deadpool-r2d2 deadpool-bolt deadpool-diesel
""".split()

# Names that must be in the scan even if the download cutoff would exclude
# them (the task calls these out explicitly).
FORCE = set("mysql_async diesel-async async-imap".split())

# Reverse-dependency roots.  `deadpool-*` family crates are discovered through
# the crates.io search below and get their own reverse-dependency queries.
POOL_ROOTS = ["bb8", "deadpool", "mobc"]


class Registry:
    def __init__(self, root):
        self.cache = root / "api-cache"
        self.cache.mkdir(parents=True, exist_ok=True)
        self.last = 0.0

    def get(self, path):
        key = urllib.parse.quote(path, safe="")
        if len(key) > 150:
            import hashlib
            key = "sha256-" + hashlib.sha256(path.encode()).hexdigest()
        p = self.cache / (key + ".json")
        if p.exists():
            return json.loads(p.read_text())
        for attempt in range(6):
            wait = max(0.0, 1.02 - (time.monotonic() - self.last))
            time.sleep(wait)
            self.last = time.monotonic()
            try:
                req = urllib.request.Request(
                    "https://crates.io/api/v1/" + path, headers={"User-Agent": UA}
                )
                with urllib.request.urlopen(req, timeout=90) as r:
                    d = json.load(r)
                tmp = p.with_suffix(".tmp")
                tmp.write_text(json.dumps(d))
                tmp.replace(p)
                return d
            except Exception:
                if attempt == 5:
                    raise
                time.sleep(2 ** attempt)
        raise AssertionError("unreachable")


def paged(reg, path, key, total_key="total"):
    page = 1
    while True:
        d = reg.get(f"{path}?page={page}&per_page=100")
        for x in d[key]:
            yield x
        total = d["meta"].get(total_key, 0)
        if page * 100 >= total:
            break
        page += 1


def discover(reg, log):
    """Return {crate: set(origin)} for the candidate universe."""
    origins = {}

    def add(name, why):
        origins.setdefault(name, set()).add(why)

    # deadpool-* crates: search by name.  crates.io search also matches
    # descriptions, so filter to the actual family prefix.
    family = set(POOL_ROOTS)
    for x in paged(reg, "crates?q=deadpool&sort=recent-downloads", "crates"):
        n = x["name"]
        if n == "deadpool" or n.startswith("deadpool-"):
            family.add(n)
    log(f"deadpool family: {len(family)} crates")

    # Reverse dependencies of every pool / pool manager.  The endpoint returns
    # one row per dependent *version*; deduplicate by crate name.
    for pool in sorted(family | set(POOL_ROOTS)):
        seen = set()
        for x in paged(reg, f"crates/{pool}/reverse_dependencies", "versions"):
            n = x["crate"]
            if n not in seen:
                seen.add(n)
                add(n, "rev:" + pool)
        log(f"reverse deps {pool}: {len(seen)}")
        add(pool, "pool-seed")

    for n in CLIENTS:
        add(n, "client-seed")
    return origins, family


def fetch_meta(reg, names, log):
    names = sorted(names)
    meta = {}
    for i in range(0, len(names), 50):
        chunk = names[i : i + 50]
        q = urllib.parse.urlencode([("ids[]", n) for n in chunk])
        d = reg.get(f"crates?per_page=100&{q}")
        for x in d["crates"]:
            meta[x["name"]] = x
        log(f"metadata {i + len(chunk)}/{len(names)}")
    return meta


def safe_extract(data, dest, prefix):
    dest.mkdir(parents=True, exist_ok=True)
    total = 0
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as tf:
        for m in tf:
            p = pathlib.PurePosixPath(m.name)
            if p.is_absolute() or ".." in p.parts or not p.parts or p.parts[0] != prefix:
                raise ValueError(f"unsafe archive path {m.name!r}")
            if not m.isfile():
                continue
            total += m.size
            if m.size > 60_000_000 or total > 400_000_000:
                raise ValueError("archive too large")
            target = dest.joinpath(*p.parts[1:])
            target.parent.mkdir(parents=True, exist_ok=True)
            if target.is_symlink():
                raise ValueError("symlink in output")
            with tf.extractfile(m) as src:
                target.write_bytes(src.read())


def download(row, root):
    name, version = row["crate"], row["version"]
    dest = root / "src" / f"{name}-{version}"
    if (dest / ".static-scan-complete").exists():
        return "cached"
    url = f"https://static.crates.io/crates/{name}/{name}-{version}.crate"
    req = urllib.request.Request(url, headers={"User-Agent": UA})
    for attempt in range(5):
        try:
            with urllib.request.urlopen(req, timeout=120) as r:
                data = r.read(60_000_001)
            if len(data) > 60_000_000:
                raise ValueError("download too large")
            safe_extract(data, dest, f"{name}-{version}")
            (dest / ".static-scan-complete").touch()
            return "ok"
        except Exception as e:
            if attempt == 4:
                return "FAIL " + repr(e)
            time.sleep(2 ** attempt)
    return "FAIL"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--root", type=pathlib.Path,
                    default=pathlib.Path.home() / "dropwise-scan-src")
    ap.add_argument("--limit", type=int, default=150)
    ap.add_argument("--no-download", action="store_true")
    ap.add_argument("--only", default="", help="comma separated crate names")
    ap.add_argument("--no-universe", action="store_true",
                    help="do not rewrite universe.csv (used by calibrate.sh)")
    a = ap.parse_args()
    root = a.root.expanduser().resolve()
    root.mkdir(parents=True, exist_ok=True)
    reg = Registry(root)

    def log(*args):
        print(*args, flush=True)

    if a.only:
        # calibration mode: no reverse-dependency discovery, no universe rewrite
        origins = {n.strip(): {"only"} for n in a.only.split(",") if n.strip()}
        meta = fetch_meta(reg, origins, log)
        missing = sorted(set(origins) - set(meta))
        if missing:
            log("missing metadata:", " ".join(missing))
        ranked = sorted(
            meta.values(), key=lambda x: (-(x.get("recent_downloads") or 0), x["name"])
        )
        selected = set(origins)
    else:
        origins, family = discover(reg, log)
        meta = fetch_meta(reg, origins, log)
        missing = sorted(set(origins) - set(meta))
        if missing:
            log("missing metadata:", " ".join(missing))
        ranked = sorted(
            meta.values(), key=lambda x: (-(x.get("recent_downloads") or 0), x["name"])
        )
        top = {x["name"] for x in ranked[: a.limit]}
        selected = top | COVERED | FORCE

    fields = ["rank", "crate", "version", "downloads_90d", "covered", "selected",
              "origin", "repository", "description"]
    rows = []
    for rank, x in enumerate(ranked, 1):
        n = x["name"]
        rows.append({
            "rank": rank,
            "crate": n,
            "version": x.get("max_stable_version") or x.get("max_version") or "",
            "downloads_90d": x.get("recent_downloads") or 0,
            "covered": "yes" if n in COVERED else "",
            "selected": "yes" if n in selected else "",
            "origin": ";".join(sorted(origins[n])),
            "repository": x.get("repository") or "",
            "description": (x.get("description") or "").replace("\n", " ")[:160],
        })
    out = OUT / "universe.csv"
    if not a.no_universe:
        with out.open("w", newline="") as f:
            w = csv.DictWriter(f, fieldnames=fields)
            w.writeheader()
            w.writerows(rows)
        log(f"discovered {len(rows)} crates; selected {len(selected)}; wrote {out}")
    else:
        log(f"discovered {len(rows)} crates; selected {len(selected)}")

    if a.no_download:
        return
    chosen = [r for r in rows if r["selected"] == "yes"]
    # Smallest first is not meaningful; run 6 in parallel but keep rate sane.
    results = []
    with concurrent.futures.ThreadPoolExecutor(max_workers=6) as pool:
        futs = {pool.submit(download, r, root): r for r in chosen}
        for fut in concurrent.futures.as_completed(futs):
            r = futs[fut]
            status = fut.result()
            results.append((r["crate"], r["version"], status))
            if status != "cached":
                log("source", r["crate"], r["version"], status)
    bad = [x for x in results if x[2].startswith("FAIL")]
    log(f"downloaded {len(results) - len(bad)}/{len(results)}")
    for b in bad:
        log("FAILED", *b)


if __name__ == "__main__":
    main()
