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

**The lineage contract — which half is authoritative about what.** The two inputs
are not redundant readings of one truth; each owns a different question, and
neither can answer the other's.

- **The AUTHORED master decides EXISTENCE.** `genspec` ITERATES `hanzo.yaml` and
  nothing else. The registry never ADDS an operation — it only removes. So a route
  the server serves perfectly is invisible to `hanzo` until hanzoai/openapi names
  it. This is the direction people get backwards, and it is the expensive one: it
  fails silently, with a working server and a CLI that has never heard of it.
- **The REGISTRY decides SERVED-NESS, per owned product.** Any route under
  `/v1/<product>/` makes cloud the authority for that product, and an authored
  operation its table lacks is dropped. Silence about a product refutes nothing.
- **The authored half is the SHAPE source only while handlers stay untyped.** Most
  of cloud is raw fiber handlers with no Go type to read a body schema off, so
  bodies and query params come from the authored spec. As handlers become zip
  typed ops their schemas appear in the registry document with prose lifted from
  the doc comments, and the authored half shrinks. It is a shrinking dependency,
  not a permanent one — `/v1/admin/plugins` is what the end state looks like:
  cloud emits the full schema, and the authored spec exists to make it REACHABLE.
- **Refresh:** `genspec` then `genproduct`, in that order, then `cargo test`
  (`tests/spec_drift.rs` re-runs `genproduct --check`, so a stale `generated.rs`
  fails the build).

  ```
  cargo run --features genspec --bin genspec -- \
      --openapi ../openapi --registry <route-table.json>
  cargo run --bin genproduct
  cargo test
  ```

  Prefer the wire for `--registry`. Use a captured table when the deployed build
  is BEHIND the one whose routes you are adding — a route in cloud `main` but not
  yet on api.hanzo.ai is refuted by the live table and silently dropped. The
  capture that answers this is the UNION of the live table and cloud main's own
  `openapi.yaml`: refutation needs a product owned AND no matching route, so a
  union treats a route as served if EITHER reading has it, which is exactly the
  truth during a rollout. Both are readings of one router at two commits.
- **A refuted operation is REMOVED, never repaired.** When a served route changes
  SHAPE, the authored path stops matching, the whole operation is dropped, and the
  command disappears rather than pointing somewhere new. `/v1/kms/orgs/{org}/secrets`
  → `/v1/kms/secrets` (the org moved into the token) did exactly this: `hanzo kms
  secrets` addressed a 404 until the contract was corrected. So a 404 from a
  generated command is a bug in hanzoai/openapi, and the fix is never in this repo.
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
- **The local wire is ZAP, not HTTP.** The host serves the SAME routes on both; ZAP
  is the fleet's primary transport and HTTP is a secondary view for third parties who
  cannot speak it. The CLI is first party, so locally it speaks ZAP over a unix socket
  in `<data>/hanzo/host/` — `src/zap.rs`, a byte-for-byte port of
  `github.com/zap-proto/http`'s codec, pinned by golden vectors emitted from the Go
  encoder itself (`src/zap/tests.rs`). No HTTP grammar and no port for the CLOUD
  CALL: `strace -e trace=connect hanzo authz health` shows `connect(AF_UNIX,
  …/host.zap.sock)` twice — the `/healthz` probe and the call — and no `AF_INET`
  for either. The host is still told to bind loopback TCP because `cmd/host`
  always listens on both; the cloud path never dials it.
  - What DOES still open a TCP socket on a local run, so nobody is surprised by an
    strace: `crates/event`'s best-effort telemetry POST to `{api_base}/v1/event`,
    which owns its own reqwest client and knows nothing about ZAP. One `AF_INET`
    to 127.0.0.1:3690 per invocation, fire-and-forget, `HANZO_DO_NOT_TRACK`
    silences it. It is a second wire to the same host and the only one left.
  - **Windows is the one platform where local is HTTP**, because it has no unix
    socket to speak ZAP over. `Origin::Local` therefore carries the socket PATH on
    unix and the loopback BASE URL on Windows — one variant, because "is this the
    local host" is the question the credential rule asks and the wire is not it.
    `src/zap.rs` is `#[cfg(unix)]`; so is the `-zap` flag handed to the spawned
    host, and `healthy()` probes whichever wire that platform actually uses.
    Forgetting this is what broke the windows-amd64 release asset in v1.9.3:
    `tokio::net::UnixStream` does not exist there, and no CI caught it because the
    release matrix had been deleted the same day.
  - Wire notes that will bite a reimplementation: the socket's 4-byte length prefix is
    BIG-endian and everything inside the frame is LITTLE-endian; a slot's `relOffset`
    is measured from the slot's own position; `relOffset == 0` is NULL regardless of
    the length word; an empty field is a `{0,0}` slot, NOT a zero-length entry.
  - Remote stays HTTPS: zaphttp has no session crypto yet (its transport reserves an
    X-Wing PQ KEM handshake for later), so TLS still terminates at the ingress. That
    is the EXCEPTION, and it ends when the ZAP transport carries its own.
  - Measured, so nobody has to guess: steady state ~66µs (ZAP/unix) vs ~98µs
    (HTTP/loopback) mean over 3000 requests; isolating the WIRE alone (both over TCP)
    it is ~91µs vs ~100µs. Real but ~32µs — against ~10-20ms of CLI process startup
    that is invisible to a user. The reason to do it is one wire and one contract,
    not speed.
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
