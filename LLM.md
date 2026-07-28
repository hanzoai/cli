# LLM.md — hanzoai/cli

**What**: the `hanzo` CLI — one Rust binary (crate `hanzo-cli`, bin `hanzo`) that codes
with an AI agent, signs in, and deploys/manages/interacts with the Hanzo Open AI Cloud,
plus runs the network. clap + tokio, rustls TLS, no C deps, no daemon.

**Canonical role**: the terminal face of the Open AI Cloud — the CLI, NOT an SDK. SDK
model: full cloud SDK per language (`hanzo-<lang>/sdk`) + AI/agents lib (Python `hanzo`
flagship = `hanzoai/python-sdk`, Node `@hanzo/ai`). This repo is the canonical impl of
the CLI; discovery/marketing repos link here, never copy it.

**Install / run**:
- `curl -fsSL https://raw.githubusercontent.com/hanzoai/cli/main/install.sh | sh` (per-platform asset, sha256-verified; needs a GitHub token while the repo is private)
- from source: `cargo install --path .` · build gate: `cargo build` / `cargo test` / `cargo clippy --bin hanzo`

**Command surface** — resource-noun tree `hanzo <resource> <command>`, plus generated
product subcommands `hanzo <product> …` (derived from the spec, below — the ONE
interface to cloud; no `hanzo api` verb, no raw-path escape). Bare `hanzo [flags] [task]`
is an AI coding session (Claude Code or `dev` backend, Hanzo MCP attached, routed +
streamed to cloud when signed in).
- identity/money: `hanzo auth login|logout|show|list|use|token` (multi-identity, like `gh auth switch`), `hanzo usage|billing|connector`
- cloud: `hanzo agent|cluster|model serve|serve`; network/wallet: `hanzo network`, `hanzo wallet` (PQ cloud custody KMS/MPC or local)
- fabric/fleet: `hanzo fabric|node|runner`; ship: `hanzo init|dev|share`, `hanzo secret scan`; tooling: `hanzo docs|mdx|ui|mcp`, `hanzo config`, `hanzo version`
- local cloud: `hanzo host start|status|stop` (see "Where cloud RUNS")

**Where the cloud surface comes from** — `hanzo <product> …` is DERIVED, never
hand-maintained, and the client links no cloud code: it reads a spec, which is what it
does over the wire anyway.

```
hanzoai/openapi hanzo.yaml ─┐                        (shape: bodies, query params)
                            ├─ genspec ─→ spec/cloud.json ─→ genproduct ─→ generated.rs
api.hanzo.ai/v1/openapi.json┘                        (existence: the live route table)
```

- `cargo run --features genspec --bin genspec` — the REFRESH seam. Joins the two
  documents and writes `spec/cloud.json`. `--registry <url|path>` (default
  `https://api.hanzo.ai/v1/openapi.json`, or `<openapi>/generated/hanzo.json` for an
  offline run), `--openapi <hanzoai/openapi checkout>` (default `../openapi`).
- `cargo run --bin genproduct` — offline, deterministic: `spec/cloud.json` → the clap
  tree. `--check` compares instead of writing; `tests/spec_drift.rs` runs it, so
  `cargo test` is the drift gate.
- **The registry refutes at product granularity.** Cloud's route table is a projection
  of its live fiber router, so a product it serves ANY route under is a product it is
  complete for — an authored operation missing from the table is dropped. A product the
  table never mentions (the inference surface: `/v1/models`, `/v1/chat/completions`) is
  answered at the edge, not by that router, so nothing is refuted. Multi-segment path
  params come from the same reading: a fiber `*` arrives as `{wildcardN}`.
- What still needs a HAND decision in `genproduct.rs`: `EXCLUDE`/`DENY` (products a
  local command owns, or noise we choose not to surface), `REMAP` (machines+gpus →
  `compute`), `METHOD_PRIORITY`, `VERBS`, and the path→verb fold. Those are POLICY —
  none of them may encode "the server 404s this", which is the registry's job.
- A capability missing from the CLI is missing from a document: author it in
  hanzoai/openapi, or serve it in hanzoai/cloud. There is no third place.
- **Why build-time and not a spec fetched at run time**: cloud's lazy `cmd/host`
  serves no fleet-wide document. zip installs `/.well-known/openapi.json` only when
  an app registers typed ops, and the host registers none — it links zip and the
  manifest, nothing else — while `/v1/openapi.json` falls through the `ai` app's
  `/v1` catch-all to ONE plugin. The whole route table exists in one place only in
  the harness that writes `openapi.yaml`. Discovering the surface at run time would
  therefore mean starting all 108 subsystems to ask what they serve, which is
  exactly what laziness exists to avoid. The spec is a build input; the server is
  not asked what commands exist.

**Where cloud RUNS** — the same command tree targets a local checkout or api.hanzo.ai;
only the origin differs. `src/commands/host.rs` is the ONE origin resolver: it returns
the active network's `api`, and when that is loopback it also guarantees a host is
listening there — reusing a running one, or starting `bin/host` from a hanzoai/cloud
checkout (`$HANZO_HOST_BIN` overrides; never `$PATH`, where `host` is the DNS tool).
- `hanzo host start|status|stop`. Everything else starts it on demand, so these exist
  only for what demand cannot express: seeing it, and ending it.
- It is a DAEMON across CLI invocations on purpose. The host is lazy — a subsystem
  starts on the first request to its prefix (~15ms) and answers in ~0.5ms after — and
  stopping it per command would make every call cold. `stop` sends SIGTERM, which zip
  drains LIFO into every child it started.
- Loopback TCP, not a unix socket, for the front door: zip serves HTTP through
  fasthttp's `ListenAndServe` (`net.Listen("tcp4", …)`), so its http transport cannot
  bind a path. The transport that can is ZAP, whose binary framing this client does
  not speak — so ZAP gets the unix socket and HTTP gets 127.0.0.1, never 0.0.0.0.
- Against a local host a MISSING credential is not refused here: the call goes out
  with no bearer and the server decides, the same rule the tree already follows for
  a 403. State (pid, log, socket, `CLOUD_DATA_DIR`) lives in `<data>/hanzo/host/`.

**Key entry points**: `src/main.rs` (clap tree + bare-`hanzo` flatten), `src/commands/`
(one module per resource; `product/` = generated cloud tree), `src/iam/` (identity,
token store, HIP-0111 OIDC PKCE), `src/commands/code/` (coding wrapper). Secrets arrive
on stdin only, never argv; credentials in OS keychain or a `0600` file.

**Brand rules (hard)**: Hanzo is a full AI cloud, never an "LLM gateway" and never
positioned vs LiteLLM. `/v1/` paths only, never `/api/`. Zen models (`enso`, `zen5-…`)
are our own family — never name upstream models.

**Spec**: `~/work/hanzo/SDK-ARCHITECTURE.md` — the canonical one-way SDK/CLI model.
