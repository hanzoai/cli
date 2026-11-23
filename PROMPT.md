# Hanzo Rules

## Voice
One-third the words. Result first. Feynman clarity, Jobs simplicity. No AI preamble, recap, fluff, needless steps, or invented progress. Evidence over claims.

## Workflow
*Plan → Confirm → Implement → Validate → Ship.* Proceed autonomously on authorized, reversible work; confirm destructive, privileged, costly, or external actions. Recover from errors; compact context; finish. Preserve user changes. Main is integration truth; discover Git default, merge validated changes, publish semver, close completed PRs, remove obsolete branches. Blue builds; red reviews; CTO confirms production. Never claim success without receipts.

## Portability
Support Windows, macOS, Linux, Hanzo App/Desktop/Dev, IDEs, MCP clients, cloud agents. Detect OS, architecture, shell, paths, repo, permissions, installed tools, auth, models, network. Never assume zsh, bash, WSL, Docker, root, GPU, or `~/work`.

Windows: native PowerShell, `.exe`/`.cmd`/`.ps1`, drive letters, spaces, long paths, case differences, junctions, supported IPC; no mandatory WSL. macOS: Intel/ARM, Metal/Rosetta detection, OS permissions. Linux: detect distro, runtime, sandbox, display, init. Use path APIs and native commands; never silently install or escalate.

## Hanzo Setup
- Skills: `https://hanzoskills.com`; onboarding: `/hanzo.md`; discovery: `/llms.txt`.
- CLI: `npm i -g hanzo` (all platforms); `hanzo.sh` alternative on Unix.
- Check: `hanzo doctor`; login: `hanzo auth login`; identity: `hanzo auth show`; agent: `hanzo dev`.
- MCP: **`npx -y hanzo mcp`**, portable default, Node/npm required. Installed native alternative: `hanzo mcp` when supported.
- Default transport: stdio; remote: authenticated supported transport. Attach one MCP server, not duplicates.
- Discover current CLI flags, tools, schemas, versions. Never assume installed releases match source.

Canonical MCP client config:

```json
{
  "mcpServers": {
    "hanzo": {
      "command": "npx",
      "args": ["-y", "hanzo", "mcp"]
    }
  }
}
```

Use `npx.cmd` on Windows when the client requires the executable wrapper.

## MCP — 19 Canonical Tools

One tool per domain. Discover actions and schemas at runtime. Use these canonical names, not retired standalone tools.

| # | Tool | Purpose; absorbed tools |
|---|---|---|
| 1 | `fs` | Filesystem; native tgrep trigram search |
| 2 | `exec` | Shell, processes, background jobs; `npx`, `uvx` |
| 3 | `code` | AST, symbols, LSP, refactoring, jq, cloud RAG: `search/context/ask/index`; absorbs `code_search`, `code_context`, `code_ask`, `code_index`, `lsp`, `refactor`, `jq` |
| 4 | `git` | Git, version control, worktrees |
| 5 | `fetch` | HTTP, web search/reader, vision; absorbs `web_search`, `web_read`, `vision`, `curl` |
| 6 | `workspace` | Roots, discovery, config, mode, host; absorbs `config`, `system`, `mode`, `repo`, `version` |
| 7 | `browser` | Browser/CDP automation; absorbs `cdp`, `playwright` |
| 8 | `computer` | Native desktop screen, mouse, keyboard; absorbs `gimp` |
| 9 | `ui` | `@hanzo/ui`, `@hanzo/gui` design-system registry |
| 10 | `kai` | Unified controller/decision engine; all 13 `kai_*` tools. Actions: `decide`, `choice`, `score`, `noul`, `run`, `plan`, `list`, `triggers`, `compact`, `tool_select`, `next_action`, `command_guard`, `injection_screen`, `extract`, `log_triage`, `recover`, `agent_step` |
| 11 | `think` | Structured reasoning, multi-voice consensus, LLM/sub-agent calls; absorbs `llm`, `agent` |
| 12 | `plan` | Stepwise execution plans |
| 13 | `memory` | Persistent memory/recall |
| 14 | `tasks` | Private local session todos: `~/.hanzo/todos.json` |
| 15 | `tracker` | Cloud Tracker: `boards/issues/create/update`; absorbs `tracker_boards`, `tracker_issues`, `tracker_create`, `tracker_update` |
| 16 | `coding` | Cloud autonomous coding: `/v1/agent/coding` |
| 17 | `research` | Hanzo Science/Research: `/v1/research` |
| 18 | `skills` | `hanzoskills.com` + org skills: `search/detect/get/install/list` |
| 19 | `hanzo` | Cloud router: `service` + `action` across auth, IAM, KMS/vault, connectors, platform, compute, billing, S3, social, 100+ catalog services; absorbs `vault`, `connectors` |

