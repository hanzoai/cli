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

**This binary answers to TWO names, and that is by design.** cloud's Go control
binary (`hanzoai/cloud`, `cmd/hanzo`) owns a small set of control verbs and
delegates every other verb to US, through `cloud/cli/link.go` `fabricCLI()`,
which resolves `hanzo-node` on PATH first and only then `hanzo` (with a
self-exec guard). So a machine carrying the Go control binary installs THIS
artifact as `hanzo-node`. Same build, two names — there is no third CLI
codebase, and `hanzo-node` is not a separate product.

The trap that follows from it: a stale `hanzo-node` is INVISIBLE. The user types
`hanzo`, the Go binary delegates, and they get whatever old build is sitting
under the other name — with no version in sight. That is not hypothetical: a
v1.7.2 `hanzo-node` served roughly 150 phantom `/v1/cloud/<verb-noun>` commands,
because it predates `genspec.rs` and the registry refutation seam and was built
from the vendored `spec/products.json` (since deleted). Nothing was wrong with
the live code; the INSTALL was old. So:

> Whatever installs `hanzo` MUST install `hanzo-node` at the same version, and
> both must be this crate. An installer that ships one name, or ships two
> different versions, produces a CLI whose surface nobody can explain.

`hanzo version` now makes that skew LOUD (`commands/version.rs`): it resolves
`hanzo-node` on PATH exactly the way a delegating caller would — first match
wins, which is the precedence that let a stale build hide — and reports the
mismatch. Same file (hardlink or symlink) or same version is silent; a different
build prints `stale delegate: <path> is vX — this is vY`. It never repairs and
never refuses; it only stops the failure from being invisible.

Today they do not, and this is the open gap:
- `curl hanzo.sh | sh` runs `uv tool install` over PyPI and maps `hanzo-cli` →
  the command `hanzo`. PyPI `hanzo-cli` is a SEPARATE pure-Python project
  (`py3-none-any`), not this crate — so that path puts a different program at
  the name, into `~/.local/bin`, which outranks `/usr/local/bin` on a normal
  PATH. It also never installs the `hanzo-node` name at all (it maps PyPI
  `hanzo-node` to the command `hanzod`, the compute node, a third thing again).
- the brew tap (`hanzoai/homebrew-tap`) has NO `hanzo` formula — only `Dev.rb` /
  `hanzo-dev.rb` and a `hanzo-desktop` cask. `brew install hanzoai/tap/hanzo`
  does not exist.
- so the ONLY path that installs this crate as `hanzo` is our own `install.sh`.

**Command surface** — resource-noun tree `hanzo <resource> <command>`, plus generated
product subcommands `hanzo <product> …` (derived from the spec, below — the ONE
interface to cloud; no `hanzo api` verb, no raw-path escape). Bare `hanzo [flags] [task]`
is an AI coding session (Hanzo MCP attached, routed + streamed to cloud when signed in).

**The coding session, and its ONE resolver** — `hanzo code` starts it on one of three
DISTINCT backends: our own `dev` agent (hanzoai/dev, the DEFAULT), `claude`, and
`codex`. None is an alias of another — naming one runs THAT agent, or fails saying it
is not installed; silently substituting a different agent than the one someone named
would be worse than refusing.

Four entry spellings, ONE implementation:

```
hanzo code                 the default backend (dev)
hanzo code dev             hanzo code --dev        hanzo code --backend dev
hanzo code claude          hanzo code --claude
hanzo code codex           hanzo code --codex
hanzo dev                  shorthand for `hanzo code dev`
hanzo "fix the test"       bare session, default backend
```

`hanzo desktop` is the SAME session pointed somewhere else — at a browser and
desktop instead of the repo — and pins the Hanzo toolset on, because those tools
ARE how an agent drives a desktop. It is a `Target`, a value passed to the one
launcher, not a second command with its own flag.

