# LLM.md — hanzo CLI

## Overview
The `hanzo` CLI: one binary to drive Hanzo's top-level concerns — network,
wallet, node/hanzo.network — plus cloud auth, cluster, deploy, and the
TS/Python SDK proxies. It is the CLI half of "it all fits together" with the
console + cloud + the fabric: ONE network model, ONE wallet model, ONE way.

## Tech Stack
- **Language**: Rust (crate `hanzo-cli`, binary `hanzo`), clap derive, tokio.
- Secrets: OS keychain via `keyring` (IAM tokens AND wallet keys) — never on disk.

## Build & Run
```bash
cargo build            # build gate
cargo test --bin hanzo # unit tests (incl. wallet derivation vectors)
cargo clippy --bin hanzo
```

## Command surface (`src/main.rs` clap tree → `src/commands/*`, `src/iam/*`)
- `login` / `whoami` / `switch` / `logout` — IAM OIDC PKCE S256 + the identity
  model (`src/iam/*`, HIP-0111; below).
- `network list|current|use <name>|add <name> …` — the network model (below).
- `wallet show|address|create|import <secret>|use <addr>|list` — wallet (below).
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
- **Storage reuses the wallet law**: secret in the OS keychain, metadata in
  config. Keychain entry = `{brand}/{owner}/{name}` → `TokenSet`, so nothing
  clobbers (`token.rs`). `config.toml` `[auth]` is the NON-SECRET index —
  the identity list + the active identity per brand — and never holds token
  material. The index exists because the keychain has no portable enumeration
  API; it is what makes listing work offline.
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
  verb); `switch [owner/name|owner]` selects, bare toggles when exactly two are
  held and lists when more; `logout [IDENTITY] [--all]` removes one or all.
  `login --token -` reads stdin so a token never lands in argv or shell history,
  and requires an identity-bearing JWT — an `hk-` key has no derivable principal
  and is refused.
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
  discovering it as a 403. NO `resumedFrom` pointer is written across an org
  boundary: the new org's record must never reference a session it cannot
  resolve, so that lineage stays in the local store — the only place it is true.
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

## Wallet model (`src/commands/wallet.rs`) — two custodies, ZERO plaintext
- Cloud custody (`kms`/`mpc`, default when signed in): the PQ identity. Keys are
  derived + held server-side (`cloud/clients/wallets`, KMS/MPC via `POST
  /v1/wallets`) — the CLI only ever sees the address.
- Local custody (`--local`, `import`): offline secp256k1 economic key. Mnemonic
  (any word count, `tiny-bip39`) or 0x private key → `m/44'/60'/0'/0/0` →
  Keccak256 EVM address. The secret lives in the OS keychain, never on disk,
  never printed. Config stores only metadata (address, custody, network).
- Auto-provision: `wallet::ensure` gives you a wallet when a command needs one.

## Node / hanzo.network (`src/commands/node.rs`)
`node up` resolves an existing hanzod (`HANZO_NODE_BIN`, then `hanzod` on PATH —
we never BUILD node binaries here, CI/CD does), starts it on the active network
(env `HANZO_NETWORK*`), records its PID, and optionally spawns the cloud control
plane (`--with-cloud`). `stop` SIGTERMs that recorded PID (never a blind pkill).
`status` reports network + liveness + the active network's `/health`.

## Key files
- `src/main.rs` — clap command tree + dispatch.
- `src/config.rs` — persisted, non-secret config (network + wallet state, `save`).
- `src/commands/{network,wallet,node,cluster,deploy}.rs` — the concerns above.
- `src/iam/*` — IAM OIDC client + the identity store (the keychain pattern wallet
  secrets reuse): `identity.rs` (who a token is, from its own claims), `token.rs`
  (per-identity keychain entries + the `Vault` test seam), `store.rs` (THE one
  way any command resolves the ACTIVE identity), `login.rs` (the four verbs).
