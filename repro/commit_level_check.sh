#!/bin/sh
# Re-run the version-switched reproductions at the fixing commit and its parent, so that the
# buggy/fixed difference cannot come from other commits in the release range. Works in a
# temporary copy; does not touch the committed lockfiles.
set -eu
cd "$(dirname "$0")"
root=$(cd .. && pwd)
check() { # case, crate, repo url, fix commit, parent commit
  case=$1 crate=$2 url=$3 fix=$4 parent=$5
  tmp=$(mktemp -d)
  cp -r "$case/src" "$case/tests" "$tmp/"
  [ -d "$case/.cargo" ] && cp -r "$case/.cargo" "$tmp/"
  sed "s#dropwise = { path = \"../..\" }#dropwise = { path = \"$root\" }#" "$case/Cargo.toml" > "$tmp/base.toml"
  for pair in "buggy $parent" "fixed $fix"; do
    set -- $pair
    { cat "$tmp/base.toml"; printf '\n[patch.crates-io]\n%s = { git = "%s", rev = "%s" }\n' "$crate" "$url" "$2"; } > "$tmp/Cargo.toml"
    rm -f "$tmp/Cargo.lock"
    if (cd "$tmp" && EXPECT=$1 cargo test -q >/dev/null 2>&1) && grep -q "rev=$2" "$tmp/Cargo.lock"; then
      echo "PASS $case $1 @ $2"
    else
      echo "FAIL $case $1 @ $2"
    fi
  done
  rm -rf "$tmp"
}
check hyper-3995 hyper https://github.com/hyperium/hyper \
  b7a679bad5e17d106b4b54835153af4e72027f82 156a6f6aaacdc782861a4cab23ab3940a029d6ac
check tokio-7979 tokio https://github.com/tokio-rs/tokio \
  c79121391db8f8d36d4213feeb25381caee110c7 ad8c59add6a1988d8c327fb3358beeeae3bbb5cd