`hanzo agent run` is GONE. Its `--mode code` was a second spelling of `hanzo
code` — the same options reaching the same `code::run`, differing in nothing —
and its `--mode desktop` was the only thing it alone could do, which is now
`hanzo desktop`. Two spellings of one implementation is the duplication this
tree exists to avoid; `hanzo code` is what a person types.

They differ in NOTHING but how the backend was written. `backend::select` is the only
place a backend is resolved, from (positional | flag | `--backend` | default), and
`main::code_session` is the only launcher; `hanzo dev`'s dispatch arm sets the backend
and hands straight to it. The one rule for the positional operand: it is the BACKEND if
it is exactly a backend name, otherwise it is the TASK — so `hanzo code dev` and
`hanzo code "fix it"` both work with no flag. Naming the backend twice
(`hanzo code claude --codex`) is REFUSED rather than resolved by a precedence rule
nobody could remember. If you are adding a spelling, add it to `select`; if you are
adding a launch path, you are forking the command.

`dev` and `codex` share one driver (`code/dev.rs` `Agent`) because ours began as a fork
of the other and they speak the same `-c` overrides and JSONL stream — they differ only
in the program exec'd, so a third is a const, not a file. The `model_catalog_json` we
write for them must carry EVERY field their parser requires: it rejects the whole
document on the first missing one, and that took the session down with it (a lost
context window is survivable, a session that cannot start is not).
- identity/money: `hanzo auth login|logout|show|list|use|token` (multi-identity, like `gh auth switch`), `hanzo usage|billing|connector`
- cloud: `hanzo serve`; network/wallet: `hanzo network`, `hanzo wallet` (PQ cloud custody KMS/MPC or local)
- fabric/fleet: `hanzo fabric|runner`; ship: `hanzo init|share`, `hanzo scan`; tooling: `hanzo config`, `hanzo version`
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
  cargo run --features genspec --bin genspec -- --openapi ../openapi
  cargo run --bin genproduct
  cargo test
  ```

  **`--registry` defaults to the wire, and a COMMITTED spec must have the wire
  among its sources** — `tests/spec_drift.rs` asserts it against the provenance
  `genspec` writes into `info.description`. A table captured to a local file
  cannot refute what the live one would, and cannot carry prose the live one has
  since gained; that is how 149 phantom `/v1/cloud/*` commands survived a version,
  and how 41 `platform` commands came to print their HTTP route where their
  description belonged.

  Mid-rollout a route lives in cloud `main` before api.hanzo.ai answers it, and
  the live table alone would refute it. Pass `--registry` MORE THAN ONCE: the
  readings are unioned (a route is served if EITHER has it — the truth during a
  rollout, both being readings of one router at two commits), the FIRST wins every
  conflict, and every source is named in the provenance. Put the wire first.

  ```
  cargo run --features genspec --bin genspec -- --openapi ../openapi \
      --registry https://api.hanzo.ai/v1/openapi.json \
      --registry <cloud-main route table>
  ```
- **PROSE IS NOT OPTIONAL, and there is no substitute for it.** Every command
  states what it does for the person running it. That sentence is the Go doc
  comment on the handler in hanzoai/cloud, lifted by zipdoc into the published
  route table and joined in by `genspec` — so a command with nothing to say is a
  missing doc comment in cloud, not a hole this repo may fill. `genproduct`
  therefore REFUSES to emit an undescribed op, naming the commands and the remedy;
  `every_command_says_what_it_does` pins the same rule against a hand-edited tree;
  and `leaf_named` has no branch for an empty summary, because the state cannot
  exist. The `METHOD /path` fallback that used to fill the column is deleted, not
  made conditional: a projection cannot repair its source, and every branch that
  handles "the source didn't say" turns a fixable gap into a shipped help line
  that reads deliberate. Nobody files a bug against a design choice. The route
  still appears in LONG help — reference detail, and what a user quotes when
  filing a bug — never in prose's place.

  Two content-free channels remain, both for want of a channel rather than a
  fallback, and neither may be papered over here. A product group with no `tags`
  description prints ``` `x` cloud operations ``` (20 products): 10 have a live
  tag whose description is EMPTY, so the missing thing is a Go PACKAGE doc comment
  in cloud; 10 are the gateway/edge inference surface with no tag in either
  document, which needs a tag authored in hanzoai/openapi AND `genspec` taught to
  read product tags from the master as well as the registry. A resource NODE
  (`agents sessions`, `admin infra`) has no channel at all — the seam is a tag per
  node emitted by zipdoc from the Go sub-router's doc comment, the same lift as
  the package doc. Until those exist this is a stated gap; a stated gap beats an
  invented sentence.
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

**The session channel** — `hanzo code` is watchable and steerable from the dashboard while it
runs. One channel, two directions, one transport, one vocabulary.

- **OUT** (`src/commands/code/session.rs`): register on `POST /v1/agents/sessions`, forward each
  parsed event to `POST …/:id/events`, close with `PATCH …/:id`. Unchanged; already shipped.
- **IN** (`src/commands/code/control.rs`): drain `GET /v1/agents/sessions/:id/control?after=<seq>`
  once a second. Cursor-driven, so an applied command is never redelivered and a reconnect replays
  exactly what was missed, in order, once. The durable log IS the buffer — nothing is queued locally.
- **The op set is CLOUD'S, not ours.** `pause` · `resume` · `stop` · `message` — the `Cmd*`
  constants in `cloud/apps/agents/sessions.go`, already mirrored by `event::Kind::Control`. Naming
  CLI-side synonyms (`interrupt`, `steer`) would be two spellings of one verb, so the wire words
  ARE the type. `hanzo` implements three of the four; `resume` is a no-op against an already-running
  session (a *paused* session has no process — reopening it is `hanzo code --resume <id>` on the
  machine that holds it).

| Command | Signal | What Claude does | Session after |
|---|---|---|---|
| `pause` | SIGINT | aborts the in-flight tool, writes `[Request interrupted by user for tool use]` + a final `result` (`terminal_reason:"aborted_tools"`), flushes the transcript, exits 0 | `paused` — resumable, same id |
| `stop` | SIGTERM | same abort path, exits 143 | `done` |
| `message` | SIGINT | as `pause`, then the supervisor relaunches with `--resume <sid>` + the new prompt | stays running, same id |

- **The status comes from the COMMAND, never the exit code.** A commanded stop exits 143 and is
  still a clean `done`. Folding those readings into one `bool` is what made a deliberate stop look
  like a crash; `finalize` now takes a `Status` the caller decides.
- **Signals go to the child's own pid, never the process group.** Putting the child in its own
  group would make it a background group against an inherited tty and earn it SIGTTIN on the first
  stdin read. Claude tears down its own tool subprocesses on abort (verified: no orphan survives a
  stopped `sleep 300`), so the direct pid is sufficient.
- **A steer that beats start-up still lands.** If the turn never disclosed a session id there is no
  transcript to preserve, so it relaunches fresh with the new instruction rather than dropping it.
- **AuthZ**: the CLI sends the bearer and NOTHING else — never an org. Cloud derives the tenant from
  the JWT `owner` and 404s a session belonging to another org, on both the detail read and the
  drain. Cross-org control is therefore impossible rather than merely refused, and a refused drain
  is an ERROR here, never an empty-but-successful page that would read as "no commands" (which would
  also wrongly advance the cursor). Proven by
  `control_tests::another_orgs_stop_cannot_terminate_our_child`.
- **Why HTTPS and not ZAP**, since the local wire is ZAP: there is no ZAP client to use. `zap/rust`
  is server-only (no client type at all), TCP, plaintext, no auth, strict request/response with no
  server-push; the `hanzo-zap` crate on crates.io is a *different*, stubbed codebase; and
  `zap-proto/ws` — the bidirectional/pubsub sub-protocol — is a README and a schema file with no
  implementation in any language. Pointing today's ZAP at a remote peer would also ship a
  developer's prompts and code in cleartext, which is exactly what `src/zap.rs` forbids in writing.
  So this follows the rule the repo already documents: **local = ZAP over a unix socket, remote =
  HTTPS until the ZAP transport carries its own session crypto.** Both directions ride the one
  `crate::http` seam, so when that lands they move together — there is no second wire to migrate.

**The Claude config home, and the carrier** — `hanzo code claude` and the user's own
Claude Code are two products sharing one binary. Two modules keep them decomplected;
both are pure functions of the resolved ROUTE, so every reader agrees without coordinating.

- **`code/home.rs` — whose config home this run uses.** A ROUTED run
  (`Route::Via`) relocates `CLAUDE_CONFIG_DIR` to `~/.hanzo/claude`, because that is
  when a saved `/model` could outrank `ANTHROPIC_MODEL` and when Hanzo's injected
  tiers could leak back out. `--no-route` (`Route::Inherit`) and `FailClosed` keep
  `~/.claude` — on Linux the account the flag promises IS a file in that home
  (`.credentials.json`), so relocating would hand the user a login prompt instead of
  the pass-through. `home::relocate` = "must we set the env var"; `home::of` = "which
  home will actually be read" (transcripts, theme). Seeding is WRITE-ONCE and happens
  only where we relocated. **Everything that reads or writes that home — the launch
  env, `transcript_path`, `theme::apply` — must take the route.** A path hard-coded to
  either home is a file that does not exist on the other one.
- **`code/tier.rs` — the zen ⇆ carrier table, the ONE source of truth.** Claude Code
  budgets a context window only from ids it RECOGNIZES, so a custom id (`zen5-pro`)
  gets the fallback budget and is clamped client-side. A tier therefore names a
  CARRIER (`claude-opus-4-8[1m]`) that Claude budgets from, and a per-run
  `--settings modelOverrides` overlay rewrites it back to the zen id before the
  request leaves the process — verified live: Claude displays
  `model claude-sonnet-4-6[1m]` while the gateway receives `zen5`. The overlay rides
  the RUN, never the seeded home: persisted, it would rewrite a `--no-route` or
  direct-Anthropic session's model to a zen id and 404 against api.anthropic.com.
  Requires Claude Code v2.1.200+.
  - The table also fills every `ANTHROPIC_DEFAULT_*_MODEL` slot. This is not
    branding: Claude resolves subagents, `/compact` and its background work through
    those slots, and their built-ins are `claude-*` ids the gateway does not serve,
    so an unset slot 400s every subagent.
  - **Key a slot on its TIER, never on its zen id.** `zen5-pro` fills two slots (opus
    and fable); an id lookup is ambiguous between them and silently gave the
    "max effort" slot the opus carrier.
  - A model absent from the table passes through untouched — the table is a lookup,
    never an allowlist. The gateway stays the sole authority on which ids are valid.
  - A CARRIED model also gets `--append-system-prompt` naming its real tier, because
    the carrier is exactly what would otherwise make it introduce itself as Claude.
    An append, so the harness keeps its own tool-use/safety prompt; an untiered id
    rides no carrier and so claims nothing.

**Key entry points**: `src/main.rs` (clap tree + bare-`hanzo` flatten), `src/commands/`
(one module per resource; `product/` = generated cloud tree), `src/iam/` (identity,
token store, HIP-0111 OIDC PKCE), `src/commands/code/` (coding wrapper). Secrets arrive
on stdin only, never argv; credentials in OS keychain or a `0600` file.

**Brand rules (hard)**: Hanzo is a full AI cloud, never an "LLM gateway" and never
positioned vs LiteLLM. `/v1/` paths only, never `/api/`. Zen models (`enso`, `zen5-…`)
are our own family — never name upstream models.

**Spec**: `~/work/hanzo/SDK-ARCHITECTURE.md` — the canonical one-way SDK/CLI model.
