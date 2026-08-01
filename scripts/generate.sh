#!/usr/bin/env sh
# The CLI's projection of the Hanzo Cloud API document — the same call site every
# other client repo has, so hanzoai/ci's `client:` lane drives all eight the same
# way.
#
# TWO INPUTS, both by value:
#   $SPEC       the document itself, already fetched at a pinned ref and already
#               digest-checked by the lane. Never a live host: at generation time
#               the deploy this describes has already happened, and asking
#               api.hanzo.ai instead would name whatever it is serving now rather
#               than the release that handed us this file.
#   $SPEC_REF   which ref that was. genspec stamps it, with the document's own
#               sha256, into the spec's provenance — so `spec/cloud.json` can
#               always answer "which release am I?".
#
# One optional input: CHECK=1 re-asks instead of rewriting (exit 1 on any delta).
# That is the same binary and the same derivation, which is what keeps the gate
# from testing a different generation than the one that writes the file.
#
# The authored master (hanzoai/openapi `hanzo.yaml`) is still the only source of
# request-body and query SHAPE for cloud's untyped handlers, so it is cloned when
# absent. It is an input to the CAPTURE, never to what the capture claims exists
# — the document decides that. When cloud's handlers finish becoming zip typed
# ops this clone goes away with the master.
#
# CREDENTIAL: SPEC_TOKEN — contents:read on hanzoai/openapi (private).
set -eu

cd "$(dirname "$0")/.."

: "${SPEC:?SPEC must name the cloud API document (hanzoai/ci's client: lane sets it)}"

MASTER="${HANZO_OPENAPI_DIR:-$PWD/.openapi}"
if [ ! -d "$MASTER/.git" ]; then
  [ -n "${SPEC_TOKEN:-}" ] || { echo "generate.sh: no hanzoai/openapi checkout at $MASTER and no SPEC_TOKEN to clone one" >&2; exit 1; }
  git clone --quiet --filter=blob:none "https://x-access-token:${SPEC_TOKEN}@github.com/hanzoai/openapi" "$MASTER"
fi

# THE MASTER IS PINNED TOO, and for the same reason the document is: a gate whose
# inputs move is a gate two runs of one commit can disagree about. Checking
# against whatever hanzoai/openapi's main happens to be turns "the capture is
# stale" red for a change nobody in this lineage made, and turns it green again
# by itself later. So `.spec-lock` records both halves — the document by ref and
# digest, the master by sha — and only a regeneration advances either.
#
# It records the master because the master still exists. It is the last source of
# request-body and query SHAPE for cloud's untyped handlers; when those finish
# becoming zip typed ops the document carries its own schemas, this clone goes
# away, and this line goes with it.
LOCK="$PWD/.spec-lock"
PINNED=$(sed -n 's/^master=//p' "$LOCK" 2>/dev/null || true)

if [ "${CHECK:-0}" = "1" ]; then
  [ -n "$PINNED" ] || { echo "generate.sh: .spec-lock records no master= — run 'make spec' to pin one" >&2; exit 1; }
  git -C "$MASTER" cat-file -e "$PINNED^{commit}" 2>/dev/null || git -C "$MASTER" fetch --quiet origin "$PINNED"
  git -C "$MASTER" checkout --quiet --detach "$PINNED"
  export HANZO_REGISTRY="$SPEC" HANZO_SPEC_REF="${SPEC_REF:-unpinned}"
  cargo run --quiet --features genspec --bin genspec --locked -- --openapi "$MASTER" --check
  exit 0
fi

git -C "$MASTER" fetch --quiet origin main
git -C "$MASTER" checkout --quiet --detach FETCH_HEAD
export HANZO_REGISTRY="$SPEC" HANZO_SPEC_REF="${SPEC_REF:-unpinned}"
cargo run --quiet --features genspec --bin genspec --locked -- --openapi "$MASTER"
cargo run --quiet --bin genproduct --locked

# The lock is written in two halves by two owners, each recording what it alone
# knows: the client: lane writes the document (repo/path/ref/sha256), this writes
# the master it actually generated against. Neither guesses at the other's.
grep -v '^master=' "$LOCK" > "$LOCK.new" 2>/dev/null || : > "$LOCK.new"
printf 'master=%s\n' "$(git -C "$MASTER" rev-parse HEAD)" >> "$LOCK.new"
mv "$LOCK.new" "$LOCK"
