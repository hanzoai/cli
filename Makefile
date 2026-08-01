# hanzo — the CLI. Three commands matter here, and the first two are about ONE
# question: is the committed command surface still a faithful projection of the
# cloud API document it names?
#
# `spec`       moves the projection forward onto a new document.
# `spec-check` refuses a tree where it has stopped being one.
# `verify`     asks the OTHER question — is the document still true of the running
#              server, in both directions? A capture can equal its inputs exactly
#              and still describe a surface production stopped serving, or miss a
#              product production started.
#
# Both go through scripts/generate.sh, which is also what hanzoai/ci's `client:`
# lane calls — so a maintainer, the push gate and the release all run the same
# derivation. A gate that runs a different generation than the writer is a gate
# that tests something nobody ships.

SHELL := /bin/bash
SPEC_REPO ?= hanzoai/cloud
SPEC_PATH ?= openapi.yaml
LOCK      := .spec-lock

.PHONY: help spec spec-check verify test build lint

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

spec: ## Regenerate spec/cloud.json + the product tree from the document in .spec-lock.
	@$(fetch_spec); ./scripts/generate.sh

spec-check: ## Refuse a capture that is no longer the projection of its own document. The D1 gate.
	@$(fetch_spec); ./scripts/generate.sh --check

# THE DRIFT GATE. `spec-check` asks whether the capture still equals its inputs;
# this asks whether the inputs are still true of production, in BOTH directions —
# a command whose route the server does not serve, and a served product no command
# reaches. It needs the built `hanzo` because reachability is a fact about the
# BINARY: a generated product that collides with a local command, or a relocation
# that stopped relocating, is invisible to any re-derivation of the same data.
verify: ## Refuse a tree the LIVE server contradicts, in either direction. The drift gate.
	@cargo build --quiet --bin hanzo --bin driftgate --locked
	@./target/debug/driftgate

test: ## Everything, including the derivation gates in tests/.
	@cargo test --locked

build: ## The shipped binary.
	@cargo build --release --bin hanzo --locked

lint:
	@cargo clippy --locked --all-targets -- -D warnings
