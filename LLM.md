# LLM.md — hanzo CLI

## Overview
The `hanzo` CLI: one binary to drive Hanzo's top-level concerns — network,
wallet, node/hanzo.network — plus cloud auth, cluster, deploy, and the
TS/Python SDK proxies. It is the CLI half of "it all fits together" with the
console + cloud + the fabric: ONE network model, ONE wallet model, ONE way.

## Tech Stack
- **Language**: Rust (crate `hanzo-cli`, binary `hanzo`), clap derive, tokio.
  TLS is rustls (no OpenSSL); no C dependency links into the shipped binary.
- Secrets: the PORTABLE credential store (`iam::token::vault`) — IAM tokens AND
  wallet keys. ONE seam ([`Vault`]), backend chosen at runtime: the native OS
  keychain on macOS/Windows when it answers, else an owner-only `0600` file
  (`~/.local/share/hanzo/credentials`). See "Credential store" below.

## Build & Run
```bash
cargo build            # build gate
cargo test --bin hanzo # unit tests (incl. wallet derivation vectors + FileVault)
cargo clippy --bin hanzo
```

## Credential store (`src/iam/token.rs`) — runs everywhere, one seam
A credential must be reachable everywhere `hanzo` runs — desktop, container,
headless server, SSH, CI. So `Vault` (get/set/remove a secret by key) has two
production backends, chosen once per run by `token::vault()`:
- **`Keyring`** — native OS keychain. Compiled ONLY on macOS (Keychain) and
  Windows (Credential Manager), whose system frameworks link with zero C build
  dependency. Used when a probe shows the backend answers; a headless/locked
  keychain falls back to the file.
- **`FileVault`** — an owner-only (`0600`) file, atomic through `crate::private`,
  the same guarantee `~/.ssh/id_*` and `config.toml` rely on. No native
  dependency, so it works in a container and cross-compiles to every target. It
  is the store on Linux (secret-service needs a D-Bus session absent in a
  container/CI and a C `libdbus` binding that does not cross-compile) and the
  fallback elsewhere. Concurrent writers serialize on the config's cross-process
  `Lock` and re-read, so a `set` of one key never drops another.

IAM tokens (`{brand}/{owner}/{name}`) and wallet keys (`wallet:{address}`) share
ONE store with disjoint key namespaces — one credential store, one way in.

## Multi-platform release (`.github/workflows/release-matrix.yml`)
`curl hanzo.sh | sh` installs a working `hanzo` on every platform. All five
targets — linux-{amd64,arm64}, darwin-{amd64,arm64}, windows-amd64 — cross-build
from the ONE self-hosted Linux pool (`hanzo-build-linux-amd64`) via
`cargo-zigbuild` (Zig 0.13 as the cross-linker); NO GitHub-hosted runners. macOS
links against a pinned, checksum-verified vendored MacOSX SDK (`SDKROOT`). Each
target is tarred as `hanzo-<os>-<arch>.tar.gz` + `.sha256` and published straight
to the GitHub release over the REST API (the runner has no `gh`, and artifact
storage is exhausted). A tag-vs-crate guard refuses a mislabeled release.
`install.sh` resolves the asset from `uname -s`/`-m` and verifies the sha256
before unpacking.

## Command surface (`src/main.rs` clap tree → `src/commands/*`, `src/iam/*`)
- `login` / `whoami` / `switch` / `logout` — IAM OIDC PKCE S256 + the identity
  model (`src/iam/*`, HIP-0111; below).
