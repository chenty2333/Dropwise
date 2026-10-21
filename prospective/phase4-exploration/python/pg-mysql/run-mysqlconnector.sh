#!/usr/bin/env bash
# HISTORICAL PLACEHOLDER: this file used to start mysql:9.6 and stop without
# exercising the client.  The unit is now implemented in
# `run-mysqlconnector-aio.sh` (mysql.connector.aio, direct mode) and reported in
# RESULTS-FRAGMENT-round3b.md.  Kept so the earlier gap stays visible.
set -u
cd "$(dirname "$0")"
exec ./run-mysqlconnector-aio.sh "$@"
