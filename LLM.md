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

`hanzo version` now makes that skew LOUD (`commands/version.rs`), in BOTH
directions, and it resolves PATH exactly the way a delegating caller would —
first match wins, which is the precedence that let a stale build hide:
- a stale delegate BEHIND us — `stale delegate: <path> is vX — this is vY`.
  Same file (hardlink or symlink) or same version is silent.
- a different file AHEAD of us holding the name — `shadowed name: 'hanzo' on
  PATH is <path> — this is <us>`. This is the one that actually bit: a Go
  control binary at `/usr/local/bin/hanzo` and this build at
  `~/.local/bin/hanzo`, each answering a different `hanzo status`.

It never repairs and never refuses; it only stops the failure from being
invisible.

**`--version`, `-V` and `version` are ONE function.** `#[command(version)]` let
clap intercept `--version`, print its own line and exit — so `commands::version::run`
(the only thing that carries the skew report) never ran for the spelling people
actually type, and the two spellings printed different text. `main` now sets
`disable_version_flag`, declares `-V`/`--version` as its own flag, and routes it
to that one function before any other dispatch. The answer is ONE line on
STDOUT — `hanzo <semver>` — because that is the shape the Go binary's delegate
parser reads (first line, last token, optional `v` stripped) and the shape every
other `--version` prints. A skew report is a WARNING, not the answer, so it goes
to STDERR and a caller piping the version gets exactly one clean line.

`install.sh` does exactly that: it installs `hanzo`, symlinks `hanzo-node` to
the same file (a copy where links are unavailable), and warns when another
`hanzo` earlier on PATH would run instead of the build it just placed — the
precedence that let a stale twin hide.

The remaining gaps are elsewhere:
- PyPI carries `hanzo`, `hanzo-cli` and `hanzo-node` as SEPARATE projects that
  are not this crate. `curl hanzo.sh | sh` used to install one of them at the
  name `hanzo`, into `~/.local/bin`, which outranks `/usr/local/bin` on a normal
  PATH. It now calls THIS installer instead, so there is one download path and
  one asset-naming rule, not two.
- releases are what installers actually resolve, and they are cut by the
  `Release Matrix` workflow. A version bump that never becomes a release ships
  nothing — main can be current while every install path still serves an old
  build. That is not hypothetical: v1.9.13, v1.9.15, v1.9.16 and v1.9.17 were
  all tagged while users were still resolving v1.9.12, so `hanzo code` was
  unreachable for everyone who had not built from source.

**CI runs on the FORGE. This is the whole story, and both halves bit us.**
- `runs-on: hanzo-build-linux-amd64` is a **git.hanzo.ai** label, advertised by
  the `git-runner-0..9` pool (28 labels, every one mapping to
  `docker://catthehacker/ubuntu:act-24.04`). **github.com has zero runners** at
  org or repo level — the ARC scale set that once served this label is gone,
  down to the `autoscalingrunnersets` CRD.