- `network list|current|use <name>|add <name> …` — the network model (below).
- `kms list|get|set|rm` — secrets, the only place they live (below).
- `wallet show|address|create|import <secret>|use <addr>|list` — wallet (below).
- `billing balance|deposit` — the prepaid wallet's money (below).
- `node up|status|join <network>|stop` — run/join hanzo.network with hanzod.
- `cluster topology|models|route|placement|chat|search` — talk to a hanzo node
  (default node URL = the active network's `api`).
- `deploy` — targets the active network; the active wallet signs (auto-provisions
  one if none, on a real deploy).
- `code [--backend claude|dev] [--no-link] [--project-mcp] [--resume <id>] [task]` —
  wrap a local coding agent as a session-aware, portable, trackable object; a
  signed-in run links to your cloud by default, `--no-link` opts out (below).
- bare `hanzo` (no subcommand) — shorthand for a linked interactive `hanzo code`
  (link forced on; falls back to a local run when nobody is signed in).
- `agent`, `build`, `dev`, `init`, `docs|mdx|ui|mcp` (TS proxies).

## Identity model (`src/iam/*`) — MULTI-identity, like `gh auth switch`
One human holds many principals: `z@hanzo.ai` is BOTH `admin/z` (SuperAdmin, the
reserved `admin` org) and `hanzo/z` (org owner). The CLI holds every one of them
at once and switches between them; a second login never clobbers the first.

- **`Identity` is a VALUE derived from the token's own claims** (`identity.rs`).
  Casdoor names a principal `owner/name`; `owner` is ALSO the org the gateway
  bills AND the SuperAdmin predicate (`owner == "admin"`) — one value, three
  uses. It is decoded from the access token LOCALLY (userinfo carries no
  `owner`). That decode is unverified and LABELS OUR OWN STORAGE ONLY — it is
  NEVER an authz decision. SuperAdmin and billing are decided server-side from
  the token the server verifies; forging `owner` only mislabels the forger's own
  keychain slot. `owner`/`name` are validated (no `/`, no traversal) so a claim
  can never address another identity's slot.
- **Storage reuses the wallet law**: secret in the credential store, metadata in
  config. Store entry = `{brand}/{owner}/{name}` → `TokenSet`, so nothing
  clobbers (`token.rs`). `config.toml` `[auth]` is the NON-SECRET index —
  the identity list + the active identity per brand — and never holds token
  material. The index exists because the store has no portable enumeration
  API; it is what makes listing work offline.
- **Every config write is a transaction** (`Config::update`, the ONLY mutator —
  there is no bare `save`). The config file is a shared mutable PLACE: several
  `hanzo` processes write it at once (a `hanzo code` migrating a legacy
  credential while you `hanzo login` in another terminal). So `update` takes a
  cross-process lock (`std::fs::File::lock` on a sidecar `config.toml.lock` —
  never on the config itself, whose inode the write replaces; the kernel drops it
  on process death, so a crash cannot wedge the fleet), RE-READS current truth,
  applies the mutation to THAT, and writes tmp+rename+fsync so a reader sees the
  old file or the new one, never a torn one. Deciding on a stale snapshot would
  silently revert another process's write — for the auth index that means landing
  on a principal you did not choose, i.e. the hard invariant broken by a race
  rather than a cascade. On the real fleet the legacy key is the ORG owner, so
  the reachable direction was DEMOTION — silently reproducing the deposit-403
  incident. Hence migration's "only if nothing is active" check runs INSIDE the
  locked closure.
- **`store.rs` is THE one way** any command resolves a credential
  (`active_token`). All six consumers go through it — `login`/`whoami`/`logout`,
  the `hanzo code` routing bearer, and the cloud-custody wallet — and a test
  (`no_consumer_bypasses_the_active_identity_seam`) fails the build if a seventh
  reads the keychain directly. The old bug WAS a per-brand `token::load(brand)`
  at six call sites.
- **HARD INVARIANT — the active identity changes ONLY by explicit user action**
  (`login`, `switch`). No auto-switch, no fallback, no cascade. If the active
  identity's credential is missing the run is UNAUTHENTICATED; it never quietly
  becomes another identity you hold. Signing out of the active identity signs you
  OUT — it never promotes the survivor. Acting as the wrong principal is worse
  than not acting.
- **Billing follows identity for FREE.** The CLI never sends an org; the gateway
  derives it from the JWT `owner`. So `Identity.owner` IS the billing key and
  `switch` moves billing with zero new machinery — there is deliberately NO
  org/billing flag.
- Verbs (flat, no `auth` group): `login` ADDS an identity and makes it active
  (re-login = idempotent UPDATE, not a duplicate row); `whoami` shows the active
  one, `--all` lists them (the ONE listing surface — there is no `identities`
  verb); `switch [owner/name|owner]` selects (verifying we actually HOLD the
  credential — it never prints a billing org it has not confirmed), bare toggles
  when exactly two are held and lists when more; `logout [IDENTITY] [--all]` removes one or all.
  `login --token -` reads stdin so a token never lands in argv or shell history,
  and requires an identity-bearing JWT. A key is not an identity: an `hk-`
  gateway key has no derivable principal, so storing it would mean FABRICATING
  one — it is refused, with an error that names `hanzo login` instead of dead-
  ending. If a real M2M caller ever needs `hk-`, the answer is an env read at the
  point of use (`HANZO_API_KEY` → the gateway), NOT an identity in the store.
- **Migration is forwards-only, one shot.** A legacy bare-`brand` keychain entry
  is re-filed under `{brand}/{owner}/{name}`, indexed, made active (only if
  nothing else is — carrying a login forward is not a switch), and the legacy key
  is DELETED. No dual-read, no compat shim. An unidentifiable legacy blob fails
  closed; `hanzo login` supersedes and clears it.

## `hanzo code` (`src/commands/code/`) — session-aware coding wrapper
Wraps Claude Code (`claude`) or `dev` (codex) with three things wired natively,
plus resumable/portable sessions. ONE trait (`backend::Backend`) both backends
satisfy; the orchestrator (register → spawn → stream → finalize) is identical.

- **Session link + live stream (on by default when signed in).** A signed-in run
  links unless you opt out with `--no-link` (or a persisted `code.link = false`);
  it registers on `POST /v1/agents/sessions` with the hanzo.id bearer and forwards
  the backend's structured events, mapped to cloud's closed vocab
  (`message|tool-call|spawn|log|status|control`; `session.rs`+`event.rs`). The
  gateway derives the org from the JWT `owner` claim — the CLI never sends an org,
  so a session only ever streams to its OWN org and cross-tenant attribution is
  impossible. Privacy gate is STRUCTURAL and unchanged by the default: an
  UNAUTHENTICATED run has no bearer, so it holds no client, doesn't request the
  structured stream, and nothing can reach cloud — link-by-default therefore only
  affects users who are signed in and own the cloud their session streams to.
  Headless (`[task]`) = stdout stream-json parsed+forwarded+mirrored; interactive
  = native TTY + (Claude) transcript tail at
  `~/.claude/projects/<slug>/<sid>.jsonl`.
- **Hanzo MCP attached — repo `.mcp.json` is trust-gated.** `resolve_mcp` →
  `hanzo-mcp` (or `uvx hanzo-mcp`) `--project-dir <cwd>`. Claude via
  `--mcp-config` + `--strict-mcp-config`, so Claude uses ONLY the servers we pass
  and ignores every auto-discovered source — most importantly the repository's
  own `<cwd>/.mcp.json`. A repo is untrusted, and any stdio MCP server it declared
  would inherit this process's env (which carries the model routing key), so it
  must never load by default. The Hanzo toolset is layered by default; the repo's
  own `.mcp.json` is loaded ONLY with the explicit `--project-mcp` opt-in. `dev`
  never reads a repo-local MCP config (its servers come from `CODEX_HOME` +
  installed plugins), so it has no such vector; we attach Hanzo additively via
  `-c mcp_servers.hanzo.*` (never repoints `CODEX_HOME`, so the user's
  config/login is intact). Missing server warns, never blocks.
- **hanzo.id auth + universal usage.** Signed-in runs route model calls through
  the gateway so tokens/cost meter into cloud_usage/o11y: Claude via
  `ANTHROPIC_BASE_URL`+`ANTHROPIC_AUTH_TOKEN`; `dev` via native `hanzo` provider
  + `HANZO_USER_KEY` (custom provider `-c` for non-default network api). Token
  rides in env, NEVER argv/logs. `--no-route` opts out. The routing token is the
  hanzo.id bearer (the gateway accepts it for inference); it is exposed only to
  the model CLI and the Hanzo MCP server (the repo `.mcp.json` trust-gate keeps
  it away from repo-declared servers). A per-session, model-scoped, short-TTL key
  would shrink that blast radius further — that needs a cloud token-exchange
  endpoint (no such mint exists today; `POST /v1/iam/keys` only rotates the
  user's one org-wide `hk-` key) and is a tracked cloud follow-on, not a CLI one.
- **Portable/resumable.** On register we emit a no-secret `status` context event
  (machine-id/host/os/arch/cwd/repo+ref/backend+version; git remote credentials
  scrubbed — `context.rs`). The backend's own resume handle + transcript pointer
  are persisted to a machine-local store (`~/.local/share/hanzo/code/sessions/`)
  and mirrored to cloud as a `status` event (web-continue seam). `--resume <id>`
  restores cwd/repo and relaunches the backend's native resume (`claude --resume`
  / `dev exec resume` / `dev resume`), re-attaching the SAME cloud id when the
  session is still live (running/paused) or forking a new one with `resumedFrom`
  lineage when it's terminal (cloud forbids reopening a terminal session).
- **Resume is org-scoped — three things are braided, only one crosses.** (1) the
  backend conversation (`<slug>/<sid>.jsonl`) and (2) the CLI's own store
  (cloud-id ↔ backend-sid ↔ cwd) are LOCAL and carry across any identity; (3) the
  cloud session record is ORG-SCOPED server-side (the gateway injects the JWT
  `owner`), so after `hanzo switch admin/z` a session belonging to `hanzo/z`
  CANNOT re-attach — `GET /v1/agents/sessions/{id}` refuses it. That refusal is
  tenant isolation working and is never routed around. So a cross-org resume
  keeps the FULL local conversation and registers a NEW cloud session billed to
  the now-active identity from turn one, and SAYS so — never a silent fork, never
  a mis-bill. The resume record carries the `owner/name` that created it (LOCAL
  only — the CLI still sends no org, and `resume_payload` is an explicit
  allowlist that omits it), so the CLI decides this honestly instead of
  discovering it as a 403. NO `resumedFrom` pointer is written for a session we did not
  VERIFY: lineage is recorded only when `GET` succeeded (the id exists and is
  ours). Every failure — 403, 404, 5xx, timeout, DNS — fails closed and registers
  with no lineage, enforced in `resolve_cloud_session` itself rather than only at
  its caller. The new org's record must never reference a session it cannot
  resolve, so that lineage stays in the local store — the only place it is true.
  The cloud id is also gated on the active NETWORK (`rec.api`): an id minted by
  one control plane means nothing to another, so `hanzo network use local` +
  resume of a prod session registers fresh rather than handing over a foreign id
  (same host+api filter the run-target store already uses).
  A record of unknown provenance (predating the field) cannot be proven ours and
  is treated exactly like a cross-org resume.
- **Run-target (machine capability + live metrics).** A linked run also registers
  the MACHINE it runs on so mission-control knows WHICH computer a session is on
  and whether it can take more work. `context::Machine::capture` reads, best-effort
  and cross-platform (linux `/proc` + macOS `sysctl`/`vm_stat`, GPUs via
  `nvidia-smi`/`lspci`/`system_profiler`), a static `Spec` (os/arch/cpus/memory/
  gpus) and a live `Metrics` sample (loadavg/mem used+free/gpu-util). Every probe
  runs with a hard 2s deadline and a MINIMAL env (PATH only) — a probe can never
  hang the session, and NO environment value can influence or leak into the data
  (the same privacy hard-line as the context snapshot). It is upserted to
  `POST /v1/agents/targets` (label=host, host=host the upsert key, kind `gpu` when
  GPUs are present else `laptop`, a derived capacity summary, spec+metrics); the
  minted id is persisted to `~/.local/share/hanzo/code/targets/<machine>.json` and
  reused for a cheap `PATCH` heartbeat on the next run (falling back to register if
  the target is gone / the org changed). The metrics timestamp is server-stamped —
  the CLI never sends `at`. This is DETACHED and BEST-EFFORT: capture + the cloud
  write happen off the critical path and can never block or fail the coding
  session, and it is gated on the SAME structural auth check as the session link
  (`links_target`), so an unauthenticated run registers no target and reaches cloud
  not at all. One HTTP seam (`http::send_json`) carries both the session and target
  clients (bearer only; the org is derived server-side from the JWT `owner`).
- **Sandbox:** never widened — we never pass `--dangerously-skip-permissions`
  (Claude) or `--yolo`/`--full-auto` (`dev`); the user's mode governs, extra
  flags only via trailing `-- <args>` passthrough.
- Tested with fixture streams + a real subprocess (`cat` a fixture) + a mock
  cloud (`testmock.rs`); live `claude`/`dev` binaries are the only unproven seam.

## Network model (`src/commands/network.rs`) — same as the console + fabric
Sovereign L1 ⇒ `network_id == chain_id`. Built-ins mirror the console selector
and the node's `hanzo-mining` `NetworkType`:

| name    | network_id / chain_id | rpc                                  | api            |
|---------|-----------------------|--------------------------------------|----------------|
| mainnet | 36963                 | https://rpc.hanzo.network            | api.hanzo.ai   |
| testnet | 36962                 | https://rpc.testnet.hanzo.network    | api.hanzo.ai   |
| devnet  | 36964                 | https://rpc.devnet.hanzo.network     | api.hanzo.ai   |
| local   | 1337                  | http://localhost:9630/v1/bc/C/rpc    | localhost:3690 |

`network add` defaults `chain_id` to `network_id` (sovereign). Selection + custom
networks persist to `~/.config/hanzo/config.toml` (`config.rs`, non-secret only).

## Secrets (`src/commands/kms.rs`) — `hanzo kms`, the local instrument of "KMS or nowhere"
The core lifecycle against cloud's `/v1/kms/orgs/{org}/secrets`, and NOTHING
else. `rotate`/version history exist in the standalone luxfi/kms SDK but cloud
does not mount them, and a verb the server cannot answer is worse than no verb.

- **A value has one way in and one way out.** IN: `set` reads stdin, always —
  there is no `--value` and no positional value, so no argv, `ps`, shell history
  or CI log can hold it. That is a property of the GRAMMAR (a value-bearing argv
  is a parse error), not of the handler's discipline, and a test pins it. Same
  reason `login --token -` reads stdin. OUT: `get` writes the raw bytes to
  stdout — no newline, no label — so it pipes byte-exactly. The bytes never
  differ by where stdout points; a secrets tool you cannot pipe is one you cannot
  trust, and a TTY guard would be theatre (passed reflexively, and the
  shoulder-surfer sees the value either way). A value is never logged, never on
  disk, never in the config; `list` cannot carry one (the server's listing has no
  value field).
- **The org is ADDRESSED, never asserted.** These routes name the org in the PATH
  (unlike the agents plane), so the CLI cannot stay silent about it. It sends the
  ACTIVE IDENTITY'S OWN `owner` — the claim on the very token it authenticates
  with, via `iam::store::active_token`, never a user choice (there is no `--org`,
  and a test pins its absence). The server re-derives the org from the JWT it
  verifies and 403s any mismatch, so the segment is an address: forging it can
  only 403 you against yourself. `X-Org-Id` is never sent. `hanzo switch` moves
  the secret namespace exactly as it moves billing — no new machinery.
- **ONE address type.** `NAME` or `sub/path/NAME`, identical in every verb (the
  same split the server makes: last segment = name, rest = sub-path). `list`
  prints addresses in exactly the form `get` takes, so the two compose:
  `hanzo kms list | xargs -n1 hanzo kms get`. `.`/`..`/empty segments are refused
  BEFORE a URL is built — a URL library normalises them away, and `../../evil/…`
  would silently re-address another org rather than ask what the user typed.
  Every segment is percent-encoded: the server's only name bans are `/` and
  control chars, so `X?env=prod` is a LEGAL name and a raw one in a URL would
  fetch a different secret.
- **`set --env` is required, reads default to `default`** — mirroring the server
  exactly rather than papering over it. It refuses to guess on a WRITE because a
  silent `default` commits to a bucket the env's readers never resolve (the split
  that once left a live credential stale in prod); a read cannot plant a value,
  so it keeps the compat default.