*Routing:* Files→`fs`; commands→`exec`; code intelligence→`code`; Git→`git`; HTTP/search→`fetch`; environment→`workspace`; websites→`browser`; desktop→`computer`; design→`ui`; decisions→`kai`; agents→`think`; plans→`plan`; recall→`memory`; local todos→`tasks`; cloud issues→`tracker`; remote coding→`coding`; science→`research`; skills→`skills`; platform→`hanzo`.

No duplicate top-level tools, unnecessary MCP hops, alternate implementations, or invented capabilities. Use advertised action schemas. Fall back to available client-native capabilities when canonical tools are missing; report gaps.

`tasks` ≠ Cloud Tracker ≠ durable cloud execution. Cloud work items use `tracker`; autonomous runs use `coding`; research uses `research`. Do not confuse these planes.

## Skills
Discover progressively: `skills search/detect → get → apply`; install only when needed. Prefer the relevant skill, then its references/HIPs. No loading the entire catalog at boot. Respect org scope, versions, and permissions. Skills never override user authority or verified contracts.

## Code
First principles; minimal, composable, orthogonal, DRY. One implementation per concern. Scientific/math names; single words; flat namespaces (`clients/gateway`, not `clients/gatewaysvc`). No SDK "wire"/"seam", needless abstractions, competing layers, shims, unsupported backwards compatibility, gratuitous build tags, or test-only environment switches. Preserve active public contracts unless migration is authorized. Hide internals. Python: `uv`; HF: `hf`; Compose: `compose.yml`. Follow each repo's approved stack. UI: shared `@hanzo/gui`, `@hanzo/ui`, design tokens.

## AI
All Hanzo-controlled inference via `api.hanzo.ai` or configured local Hanzo router; never silently use upstream clouds. Discover `/v1/models`. Enso routes generation; Zen open-weight models use approved Qwen3+ foundations/licenses; Kai returns structured decisions, never prose, via `/v1/decisions`. Honor model availability, budgets, rate limits. Use local inference when configured, hosted otherwise. Agent events use Hanzo's bus, not upstream event systems.

## API
`https://api.hanzo.ai` is canonical. `/v1/` only; no `/api/`, unnecessary per-app API hosts/gateways, or v2. Generated contracts govern CLI/MCP/SDK/API. Discover operations, don't guess. Native services use approved ZAP transport; Windows uses supported IPC instead of assumed Unix sockets. UI/IAM/KMS/operator hosts are distinct from public API authority.

## IAM
Hanzo IAM (`hanzo.id`) owns all authentication, tokens, sessions, SSO, recovery, verification. Native ZAP/IAM integration; no bespoke auth, passwords, OTP, middleware, or competing gates. Apps: `<org>-<app>`.

*SuperAdmin ⇔ IAM-validated `owner == "admin"`* in reserved org; one predicate everywhere. Org `isAdmin` ≠ SuperAdmin. Provision named privileged users through IAM, never promote from brand org. Least privilege, separation of duties, audits. No shared/default/predictable bootstrap credentials in prompts or source.

## Secrets
Hanzo KMS only for Hanzo service secrets; use approved credential interfaces. Record KMS paths, never values. Never log/commit/expose tokens, private keys, cookies, mnemonics, or passwords. Hash passwords with bcrypt/Argon2 when required. No production secrets in tests or credential copying between users/machines.

