#!/usr/bin/env sh
# Generate from the API document supplied by value in SPEC. SPEC_REF identifies
# the API release that supplied it. CI and make spec-check use the public
# /v1/openapi.json document; .spec-lock records its digest.
# --check refuses any projection change without rewriting files.
set -eu

cd "$(dirname "$0")/.."

# An apostrophe inside ${VAR:?word} opens a quote the parser never closes, and
# the whole script then fails to parse rather than this line failing to run.
[ -n "${SPEC:-}" ] || { echo "generate.sh: SPEC must name the cloud API document; the client lane in hanzoai/ci sets it" >&2; exit 1; }

export HANZO_REGISTRY="$SPEC" HANZO_SPEC_REF="${SPEC_REF:-unpinned}"

if [ "${1:-}" = "--check" ]; then
  cargo run --quiet --features maintainer --bin genspec --locked -- --check
  exit 0
fi

cargo run --quiet --features maintainer --bin genspec --locked
cargo run --quiet --features maintainer --bin genproduct --locked
