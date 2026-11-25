#!/bin/sh
# Container / clean-cache E2E integration tests for hanzoai/cli#65:
#   1. `hanzo models list` output formats (`table`, `json`, `yaml`)
#   2. `hanzo chat completions` typed flags and `--data` refusal text
#   3. `hanzo-mcp` starts and answers `initialize` within client timeout on a cold cache
#   4. Isolated auth flow (`auth login --token -`, `auth show`, `auth token`, `auth logout`)
#
# Runs inside Dockerfile.e2e (clean Debian image with empty cache) or directly
# on any POSIX host with `HANZO_E2E_BIN` / `HANZO_E2E_MCP_BIN` or public installer.
set -eu

E2E_ROOT=$(mktemp -d)
trap 'rm -rf "$E2E_ROOT"' EXIT HUP INT TERM

export HOME="$E2E_ROOT/home"
export XDG_CACHE_HOME="$E2E_ROOT/cache"
export XDG_CONFIG_HOME="$E2E_ROOT/config"
export XDG_DATA_HOME="$E2E_ROOT/data"
export NPM_CONFIG_CACHE="$E2E_ROOT/npm-cache"
export UV_CACHE_DIR="$E2E_ROOT/uv-cache"
export NO_COLOR=1
export HANZO_NO_ANIMATION=1
mkdir -p "$HOME" "$E2E_ROOT/bin"

HANZO_BIN="${HANZO_E2E_BIN:-}"
MCP_BIN="${HANZO_E2E_MCP_BIN:-}"

if [ -z "$HANZO_BIN" ] || [ -z "$MCP_BIN" ]; then
  curl -fsSL https://hanzo.sh -o "$E2E_ROOT/install.sh"
  sh "$E2E_ROOT/install.sh" --dir "$E2E_ROOT/bin" hanzo mcp
  HANZO_BIN="${HANZO_BIN:-$E2E_ROOT/bin/hanzo}"
  MCP_BIN="${MCP_BIN:-$E2E_ROOT/bin/hanzo-mcp}"
fi

CFG="$E2E_ROOT/config.toml"

echo "==> 1. hanzo models list (anonymous, table + json)"
"$HANZO_BIN" models list -o json --config "$CFG" > "$E2E_ROOT/models.json"
jq -e '(if type == "array" then length else (.data | length) end) > 0' "$E2E_ROOT/models.json" >/dev/null
"$HANZO_BIN" models list -o table --config "$CFG" | grep -qE 'zen|enso'
echo "PASS: models list"

echo "==> 2. hanzo chat completions --data refusal"
if "$HANZO_BIN" chat completions --data '{"model":"zen6-flash"}' --config "$CFG" >"$E2E_ROOT/data.out" 2>"$E2E_ROOT/data.err"; then
  echo "FAIL: expected --data on typed command to fail" >&2
  exit 1
fi
grep -q "takes its body as flags, not --data" "$E2E_ROOT/data.err"
grep -q -- "--model" "$E2E_ROOT/data.err"
grep -q -- "--messages" "$E2E_ROOT/data.err"
echo "PASS: chat completions --data error text"

echo "==> 3. hanzo-mcp cold-cache stdio initialize within 10s"
INIT_REQ='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"container-e2e","version":"1.0.0"}}}'
if command -v timeout >/dev/null 2>&1; then
  MCP_REPLY=$(printf '%s\n' "$INIT_REQ" | timeout 10 "$MCP_BIN" | head -n 1)
else
  MCP_REPLY=$(printf '%s\n' "$INIT_REQ" | "$MCP_BIN" | head -n 1)
fi
printf '%s\n' "$MCP_REPLY" | jq -e '.jsonrpc == "2.0" and .id == 1 and (.result.protocolVersion | length > 0) and (.result.serverInfo.name | length > 0)' >/dev/null
echo "PASS: hanzo-mcp cold-cache initialize"

echo "==> 4. auth flow & live typed chat completions (when HANZO_E2E_TOKEN is set)"
if [ -n "${HANZO_E2E_TOKEN:-}" ]; then
  printf '%s\n' "$HANZO_E2E_TOKEN" | "$HANZO_BIN" auth login --token - --config "$CFG"
  "$HANZO_BIN" auth show --config "$CFG"
  "$HANZO_BIN" chat completions \
    --model zen6-flash \
    --messages '{"role":"user","content":"Reply OK"}' \
    --max-tokens 16 \
    -o json \
    --config "$CFG" > "$E2E_ROOT/chat.json"
  jq -e '.choices[0].message.content | length > 0' "$E2E_ROOT/chat.json" >/dev/null
  "$HANZO_BIN" auth logout --config "$CFG"
  echo "PASS: live auth + chat completions typed flags"
else
  # Offline synthetic JWT check when no live token is provided
  FAKE_JWT="eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.eyJvd25lciI6ImFjbWUiLCJuYW1lIjoiYWxpY2UiLCJzdWIiOiJ1LTEiLCJhenAiOiJhY21lLWNsaSJ9.c2ln"
  printf '%s\n' "$FAKE_JWT" | "$HANZO_BIN" auth login --token - --config "$CFG"
  "$HANZO_BIN" auth list --config "$CFG" | grep -q "acme/alice"
  test "$("$HANZO_BIN" auth token --config "$CFG")" = "$FAKE_JWT"
  "$HANZO_BIN" auth logout --config "$CFG"
  echo "PASS: isolated auth flow (no HANZO_E2E_TOKEN)"
fi

echo "ALL CONTAINER E2E CHECKS PASSED"
