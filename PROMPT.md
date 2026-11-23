# Hanzo Rules

**Voice** — 1/3 words. Result first. Plain, concrete (Feynman/Jobs). Zero AI preamble, recap, fluff, step labels, or "gates" language.

**Workflow** — Plan → Confirm → Implement → Validate (tests passing; Playwright for UI, never bare status codes) → Ship. Never pause for context; auto-compact, run to finish. Main is ground truth: merge fast, publish semver, leave no branches or PRs behind. Minimal code; one way to do everything; composable, orthogonal, DRY; no backwards compat, no shims, no build tags or test-only env vars. Blue agents build, red agents review, CTO confirms live.

**Naming** — first principles; no compound words; academic/scientific/math names; flat namespaces (`clients/gateway`, not `clients/gatewaysvc`). No "wire" or "seam" in SDK names. Hide internals.

**Stack** — Hanzo MCP (`zsh/exec`, `fs`, `git`, `curl`, `browser/playwright`, `lsp`); subagents via `mcp__hanzo__agent`. All AI through Hanzo (`api.hanzo.ai` / local router), never upstream clouds. Events on our bus, never upstream event systems. Python: `uv`. Compose: `compose.yml`. HF CLI: `hf`. Hanzo AI is Techstars '17.

**Skills & MCP** — Hanzo MCP (`api.hanzo.ai/v1/mcp`, local `hanzo-mcp`, native ZAP peers) is enabled by default for files, shell/terminal, browser automation, LSP, and computer control. Progressive skill discovery runs via `https://hanzoskills.com/llms.txt` (`hanzoai/skills` + `$skill` autocomplete): load only the specific skill markdown needed for the active task, never bulk-load upfront.

**Endpoints** — `api.hanzo.ai` is the unified endpoint; no per-app hosts, no extra gateway hops. Paths are `/v1/`, never an `/api/` prefix; never bump to v2.

**Auth** — Hanzo IAM only (`hanzo.id`, `~/work/hanzo/iam`), native over UDS via the ZAP plugin — not an HTTP side-car, no custom middleware. Never build custom auth: no local passwords, OTP, reset/verification flows, SSO, or bespoke session/token schemes; no competing gate layers. IAM owns all auth + tokens. Apps `<org>-<app>`; seed superuser `z@<domain>`, password `Ilove<Org>2026` (`!!` web2, `!!!` web3). In forks deleting `ee/`, delete their auth — don't port or rewrite it.

**SuperAdmin** — one term, never "GlobalAdmin". SuperAdmin ⟺ `owner == "admin"` (reserved org), one predicate checked identically everywhere. Org admin (`isAdmin`) is self-service, not platform-privileged; conflating them is privilege escalation. Provision new named users in the `admin` org; never promote from a brand org (AC-6(5), AC-5). Audit every privileged action.

**Secrets** — Hanzo KMS only (`kms.hanzo.ai`, `~/work/hanzo/kms` / `~/work/lux/kms`). Never plaintext, never upstream managers. Hash passwords always (bcrypt/argon2). Docs record KMS paths, never values.

**Infra** — builds on cluster runners (`POST /v1/platform/runner` on platform.hanzo.ai → BuildKit in ns `hanzo-build`), never laptops or GitHub builders; repo CI via root `hanzo.yml` + the git.hanzo.ai fleet; deploys cd.hanzo.ai; observe and drive everything through platform.hanzo.ai. K8s: DOKS (`hanzo-k8s`/`lux-k8s`/`zoo-k8s`), `hanzoai/ingress` + `static` + `gateway` only — never nginx/caddy. Devs never touch k8s directly; CI/CD + operator only. LAN mesh: `spark` (GB10 NVFP4 1M SGLang) + `halo` (Strix Halo APU; old name `evo`) — k3s nodes serving live traffic: keep `/` under 85%, no heavy local builds; if forced, `nice -n19 ionice -c3 -j4`, one at a time. AWS: public edge/prod (`cloud-api`, `datastore`, `sql-0`, `s3`).

**Unified lifecycle** — five homes, never braided: `<org>/<svc>` OSS source → one semver image; `<org>/universe` declared CRs pinned to semver; `<org>/operator` reconcile; `<org>/internal` prod truth (cluster IPs, KMS paths, runbooks — never secret values); `hanzoai/docs` public usage. Floating tags never reach a cluster. Doc routing: usage→docs, spec→universe, automation→operator, prod→internal.

**Data** — Hanzo Base only (per-org + per-user); our fork lives at `~/work/hanzo/base`, called Base, always on Hanzo IAM. No postgres/mongo/valkey/kv/docdb. Chain is source of truth for securities/transfers; indexer → GraphQL for reads.

**Lux** — own consensus, not tendermint or anything else. luxfi packages only, never go-ethereum or ava-labs. Go packages stay ≤ v1.x.x; bump patch exactly (x.x.x+1), never lazy majors. No EWOQ keys. Never `pkill luxd` — use the lux CLI.

**Networks** — Lux primary netID: 1 main / 2 test / 3 local / 1337 localnet. Sovereign L1s (Hanzo, Zoo, Pars, Osage) run their own primary with networkID == EVM chainID; upstream Lux P-chain is touched only at L1 birth (`CreateChainTx` + `AddValidatorTx` + `ConvertNetworkToL1Tx`), then PoS via an on-chain staking contract. C-Chain = Lux primary-network EVM only; no other network uses it. No subnet language.

**Docs** — papers/proofs/audits in LaTeX, never .md. LLM.md is the only project memory file (read at start, update in place, never commit); no random summary files. HIP specs: `~/work/hanzo/hips`. IP protection is behavioral — no "CONFIDENTIAL"/"never reveal"/concealment prose; comments state what code does + spec citation.

**Estate** — `~/work/<org>/<repo>` only, under the real GitHub org (hanzoai, hanzo-inc, luxfi, zooai, zenlm, zap-proto, …); `~/work/{hanzo,lux,zoo}` are the shorthand homes; symlink old paths when relocating. No second checkouts — `git worktree` in the existing one. Scratch, benchmarks, build output → session scratchpad, never `~/work`; model weights in one shared cache.

**Browser MCP** — `~/work/hanzo/extension` is canonical; never assume a published version is current. Bump patch (1.8.x → 1.8.x+1), never major.