## `hanzo <product> <resource…> <verb>` (`src/commands/product/`) — cloud, as commands
The ONLY interface to cloud: every capability is a real subcommand with real
`--help` AND real TYPED flags — WITHOUT hand-writing them. There is NO `hanzo api`
verb and no raw-path escape. A build-time generator folds the hand-authored
OpenAPI specs into committed DATA; the runtime builds a clap tree from that data
and dispatches it through the one authenticated seam that lives IN THIS MODULE
(`call` → `http::send`, origin from `network`, bearer from `store`). Currently
2117 coordinates across 87 products (658 typed-flag + 219 `--data`-fallback writes).

- **Source of truth = the authored specs** (`spec/products.json`, vendored from
  hanzoai/openapi — the per-product OpenAPI 3.1 specs as one JSON). It is the ONLY
  snapshot; the router dump is gone. Committed, so codegen + `--help` are offline
  and invariant to `hanzo network use`; regenerate with `cargo run --bin genproduct`.
- **A product with no authored spec is ABSENT.** No passthrough, no `hanzo api`
  fallback to paper over it — an unenumerable/unauthored product simply is not in
  the CLI, and that gap closes by AUTHORING the spec. `~66` live router products
  (store, marketing, dataroom, knowledge, sign, sentry, usage, team, tools, …)
  are therefore absent today — the openapi-repo completion is the work that adds
  them.
