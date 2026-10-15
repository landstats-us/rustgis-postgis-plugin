#!/usr/bin/env bash
# Builds the plugin and packages it for RustGIS:
#   pkg/                       plugin.json + plugin.wasm (Plugin Manager › Settings › Add Plugin Folder…)
#   dist/postgis-<ver>.rgplugin the installable package
set -euo pipefail
cd "$(dirname "$0")"

cargo build --release --target wasm32-unknown-unknown

version=$(python3 -c 'import json; print(json.load(open("plugin.json"))["version"])')
rm -rf pkg && mkdir -p pkg dist
cp plugin.json pkg/
cp target/wasm32-unknown-unknown/release/postgis.wasm pkg/plugin.wasm
rm -f "dist/postgis-$version.rgplugin"
(cd pkg && zip -qX "../dist/postgis-$version.rgplugin" plugin.json plugin.wasm)
ls -l pkg/plugin.wasm "dist/postgis-$version.rgplugin"
