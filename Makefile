# Hanzo CLI checks: generate from the same public API document used by CI.
# The lock records the API release and the digest actually used. spec-check
# refuses changed input; make spec refreshes the projection from that input.
SHELL := /bin/bash
SPEC_URL ?= https://api.hanzo.ai/v1/openapi.json
LOCK := .spec-lock

.PHONY: help spec spec-check live live-check verify test build lint

help: ## This.
	@grep -hE '^[a-z-]+:.*##' $(MAKEFILE_LIST) | sed 's/:.*## /\t/' | expand -t20

define fetch_spec
ref=$$(sed -n 's/^ref=//p' $(LOCK)); \
want=$$(sed -n 's/^sha256=//p' $(LOCK)); \
[ -n "$$ref" ] && [ -n "$$want" ] || { echo "$(LOCK) must name the API release and digest"; exit 1; }; \
tmp=$$(mktemp); trap 'rm -f "$$tmp"' EXIT; \
curl -fsSL "$(SPEC_URL)" -o "$$tmp"; \
got=$$(sha256sum "$$tmp" | cut -d' ' -f1); \
[ "$$got" = "$$want" ] || { echo "API document changed: refresh the lock and projection together (expected $$want, got $$got)"; exit 1; }; \
export SPEC="$$tmp" SPEC_REF="$$ref" HANZO_SPEC_REPO="hanzo-inc/cloud"
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
# `cargo test` skips building them and the two tests that exec them died on
# `NotFound` instead of ruling. A gate that cannot run reads exactly like a gate
# that passed — the same defect `verify` had, wearing different clothes. The
# tests now run this same build themselves (`tests/spec_drift.rs` `maintainer`,
# hanzoai/cli#71), so a bare `cargo test` rules too; the build here keeps it a
# fingerprint check by the time they ask.
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
	@sh scripts/test-installer-strict.sh
	@sh scripts/test-generate-check.sh
	@node --test npm/*.test.js
	@python3 -m unittest discover -s tests -p test_installer.py
	@cargo build --quiet --features maintainer --bin driftgate --bin genproduct --locked
	# The cache has a home of its own, named before HOME moves — the same move
	# the gate in hanzo.yml makes for CARGO_HOME and RUSTUP_HOME. The issuer
	# tests (`trusts`, the credential store) fetch the live discovery documents
	# and cache them through dirs::cache_dir; with XDG_CACHE_HOME unset that is
	# $HOME/.cache, so a test run wrote the runner's HOME and the gate refused
	# it (forge run #85 on the PR #72 merge: hanzo.ai, hanzo.id, lux.id). The
	# cache belongs in the build tree, never in a person's HOME.
	@XDG_CACHE_HOME="$(CURDIR)/target/test-cache" cargo test --workspace --locked --no-fail-fast

build: ## The shipped binary.
	@cargo build --release --bin hanzo --locked

lint:
	@cargo clippy --locked --all-targets -- -D warnings

.PHONY: e2e e2e-installed
e2e: ## Real login, usage, local dev and cloud sessions (requires HANZO_E2E_TOKEN).
	@test -n "$${HANZO_E2E_TOKEN:-}" || { echo 'Set HANZO_E2E_TOKEN using hanzo auth token'; exit 1; }
	@cargo test --locked --test production -- --ignored --nocapture --test-threads=1

e2e-installed: ## Install published tools in isolation and test production end to end.
	@sh scripts/e2e-installed.sh
