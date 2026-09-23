#!/bin/sh
# Remaining rounds, issues/PRs only: commit search trips GitHub's secondary rate limit
# (~90 s per query) and in the first 369 labelled candidates every commit was a dup of a PR.
cd "$(dirname "$0")/.."
echo "grpc/grpc-rust" >> data/repos-rest.txt
./survey.py collect --repos data/repos-rest.txt --no-commits
./survey.py collect --repos repos.txt --termset symptom --no-commits
./survey.py export
echo ALL-DONE
