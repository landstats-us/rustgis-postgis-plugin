#!/usr/bin/env bash
# Runs the built plugin (./build.sh first) in rustgis-cli against a real database and exports a
# map of the layer it adds.
#
#   ./e2e.sh [schema.table] [connection]
#   RUSTGIS_CLI=/path/to/rustgis-cli RUSTGIS_DUCKDB_BIN=/folder/with/duckdb-1.5 ./e2e.sh
#
# Both arguments are required, e.g.
#   ./e2e.sh public.counties 'host=localhost dbname=gis user=postgres'
set -euo pipefail
cd "$(dirname "$0")"

table=${1:?usage: ./e2e.sh schema.table connection}
connection=${2:?usage: ./e2e.sh schema.table connection}
cli=${RUSTGIS_CLI:-rustgis-cli}
out=${OUT:-e2e.png}

params=$(python3 -c 'import json, sys; print(json.dumps({"table": sys.argv[1], "connection": sys.argv[2]}))' "$table" "$connection")
plugins=$(mktemp -d)
trap 'rm -rf "$plugins"' EXIT

RUSTGIS_PLUGINS_DIR=$plugins "$cli" run --no-plugins --plugin ./pkg \
  --cmd "plugin:postgis/add-table=$params" \
  --cmd "share.exportMap={\"path\": \"$out\", \"width\": 1400, \"height\": 800}"