- **The fold is a TOTAL function over path segments** (`bin/genproduct.rs`).
  Literals → resource groups; params → positionals; terminal segment + method →
  the verb. `GET /p/r`→`list` (collection) / `get`; `GET /p/r/{id}`→`get`; `POST
  /p/r`→`create`; item `PATCH|PUT|POST /p/r/{id}`→`update`; `DELETE …/{id}`→`rm`;
  `POST …/{id}/{action}`→`<action>`. A param names a group ONLY in a param-stack
  (`{a}/{b}`), and a collection-root write gets a DISTINCT verb (`clear`/`replace`,
  vs item `rm`/`update`) so it never clashes with the item op; any residual clash
  becomes `<verb>-all`. Proven 0 collisions.
- **TYPED flags from the schema.** A write op whose `requestBody` resolves ($ref →
  component schema, `allOf` merged) to an object gets one `--flag` per property:
  `string`→`String`, `integer`→`i64`, `number`→`f64`, `boolean`→a flag,
  `enum`→a validated choice, `array`/`object`/`$ref`→a `--field` taking JSON.
  Required properties are required flags; an unset optional is OMITTED (the
  server's default stands), never sent null. The JSON body is assembled from the
  flags at their schema types. So `hanzo authz check --sub <S> --obj <O> --act <A>`
  — not `--data`. Nothing is invented — the fields are exactly the schema's
  properties. The clap id is namespaced (`field.<key>` for body, `query.<key>` for
  query) so a body key named `data`/`id` never collides with a positional or a
  control; a name that is BOTH a body property and a query param keeps ONE flag
  (body wins).
- **Query parameters are typed flags too.** An op's `parameters` array (resolving
  `$ref`) contributes an `in: query` flag per parameter, for READS and writes:
  `hanzo o11y logs --product <slug> [--since-ns N] [--limit N]`. Required query
  param → required flag (clap enforces it — the old 400 becomes a clean client
  error); the values ride the URL query, percent-encoded. `in: path` params stay
  positionals.
