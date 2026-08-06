# hanzo — the CLI. Five commands matter here, and they are TWO questions asked
# about ONE chain:
#
#   hanzoai/cloud@<tag> openapi.yaml   the document, pinned in .spec-lock
#        │ genspec
#   spec/cloud.json                    the projection
#        │ genproduct
#   generated.rs                       the command tree
#        ·  spec/live.json             what the server said about all of it
#
# `spec` / `spec-check`  IS THE PROJECTION STILL ITS INPUT?  Re-derive, refuse a
#                        delta. Needs the document (network, a token).
# `live` / `live-check`  IS THE INPUT STILL TRUE OF PRODUCTION?  Ask the running
#                        server, with a control for every probe, and write down
#                        the raw answers. Needs the network and nothing else.
# `verify`               RULE ON WHAT IS COMMITTED. Both directions, every link of
#                        the chain, no network at all — which is why `cargo test`
#                        runs it too, and why it cannot be the gate that gets
#                        switched off for being slow or flaky.
#
# That split is the whole design. A gate that needs the network in CI gets
# disabled, and a disabled gate is worse than none; a gate that never asks the
# server can only compare derived artifacts with each other, which they satisfy
# whether or not either is still true. So ASKING and RULING are two commands over
# one checked-in body of evidence.
#
# `spec` and `spec-check` both go through scripts/generate.sh, which is also what
# hanzoai/ci's `client:` lane calls — so a maintainer, the push gate and the
# release all run the same derivation. A gate that runs a different generation
# than the writer is a gate that tests something nobody ships.

SHELL := /bin/bash
SPEC_REPO ?= hanzoai/cloud
SPEC_PATH ?= openapi.yaml
LOCK      := .spec-lock

.PHONY: help spec spec-check live live-check verify test build lint

help: ## This.
	@grep -hE '^[a-z-]+:.*##' $(MAKEFILE_LIST) | sed 's/:.*## /\t/' | expand -t20

# The document, BY VALUE at the ref this tree names. Fetching it — rather than
# reading api.hanzo.ai — is what makes the check reproducible: the same ref
# always yields the same bytes, and the digest below proves it. A live host
# answers "what is deployed right now", which is a different question and not
# one a committed artifact can be measured against.
define fetch_spec
ref=$$(sed -n 's/^ref=//p' $(LOCK)); \
want=$$(sed -n 's/^sha256=//p' $(LOCK)); \
[ -n "$$ref" ] || { echo "no $(LOCK) — this tree does not name a document"; exit 1; }; \
[ -n "$${SPEC_TOKEN:-}" ] || { echo "SPEC_TOKEN (contents:read on $(SPEC_REPO)) is unset"; exit 1; }; \
tmp=$$(mktemp); \
curl -fsSL -H "Authorization: Bearer $$SPEC_TOKEN" -H 'Accept: application/vnd.github.raw' \
  "https://api.github.com/repos/$(SPEC_REPO)/contents/$(SPEC_PATH)?ref=$$ref" -o "$$tmp"; \
got=$$(sha256sum "$$tmp" | cut -d' ' -f1); \
[ "$$got" = "$$want" ] || { echo "$(SPEC_REPO)@$$ref:$(SPEC_PATH) hashes to $$got, but $(LOCK) says $$want — the ref moved under this capture"; exit 1; }; \
export SPEC="$$tmp" SPEC_REF="$$ref"
endef

# Moving the projection forward RE-ASKS THE SERVER in the same breath. Evidence
# is about a specific spec/cloud.json — it carries that file's digest — so a
# regeneration without a re-capture leaves the tree with evidence about a
# document that no longer exists, and `verify` says so rather than passing on it.
# One command, because they are one act.
spec: ## Regenerate spec/cloud.json + the product tree, then re-capture the evidence.
	@$(fetch_spec); ./scripts/generate.sh
	@$(MAKE) --no-print-directory live

spec-check: ## Refuse a capture that is no longer the projection of its own document.
	@$(fetch_spec); ./scripts/generate.sh --check

# ASK. The only thing here that touches the running server, and the only thing
# that writes spec/live.json. Every probe carries a CONTROL — a nonsense sibling
# under the same prefix — because a relay door or an auth wall answers
# identically for a real path and an invented one, and an answer that cannot tell
# those apart is not evidence about either.
live: ## Re-ask the live server and re-capture spec/live.json. The one network step.
	@cargo build --quiet --features maintainer --bin driftgate --locked
	@./target/debug/driftgate --refresh

live-check: ## Refuse evidence the live server no longer agrees with. THE NIGHTLY.
	@cargo build --quiet --features maintainer --bin driftgate --locked
	@./target/debug/driftgate --refresh --check

# RULE. Both directions — a command whose route the server does not serve, and a
# served product no command reaches — plus every link of the chain, against what
# is committed and nothing else. It needs the built `hanzo` because reachability
# is a fact about the BINARY: a generated product that collides with a local
# command, or a relocation that stopped relocating, is invisible to any
# re-derivation of the same data and obvious to one exec.
verify: ## Refuse a tree the committed evidence contradicts. Hermetic. The drift gate.
	@cargo build --quiet --features maintainer --bin hanzo --bin driftgate --locked
	@./target/debug/driftgate

# THE GATES IN tests/ EXEC BINARIES CARGO DOES NOT BUILD FOR THEM. `driftgate`
# and `genproduct` are maintainer tools behind `required-features`, so a plain
# `cargo test` skips building them and the two tests that exec them die on
# `NotFound` instead of ruling. A gate that cannot run reads exactly like a gate
# that passed — the same defect `verify` had, wearing different clothes.
#
# The gate is built WITH the feature; its SUBJECT is not. Reachability is a fact
# about the binary a person installs, and `--features maintainer` changes that
# binary (measured: the plain and maintainer `hanzo` differ in sha256, with a
# same-flags control in a third target dir proving it is the feature and not the
# path). So the build below names only the two tools, and `cargo test` rebuilds
# `hanzo` the way a user gets it.
#
# --workspace because `crates/event` is a member and a bare `cargo test` runs the
# root package alone, so hanzo-event's tests never ran once. --no-fail-fast
# because stopping at the first failing package is how they stayed unrun: the
# drift gate is upstream of them and fails on real drift by design.
test: ## Everything, including the derivation gates in tests/ — `verify` among them.
	@cargo build --quiet --features maintainer --bin driftgate --bin genproduct --locked
	@cargo test --workspace --locked --no-fail-fast

build: ## The shipped binary.
	@cargo build --release --bin hanzo --locked

lint:
	@cargo clippy --locked --all-targets -- -D warnings
