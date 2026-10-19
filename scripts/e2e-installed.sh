#!/bin/sh
set -eu
: "${HANZO_E2E_TOKEN:?Set HANZO_E2E_TOKEN using hanzo auth token}"
e2e_dir=$(mktemp -d)
trap 'rm -rf "$e2e_dir"' EXIT HUP INT TERM
curl -fsSL https://hanzo.sh -o "$e2e_dir/install.sh"
sh "$e2e_dir/install.sh" --dir "$e2e_dir/bin" hanzo dev gen mcp
for tool in hanzo hanzo-node dev gen hanzo-mcp; do
    "$e2e_dir/bin/$tool" --version
done
[ "$("$e2e_dir/bin/hanzo" --version)" = "$("$e2e_dir/bin/hanzo-node" --version)" ]
PATH="$e2e_dir/bin:$PATH" HANZO_E2E_BIN="$e2e_dir/bin/hanzo" 
export PATH HANZO_E2E_BIN
cargo test --locked --test production -- --ignored --nocapture --test-threads=1
