#!/bin/sh
# Builds the Lettering Extrude plug-in (.wasm) into dist/.
# Needs the wasm32-unknown-unknown target: `rustup target add wasm32-unknown-unknown`.
set -eu
cd "$(dirname "$0")"
cargo build --release --target wasm32-unknown-unknown
mkdir -p dist
out=target/wasm32-unknown-unknown/release/vectorcraft_plugin_letter_extrude.wasm
cp "$out" dist/letter-extrude.wasm
echo "built $out ($(wc -c < "$out") bytes) -> dist/letter-extrude.wasm"