- The forge reads BOTH `.hanzo/workflows` and `.github/workflows`; GitHub reads
  only `.github/workflows`. So a pipeline left under `.github/` runs **twice** —
  once on the forge, and once on GitHub where nothing can pick it up and the job
  queues in silence (v1.9.13's GitHub copy sat 17 hours). Everything CI lives in
  `.hanzo/workflows/` so there is exactly one build per tag.
- **`github.token` is a FORGE token.** Publishing a GitHub Release with it fails,
  and fails LAST — after checkout, the tag check, Zig, cargo-zigbuild, the macOS
  SDK, the test gate and all five cross-builds have passed. The only symptom is
  `could not create or find release <tag>`. Use `secrets.GH_PAT` (org secret at
  git.hanzo.ai/hanzoai; repo-level secrets are empty and org secrets inherit).
- The split to keep straight: **BUILD is the forge's, DISTRIBUTION is GitHub's.**
  hanzo.sh resolves `api.github.com/repos/hanzoai/cli/releases/latest`, so assets
  must land on GitHub Releases no matter where they were compiled. That crossing
  is the one place this pipeline needs a credential.
- **Never `runs-on: ubuntu-latest`.** That label was deliberately removed from the
  pool after an upstream workflow wedged all ten runners; a job requesting it
  queues for its full 24h timeout and reports no error at all.
- All five platform assets come from that ONE linux/amd64 docker pool.
  `cargo-zigbuild` makes Zig the cross-linker, so no macOS or Windows runner
  exists or is needed — a docker-only, linux-only pool ships linux-{amd64,arm64},
  windows-amd64 and darwin-{amd64,arm64}. Anyone proposing per-arch runners to
  "fix" the release has misread the problem.

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
- the whole cloud, one screen: `hanzo status` (see below)

**`hanzo status` is the fleet view, and it is COMPOSED, never a new API**
(`commands/status.rs`). It reads three routes cloud already serves, concurrently,
through the ONE authenticated seam (`product::Seam` — origin from `network`,
bearer from the active identity, no org header): `GET /v1/k8s/clusters`
(`clusters`), `GET /v1/deploy/applications` (argocd `items`), `GET
/v1/fleet/workers` (`workers`). There is no fourth wire and no CLI-only side
channel. Two laws it exists to enforce:
- **Most important first.** Anything unhealthy leads the page — an application
  that is not `Healthy`+`Synced`, a cluster not running, a node not online. The
  rest is grouped and COUNTED, so 336 healthy applications are three lines.
- **A surface that did not answer is UNAVAILABLE, never zero.** `/v1/deploy/gitops`
  answers `403 not authorized for this deploy console` to a non-console identity,
  and `/v1/k8s/clusters` legitimately answers `{"clusters":[]}` — those are
  DIFFERENT facts. A refusal prints its status and the server's own words
  (collapsed to one clipped line, so an ingress HTML page cannot eat the page it
  was meant to explain); an empty 200 prints "none reported"; an answer with no
  list prints "unreadable". "all clear" is claimed only when every surface was
  actually read. One failing surface never fails the command — failing to read
  ALL of them does (non-zero exit).

Before this existed, `status` was not a command at all: it fell through to the
flattened coding-session positional, so typing `hanzo status` started a headless
coding agent on the task "status".

**Where the cloud surface comes from** — `hanzo <product> …` is DERIVED, never
hand-maintained, and the client links no cloud code: it reads a spec, which is what it
does over the wire anyway.

```
hanzoai/cloud@<tag>        ─┐  THE SOURCE: it enumerates, and carries the shapes
  openapi.yaml              ├─ genspec ─→ spec/cloud.json ─→ genproduct ─→ generated.rs
hanzoai/openapi hanzo.yaml ─┘  the SUPPLEMENT: shape it has not typed, and only
                               where the document is not the authority
```

Both halves are PINNED, in `.spec-lock`, and only `make spec` moves either. The
right-hand input used to be `api.hanzo.ai/v1/openapi.json`, a host — which cannot
say which deploy it was, so a capture taken from it could never be re-derived and
never be checked.

- `make spec` / `make spec-check` — the refresh seam and its gate, both through
  `scripts/generate.sh` (the same call site hanzoai/ci's `client:` lane uses).
  Underneath: `genspec` reads the document from `HANZO_REGISTRY` (JSON or YAML)
  and the shapes from `--openapi <hanzoai/openapi checkout>`, and stamps
  `hanzoai/cloud@<ref> sha256:<digest>` into the spec's provenance.
  `--check` re-runs the whole derivation and refuses a delta without writing.
- `make verify` — THE DRIFT GATE (`src/bin/driftgate.rs`). The other question: is
  the document still TRUE of the running server, in BOTH directions? On every push
  and PR (`hanzo.yml` `test:`), NIGHTLY (`.hanzo/workflows/drift.yml`) and before a
  release mints anything. See "The drift gate" below.
- `cargo run --bin genproduct` — offline, deterministic: `spec/cloud.json` → the clap
  tree. `--check` compares instead of writing; `tests/spec_drift.rs` runs it, so
  `cargo test` pins the tree to its spec.
- **The document is the authority at product granularity.** Cloud's route table is the
  weave of what each app binary emits from its own router, so a product it serves ANY
  route under is a product it is complete for — an authored operation missing from the
  table is dropped. A product the table never mentions (the inference surface:
  `/v1/models`, `/v1/chat/completions`) is answered at the edge, not by that router, so
  nothing is refuted. Multi-segment path params come from the same reading: a fiber `*`
  arrives as `{wildcardN}`.
- What still needs a HAND decision: `src/curation.rs` — ONE table naming every
  product the tree does not surface at its own bare name (`Instead::Nothing`), the
  ones another command answers to (`Instead::Claimed`), and the ones absorbed
  elsewhere (`Instead::Under("compute")` for machines+gpus) — plus
  `METHOD_PRIORITY`, `VERBS` and the path→verb fold in `genproduct.rs`. Those are
  POLICY — none of them may encode "the server 404s this", and that is now
  ENFORCED, not promised (the curation law, below).
- **The reason is DATA, and the gate falsifies it.** `DENY`/`REMAP` were two tables
  with their reasons in comments, and a comment cannot be checked. Each entry now
  carries a `why` sentence and, where it claims another spelling reaches the
  surface, that spelling — which `make verify` RUNS. `genproduct` applies the table;
  `driftgate` excuses gaps with it and counts how many times it had to. One list,
  two readers: a product dropped there and a product excused here can never be two
  different lists.
- **A name may only be reserved by a command that EXISTS.** `EXCLUDE` used to sit
  beside `DENY` holding `billing`/`agent`/`deploy` under "local commands own these
  bare names" — a third statement of one fact (every entry was already in `DENY`,
  and `product::augment` reads the real answer off the parser), and two thirds
  false: `agent` and `deploy` had been DELETED as top-level commands, so the
  reservation was held for nobody and the 21 documented `/v1/deploy/*` routes
  reached no one. `EXCLUDE` is gone; `product::mounted(&Cli::command())` is the ONE
  filter, shared by `augment` (which mounts) and `catalog` (which advertises), and
  `a_reservation_must_name_a_command_that_exists` is the gate.
- **A shadow is a stated gap, never a silent one.** A local command that owns a bare
  name hides that product's operations: `hanzo billing` reaches 2 of the 22
  documented `/v1/billing/*` routes, `hanzo code` 0 of 6 `/v1/code/*`, `hanzo
  engine` 0 of 4 `/v1/engine/*`. Closing a shadow means the local command ABSORBS
  the operations — a UX decision per command, not a list edit. `agent` is denied for
  a different reason and says so: `/v1/agent` (one tool-calling round) and
  `/v1/agents` (the registry) are two products on one noun, and the fix is a route
  move in hanzoai/agent.
- A capability missing from the CLI is missing from a document: author it in
  hanzoai/openapi, or serve it in hanzoai/cloud. There is no third place. `/v1/cd`
  is the worked example: it answers 200 with `<title>Hanzo CD</title>` and
  contributes ZERO keys to the route table, because it is the Hanzo CD **console**,
  not an API — its own noscript line says "Hanzo CD can be used with the Hanzo CD
  CLI". Its API is `/v1/deploy/*`, and `hanzo deploy` is that CLI. `cd` is not a
  product and must not become a command name — it is the shell's own builtin.

**The lineage contract — which half is authoritative about what.** The two inputs
are not redundant readings of one truth; each owns a different question, and
neither can answer the other's.

- **The DOCUMENT decides EXISTENCE, and carries the SHAPE.** `genspec` ITERATES
  cloud's `openapi.yaml`. Every operation it carries is an operation the CLI has,
  with that document's own prose and its own request body and parameters — zip
  reflects them off the live Go type, so they ARE the shape, not an approximation
  of it. Before this inverted (1.9.20) the master iterated and the document could
  only delete: **0 of 1899 operations came from the document**, 244 took a
  hand-written request body while cloud published the reflected one, and 66 more
  carried no body at all because the master happened to be silent about an
  operation the document types. A refuter can only answer about operations someone
  thought to author; that is the expensive direction, because it fails silently —
  a working server and a CLI that has never heard of it.
- **The MASTER supplements.** It is joined at the document's own addresses to fill
  in a body or query parameter the document has not typed yet, and it is read on
  its own ONLY where the document is not the authority: a product the table never
  mentions, or a route it answers through a `/v1/<product>/*` catch-all. A door
  says the request REACHES the mounted service and says nothing about what the
  service serves there, so the master may enumerate behind it — that is where the
  iam and bot subtrees come from. When those services publish their own documents
  the door becomes a list and the master's job there ends. A shrinking dependency,
  not a permanent one: **1384 of 1895 operations (73%) are the document's today**,
  and `/v1/admin/plugins` is what the end state looks like — cloud emits the full
  schema and nothing else is consulted.
- **THE CURATION LAW.** A `CURATED` entry may name ONLY a product the document
  carries; `genproduct` refuses to generate otherwise, naming the stale entries. A
  name no document mentions asserts one thing — that the server does not serve it —
  and that is `genspec`'s answer by construction, not a list in the client. Under
  that law 1.9.20 struck nine names no document had ever carried (`console`
  `search-docs` `index-docs` `chat-docs` `embed-status` `account-bridge`
  `agent-bindings` `provisioning` `do`), two "redundant plurals" the document shows
  to be separate surfaces (`/v1/bots` is bot RUNS while `/v1/bot` is bot nodes plus
  a door; `/v1/networks` is the org's Zero Trust overlay while the local `network`
  SELECTS one), and five relay-door products called enumeration artifacts, each
  with its own prose and 1–11 served operations (`files` `upload` `download`
  `indexers` `settings`). What survives states a fact about what a COMMAND LINE is:
  a CLI is not a browser (`csrf`), the document that decides the commands is not
  one of them (`openapi.json`), `completions` names shell completion here (the
  operation is `hanzo chat completions`), a local command owns its bare name
  (`code` `help` `billing` `engine`), and one noun cannot name two products
  (`agent` vs `agents`).
- **Refresh: `make spec`. Gate: `make spec-check`.** Both go through
  `scripts/generate.sh`, which is also what hanzoai/ci's `client:` lane calls, so
  a maintainer, the push gate and the release all run ONE derivation. `--check`
  only decides write-vs-compare; a gate that runs a different generation than the
  writer tests something nobody ships.

  ```
  make spec         # regenerate onto the document .spec-lock names
  make spec-check   # refuse a capture that is no longer its projection
  make verify       # refuse a tree the LIVE server contradicts, either direction
  ```

  **THE CAPTURE NAMES A RELEASE, and `.spec-lock` is where it says so.** Four
  lines the release writes (`repo`, `path`, `ref`, `sha256` of
  hanzoai/cloud's `openapi.yaml` at the tag it deployed) plus one
  `generate.sh` writes (`master`, the hanzoai/openapi commit it read shapes
  from). Every input pinned; two runs of one commit cannot disagree.

  This replaced "the spec must name the wire", which was half right and caught
  nothing. A URL names a HOST, and a host cannot say which deploy it was — so a
  capture built from the wire keeps claiming the wire forever while the router
  moves underneath it, and every stale spec ever committed satisfied that
  assertion. `spec/cloud.json` now carries `hanzoai/cloud@<tag> sha256:<digest>`,
  computed over the bytes actually read, and `spec_drift.rs` refuses any
  disagreement between the spec, the lock, and a release-shaped tag.

  **THE SEAM IS WIRED — that is the whole point.** `genspec` used to be reachable
  only by a maintainer typing it: `grep -rn genspec` over this repo's CI returned
  nothing, and there was no `hanzo.yml` and no `Makefile` at all. Now
  `hanzo.yml`'s `spec-drift-check` runs it on every push, and hanzoai/cloud's
  release sends `repository_dispatch: spec-update` carrying `(version, sha,
  spec_sha256)` — the lane fetches that exact document, regenerates, compiles,
  and cuts a CLI patch. The capture cannot go stale silently because a cloud
  release is what moves it.

  Measured when it was wired: the committed capture was 7 operations short and 8
  wrong against a document cloud had already committed — the `/v1/billing/*`
  compound-word renames, published-dead and served-undocumented.

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

  **And provenance is not freshness — that is what `make verify` is for.**
  Recording the source says WHERE a spec came from; it says nothing about whether
  it is still true. One spec shipped 129 operations across 9 products (commerce
  109, admin 7, billing 5, …) addressing routes api.hanzo.ai does not serve, and
  its provenance was impeccable. `genspec --verify` used to re-ask the table here
  and was DELETED, not kept beside a stronger gate asking the same question: it
  could only refute inside a product the table OWNS, which leaves roughly a third
  of the surface un-refutable, and it never asked the other direction at all.

  **THE DRIFT GATE — `make verify` (`src/bin/driftgate.rs`).** The shipped surface
  and the live server, held against each other in BOTH directions, with the host as
  the arbiter wherever the table cannot answer. `make spec-check` proves the capture
  still equals its inputs; only this proves the inputs are still true.

  - **404 IS NOT 403, and one 404 is not a 404.** `401`/`403` mean the route is
    there and wants a caller; `404` means nothing is at that address; `405` means
    the path is routed and the verb is not. Conflating them is how a hand analysis
    of this surface reported three production breaks that were not breaks. And a
    single 404 is not evidence either — fourteen `/v1/pricing` paths answered 404
    to one concurrent sweep and 200 to every serial re-ask a minute later, so a 404
    is confirmed three times, serially, before it counts. A 404 that does not
    repeat is FLAPPING: present, printed, never drift.
  - **Only a `GET` on a LITERAL path can be asked of a host.** A `{param}` makes
    404 mean "no route" or "no such id"; and cloud's router answers 404, not 405,
    to a verb it lacks at a path it has (`POST /v1/admin/credits` is in the live
    table and a GET of it 404s). Everything else is UNDECIDABLE and is COUNTED — a
    stated gap beats a guessed answer. The probe is only ever a GET: a gate that
    DELETEs to find out whether something is there is not a gate.
  - **A `*` door is not an answer.** The table names the route → served (asked
    anyway where it can be: a route can be registered with a dead mount behind it).
    It owns the product and names no such route → REFUTED, decided without asking.
    It offers only `/v1/iam/*`, or is silent → ask the host. Roughly a third of the
    operations sit behind a door or in a namespace the table never mentions.
  - **Whose defect it is turns on who claimed the route.** No document claimed it
    and the host 404s → the CLI's phantom, hard failure. The LIVE TABLE claimed it
    and the host 404s → cloud's table and cloud's server disagree, which no edit
    here can settle: reported against `CONTRADICTED`, a CEILING that may not grow in
    silence and is free to fall when somebody redeploys. Today it is 3, all
    `/v1/billing` (`gpu-eligibility`, `payment-config`, `payment-methods`).
  - **The other direction asks the BINARY.** `hanzo <product> --help` — whether a
    person can reach a product is a fact about the built CLI, and a generated
    product that collides with a local command, or a relocation that stopped
    relocating, is invisible to any re-derivation and obvious to one exec.
  - **A served product with no command is drift unless `src/curation.rs` says
    otherwise**, and every entry naming a spelling gets that spelling RUN, so an
    excuse that stopped being true turns CI red. The applied count is pinned
    EXACTLY (`EXCUSED`, today 7): an exception nobody counts is how 21 served
    `/v1/deploy` operations came to be reachable by nothing while the table
    dropping them still called it a decision.
  - **BLIND fails.** No answer at all is not "no drift" — it is "I could not look".

  **Run `cargo test`, never `cargo test --bin hanzo`.** That selector takes only
  the binary's unit tests, so everything under `tests/` compiles and never runs.
  The release workflow said `--bin hanzo` from the day `spec_drift.rs` was written,
  which is how a spec built off a scratchpad capture passed every build it ever saw.
- **Top-level names come from two places and no third: a generated product, or a
  hand-written local command.** There was a curated ALIAS table (one entry, `hanzo
  logs` → `hanzo o11y logs`) for a nicer spelling of a nested op. It was a
  hand-kept claim about the route surface — the same shape of defect as a captured
  route table, one layer up — and cloud began serving `/v1/logs/{query,write,health}`.
  Yielding the name was not enough: `augment` skipped the shadowed alias while
  `resolve` still preferred it, so the parser MOUNTED the product and dispatch sent
  it to the o11y op, and `hanzo logs query` PANICKED on an argument id its own
  command never defined. A test that asserted only the MOUNT never saw it; the two
  halves disagreed in the gap between them. The mechanism is deleted and the test
  now walks parse → resolve → op.
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

**Stated gaps — upstream, and NOT to be patched here.** The CLI is a projection, and
a projection that repairs its source hides the defect at the one place it can be fixed.

- **Required-ness of query parameters is being lost in cloud's own document.** Four
  shipped operations declare `required: false` on a parameter whose own description
  reads "Required.": `GET /v1/o11y/{logs,metrics,status}` (`product`) and
  `DELETE /v1/clusters/{clusterId}/pools/{poolId}` (`provider`). The live table and
  the authored master agree, so this is zip's parameter projection in hanzoai/cloud
  dropping the flag. Until it is fixed clap cannot enforce those four, and the CLI
  must not restate the constraint by hand — a client that knows a rule the document
  does not is exactly the drift this pipeline exists to end.
- **`/v1/logs` and `/v1/o11y/logs` are two doors onto one noun** — "Search your org's
  logs by label, time and substring" vs "a page of one product's logs for the caller's
  org". One should win in hanzoai/cloud; the CLI follows whichever does.
- **The registry lists routes the server 404s** — 3 of them, all `/v1/billing`
  (`gpu-eligibility`, `payment-config`, `payment-methods`): a route registered whose
  mount or handler is dead, so the router knows it, the table it projects claims it,
  and nothing runs. `make verify` MEASURES this instead of leaving it a known hole
  (`CONTRADICTED`, a ceiling). Pricing was previously named here as "the bulk" of the
  class; it is not — its 14 paths answer 200 on every serial re-ask, and a concurrent
  sweep reading transient 404s as fact is exactly why the gate confirms a 404 three
  times before believing it. The fix for the real 3 is in cloud's router, never a
  list here.
- **`version::tests::a_v_prefix_is_tolerated` is flaky** — 1 failure observed in ~10
  full `cargo test` runs on this box, 0 in 8 consecutive runs after. It writes a stub
  script and execs it, so it races other tests' spawns. It is now in the release gate;
  an intermittent red gate teaches people to re-run, which is how a gate dies.
