#!/usr/bin/env sh
# The CLI's projection of the Hanzo Cloud API document — the same call site every
# other client repo has, so hanzoai/ci's `client:` lane drives all eight the same
# way.
#
# ONE INPUT, by value:
#   $SPEC       the document itself, already fetched at a pinned ref and already
#               digest-checked by the lane. Never a live host: at generation time
#               the deploy this describes has already happened, and asking
#               api.hanzo.ai instead would name whatever it is serving now rather
#               than the release that handed us this file.
#   $SPEC_REF   which ref that was. genspec stamps it, with the document's own
#               sha256, into the spec's provenance — so `spec/cloud.json` can
#               always answer "which release am I?".
#
# One optional argument: `--check` re-asks instead of rewriting (exit 1 on any
# delta). Same binary, same derivation — which is what keeps the gate from
# testing a different generation than the one that writes the file. It is an
# argument and not an env var because that is the spelling every other client
# repo's call site already uses, and one spelling of one idea is the point.
#
# THE SECOND INPUT IS GONE, and with it the clone and the credential. This script
# used to fetch hanzoai/openapi and pin it in `.spec-lock` beside the document,
# because the hand-authored master `hanzo.yaml` was the last source of
# request-body and query SHAPE for cloud's untyped handlers. It had stopped being
# only that: 71 operations entered `spec/cloud.json` on the master's word alone —
# two addressing nothing at all, and sixty-six "confirmed" only by a
# `{wildcardN}` relay door, which answers identically for a real path and an
# invented one. A second authority over what EXISTS is how a CLI grows commands
# no server serves. The document decides existence, prose and shape alike now, so
# there is nothing left to clone and no SPEC_TOKEN to hold.
set -eu

cd "$(dirname "$0")/.."

# An apostrophe inside ${VAR:?word} opens a quote the parser never closes, and
# the whole script then fails to parse rather than this line failing to run.
[ -n "${SPEC:-}" ] || { echo "generate.sh: SPEC must name the cloud API document; the client lane in hanzoai/ci sets it" >&2; exit 1; }

export HANZO_REGISTRY="$SPEC" HANZO_SPEC_REF="${SPEC_REF:-unpinned}"

if [ "${1:-}" = "--check" ]; then
  cargo run --quiet --features genspec --bin genspec --locked -- --check
  exit 0
fi

cargo run --quiet --features genspec --bin genspec --locked
cargo run --quiet --bin genproduct --locked

# `.spec-lock` has ONE writer now — hanzoai/ci's client: lane, which knows the
# document's repo, path, ref and digest and is the only thing that can know them.
# This script used to append a second half (`master=`, the hanzoai/openapi commit
# it generated against). There is no second half.