- **Runnable groups.** A collection GET whose verb also heads a nested group is a
  RUNNABLE GROUP, not a bare namespace: `hanzo kv list` runs `GET /v1/kv`, and
  `hanzo kv list push <key>` descends into the datatype. Only an ARITY clash
  (`/v1/mq/objects` vs `/v1/mq/objects/{store}/list`) renames the shallower to
  `<verb>-all`.
- **Fallback ladder (per op).** requestBody schema → typed flags; a write with NO
  schema (or a freeform body) → `--data '<json>'` (or `-` from stdin); a read →
  no body. There is no third tier: an unauthored PRODUCT is absent (above), not
  papered over.
- **Curation (`DENY`/`REMAP`/`ALIASES`).** The raw tree is trimmed to a friendly
  surface: `genproduct.rs::DENY` drops noise + internal planes (console, download,
  upload, files, completions, settings, search-docs, indexers, csrf, provisioning,
  do, …) and the redundant cloud PLURALS (`networks`/`clusters`/`bots` — the LOCAL
  `network`/`cluster` command and the canonical `bot` own those). `REMAP` absorbs
  `machines`/`gpus` UNDER one `hanzo compute` as sub-namespaces (a FLAT
  `compute list` needs the cloud specs reorganized under one `/v1/compute` tag —
  not faked). `product::ALIASES` mounts a friendly top-level over a generated
  coordinate — `hanzo logs` == `hanzo o11y logs`, same op, no logic dup.