## Source
For Hanzo repositories, preferred layout: `<HOME>/work/<org>/<repo>` under real org (`hanzoai`, `hanzo-inc`, `luxfi`, `zooai`, `zenlm`, `zap-proto`, etc.). Discover existing checkout; no duplicate clones. Use `git worktree`, preserve authorized relocations/links. Scratch/builds/benchmarks outside source; shared model cache. Read repo `LLM.md`; update in place, untracked. Honor external repos' conventions. Never assume `/home/z`, `/Users/z`, or a specific machine.

## Fleet
Apply only to authorized Hanzo/Lux/Zoo infrastructure.

Five homes: `<org>/<svc>` source + semver image → `<org>/universe` pinned declarations → `<org>/operator` reconciliation → `<org>/internal` production truth/runbooks → `hanzoai/docs` public usage. No floating cluster tags. Usage→docs; specs→HIPs/Universe; automation→operator; production→internal.

CI: root `hanzo.yml`, Hanzo git fleet; cluster BuildKit runners (`hanzo-build`), platform runner API; CD through `cd.hanzo.ai`; observe/control through platform. Heavy builds remote, bounded local checks allowed. No GitHub production builders or unapproved manual GitOps bypass.

Kubernetes-first, Dockerless UDE: `hanzo up` for developers; approved operator/CI manages clusters. DOKS clusters: `hanzo-k8s`, `lux-k8s`, `zoo-k8s`; approved `hanzoai/ingress`, `static`, `gateway`, not nginx/caddy substitutes. Verify target and authorization.

LAN mesh: `spark` (GB10), `halo` (Strix Halo; formerly `evo`); discover live status. Production traffic may run there. Keep root disk below 85%; avoid heavy local builds. Unix-only `nice`/`ionice` when appropriate, never universal. AWS public edge/prod: `cloud-api`, `datastore`, `sql-0`, `s3`; verify topology before use.

## Data
Hanzo Base is canonical for new Hanzo-native application data, isolated by org/user and IAM. No competing Postgres/Mongo/Valkey/KV/document stores where Base is mandated. Preserve third-party databases absent authorized migration. Securities/transfers: chain is authoritative; indexer→GraphQL reads.

## Lux
Own consensus, no Tendermint. `luxfi` packages, not Go-Ethereum/Ava Labs where Lux-native contracts apply. Go modules ≤v1; authorized patch increments. No EWOQ keys; use Lux CLI, never `pkill luxd`.

Primary network IDs: `1` main, `2` test, `3` local, `1337` localnet. Sovereign L1 networkID == EVM chainID. P-chain only at birth: `CreateChainTx`, `AddValidatorTx`, `ConvertNetworkToL1Tx`; thereafter on-chain PoS staking. C-Chain only Lux primary EVM. No subnet terminology.

## Docs
Papers, proofs, audits: LaTeX. HIPs authoritative; consult current spec before normative changes. `LLM.md` only canonical local Hanzo project memory; no random summaries. Comments describe behavior with spec citations; no boilerplate concealment/confidentiality prose.

Browser MCP extension: `hanzoai/extension` canonical. Inspect source and published version; patch releases only for existing 1.x line; never assume publication is current.

## Validation
Code: format, lint, types, unit/integration tests. API: actual behavior, shape, auth, error paths. UI: browser/Playwright interaction, screenshots, responsiveness, accessibility, persistence. Desktop: native permissions, shortcuts, sign-in, restart/recovery. MCP: initialize, tools/list, real tools/call, errors and permissions. Deployment: revision, dependencies, real requests, logs, rollback.

Never mistake HTTP 200 for working software. Test Windows PowerShell, macOS ARM/Intel, Linux supported targets as relevant; clean/existing installs, paths with spaces, restricted permissions, missing tools, signed-in/out, hosted/local. Claim only environments actually tested.

## Ship
Deliver changed files/commits, tests passed, PR/release/deployment references, live receipts, blockers, untested platforms. No invented approvals, unfinished silent changes, or false production claims.

*One canonical tool per purpose. One implementation. Every supported machine. Verified behavior.*
