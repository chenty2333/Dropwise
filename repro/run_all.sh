#!/bin/sh
# Re-run every paired reproduction. Each case prints PASS when the buggy variant shows a
# violation and the fixed variant is clean (as asserted by its tests).
set -u
cd "$(dirname "$0")"
status=0
run() { # dir, then optional "pkg buggy_version fixed_version" for version-switched cases
  dir=$1; shift
  if [ $# -eq 3 ]; then
    (cd "$dir" && cargo update -q -p "$1" --precise "$2" && EXPECT=buggy cargo test -q >/dev/null 2>&1 \
      && cargo update -q -p "$1" --precise "$3" && EXPECT=fixed cargo test -q >/dev/null 2>&1; r=$?; \
      cargo update -q -p "$1" --precise "$2"; exit $r)
  elif [ -x "$dir/run.sh" ]; then
    (cd "$dir" && ./run.sh -q -- --test-threads 1 >/dev/null 2>&1)
  else
    (cd "$dir" && cargo test -q -- --test-threads 1 >/dev/null 2>&1)
  fi
  if [ $? -eq 0 ]; then echo "PASS $dir"; else echo "FAIL $dir"; status=1; fi
}
run tokio-6877
run hyper-3995 hyper 1.9.0 1.10.0
run tokio-7979 tokio 1.51.0 1.52.0
run pingora-931
run modelled
run tokio-3825
run redis-851
exit $status