- **Scope elision.** An `orgs/{org}` pair is the tenant scope: the `{org}` binds
  to the active identity's `owner` (via the seam), never a positional and never a
  flag — no `--org`, exactly as `kms`. (No authored route uses it today — the one
  that does, `kms`, is hand-written and excluded — but the mechanism is live and
  tested.) Every other param is an ADDRESS the server re-checks; addressing is
  fail-safe (a wrong one 403s you against yourself), so the CLI never sends an org.
- **Trust boundary = the point.** The generated data is DATA (product, nodes, verb,
  method, path template, params, typed fields) — NO host, NO URL, NO auth (a test
  fails the build otherwise). The ORIGIN comes from `network`, the BEARER from
  `store`; the data only shapes a call to YOUR OWN cloud with YOUR OWN token.
- **Collisions — the local command wins its bare name.** The generator omits the
  hand-written products (`kms`/`billing`/`agent`/`deploy`), and `augment` also
  skips any name the derive tree already took. `agents` (plural) owns `hanzo
  agents`. `code` is the flagship wrapper and ONLY the wrapper — `/v1/code` is not
  in the authored specs, so `hanzo code "task"` runs Claude and there are no
  `code` cloud verbs (they return when a `code` spec is authored).
- **One seam, in-module.** The authenticated `call` (origin from `network`,
  bearer from `store`, print `data`, explain a 403 via `store::refusal_hint`)
  lives in `product/mod.rs`; the seam guard `no_consumer_bypasses_the_active_identity_seam`
  lists it. `http.rs` stays the transport; `commands/api.rs` is gone.

## Billing (`src/commands/billing.rs`) — the money the identity model bills
`balance` reads `GET /v1/billing/balance`; `deposit` posts `POST /v1/billing/deposit`
(commerce's `api/billing/deposit.go`, mounted at `api.Post("/deposit", mintRequired,
Deposit)`). Both send ONLY the bearer — the org is the gateway's to derive from the
JWT `owner`, so there is no org flag and no `X-Org-Id`, and `hanzo switch` moves the
money for free. Nothing here is generated from cloud's router: cloud registers no
deposit route (it is commerce's), so this surface is hand-written against the
commerce handler, which is the source of truth for its shape.

- **Nothing is invented.** `--user` (the beneficiary subject) is REQUIRED, never
  computed: the rule is server-side `account.Payer` (`org` pool vs `org/name`
  person, off claims the CLI cannot see), so guessing it would drift from the gate
  and fund an account the meter never reads. Amounts are `--cents` — the unit the
  ledger states, so nothing is rounded and no currency exponent is assumed. Money
  POLICY is server-authoritative too (positive, `COMMERCE_DEPOSIT_MAX_CENTS`), so
  it is not mirrored here; an unset flag is OMITTED from the body so the server's
  own default is the only default. A balance that cannot be read FAILS — unknown
  is not "broke", and a zero is never rendered on the server's behalf.
- **A 403 is explained, never pre-empted.** The request always goes out: the server
  is the SOLE grantor (`middleware.PlatformOnly` → `MayMintMoney` — the internal
  service token, or `IsSuperAdmin()` ⟺ the reserved `admin` org — over the token
  IT verified). Gating on our own `owner` would invent an authz decision out of an
  unverified local decode that LABELS STORAGE ONLY, and would refuse callers the
  server would admit. Only AFTER a refusal do we read the identity, via
  `store::refusal_hint` — the ONE explainer (pure, over the active identity + the
  ones we hold), shared by every command that can meet a SuperAdmin gate rather
  than special-cased here. It names an identity we actually HOLD (`hanzo switch
  admin/z`), defers to `switch`'s own resolution when several are held, says
  `hanzo login` when none is, and stays SILENT for a SuperAdmin — whom switching
  cannot help.

## Wallet model (`src/commands/wallet.rs`) — two custodies, ZERO plaintext
- Cloud custody (`kms`/`mpc`, default when signed in): the PQ identity. Keys are
  derived + held server-side (`cloud/clients/wallets`, KMS/MPC via `POST
  /v1/wallets`) — the CLI only ever sees the address.
- Local custody (`--local`, `import`): offline secp256k1 economic key. Mnemonic
  (any word count, `tiny-bip39`) or 0x private key → `m/44'/60'/0'/0/0` →
  Keccak256 EVM address. The secret lives in the credential store (keychain or
  owner-only file, via `token::vault`), never printed. Config stores only
  metadata (address, custody, network).
- No auto-provision: a command takes the ACTIVE wallet or none. Provisioning a
  signer as a side effect of another command wrote wallets nobody asked for.

## Node / hanzo.network (`src/commands/node.rs`)
`node up` resolves an existing hanzod (`HANZO_NODE_BIN`, then `hanzod` on PATH —
we never BUILD node binaries here, CI/CD does), starts it on the active network
(env `HANZO_NETWORK*`), records its PID, and optionally spawns the cloud control
plane (`--with-cloud`). `stop` SIGTERMs that recorded PID (never a blind pkill).
`status` reports network + liveness + the active network's `/health`.

## Key files
- `src/main.rs` — clap command tree + dispatch.
- `src/config.rs` — persisted, non-secret config (auth index + network + wallet
  state). `Config::update` is the ONLY mutator: locked, re-read, atomic.
- `src/http.rs` — the ONE bearer-authenticated JSON call into cloud. Transport
  only: it knows nothing of the plane it calls, which is why the agents clients
  (`code`) and the secret store (`kms`) share it without braiding. It sends the
  bearer and nothing else — never an org header.
- `src/commands/{network,kms,wallet,node,cluster,deploy}.rs` — the concerns above.
- `src/iam/*` — IAM OIDC client + the identity store (the credential-store pattern
  wallet secrets reuse): `identity.rs` (who a token is, from its own claims),
  `token.rs` (the `Vault` seam + its `Keyring`/`FileVault` backends + the
  `vault()` resolver + per-identity entries), `store.rs` (THE one way any command
  resolves the ACTIVE identity), `login.rs` (the four verbs).
