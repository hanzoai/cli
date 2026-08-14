<p align="center"><img src=".github/hero.svg" alt="hanzo" width="880"></p>

# hanzo — AI engineer in your terminal

The Hanzo CLI. One binary to code with an AI agent, sign in, and drive every product of
the [Hanzo cloud](https://hanzo.ai) from the terminal — plus run the network. Written in
Rust: a single static binary, rustls TLS, no runtime, no daemon.

```bash
hanzo "add pagination to the /v1/orders endpoint and write the tests"
```

## Install

```bash
curl -fsSL https://hanzo.sh | sh
```

One install line, and it is the same one every Hanzo surface prints. It resolves the
release asset for your platform (linux · macOS on amd64 · arm64, Windows on amd64),
verifies its sha256, and puts `hanzo` in `~/.local/bin` — along with `hanzo-mcp` (the
MCP server) and `dev` (the coding agent `hanzo code` runs). Re-run to upgrade.

`hanzo.sh` drives this repo's `install.sh`, which is the one implementation of
"fetch a Hanzo binary". To install only the CLI: `curl -fsSL https://hanzo.sh/cli | sh`.

Or from Homebrew:

```bash
brew install hanzoai/tap/hanzo
```

From source (Rust toolchain):

```bash
git clone https://github.com/hanzoai/cli && cd cli
cargo install --path .
```

## Quick start

```bash
# Sign in — interactive picker: Hanzo (OIDC) · OpenAI · Anthropic · paste a key
hanzo auth login

# Bare `hanzo` IS an AI engineer in your terminal — an interactive coding session,
# linked to your cloud (mission-control) when you're signed in.
hanzo

# Or run a task headless
hanzo "fix the failing test in api/handlers.go"

# Who am I, and what's left in the tank
hanzo auth show
hanzo usage summary
hanzo billing balance
```

A group name on its own is not a command — `hanzo usage` and `hanzo auth` print their
subcommands and stop. A first word the CLI does not recognise is read as the task, so
`hanzo login` starts a coding session about the word "login"; the command is
`hanzo auth login`.

## Coding — bare `hanzo`

`hanzo` wraps a local coding agent as a session-aware, resumable, portable object: the
Hanzo MCP toolset is attached, model calls route through your metered Hanzo cloud, and a
signed-in session streams live to mission-control. Auto-approve is on by default; the
repo's own `.mcp.json` is trust-gated off unless you opt in.

The default backend is `dev`, our own agent — `curl -fsSL https://hanzo.sh | sh` installs
it. `--backend claude` and `--backend codex` drive other agents you already have.

```bash
hanzo --model enso-ultra "refactor the auth middleware"
hanzo --backend dev "port this module to async"
hanzo --resume <session-id>          # continue a prior session
hanzo --ask "…"                      # confirm each action (alias: --safe)
hanzo --no-link "…"                  # keep it local, don't stream to cloud
hanzo --no-route "…"                 # use the backend's own model account
```

Zen models (`enso`, `enso-ultra`, `enso-flash`, `zen5-coder`, …) are Hanzo's own
family; the gateway is the sole authority on the model catalog.

## Commands

The CLI is a resource-noun tree — `hanzo <resource> <command>`. Cloud capabilities
beyond the hand-written resources appear as first-class **generated product
subcommands** (`hanzo <product> <resource…> <verb>`, typed from the OpenAPI specs) —
the one interface to `api.hanzo.ai/v1`. There is no `hanzo api` verb and no raw-path
escape.

### Identity & billing

Multi-identity, like `gh auth switch` — hold many principals at once and switch
between them; a second login never clobbers the first.

```bash
hanzo auth login [--provider hanzo|openai|anthropic] [--token -]
hanzo auth show                 # the active identity
hanzo auth list                 # every identity, active one marked
hanzo auth use [owner/name]     # switch active identity
hanzo auth token                # the active short-lived access token
hanzo auth logout [identity] [--all]
hanzo usage summary             # your own footprint over a window
hanzo usage samples             # per-provider: one linked account's own plan
hanzo billing balance | deposit
hanzo connector add|list|verify|rm --provider cloudflare
```

Secrets arrive on **stdin only**, never argv (`--token -`) — nothing lands in
shell history, `ps`, or CI logs. Credentials live in the OS keychain (macOS /
Windows) or an owner-only `0600` file otherwise.

### Cloud

```bash
hanzo agents run <agent>                 # run one of your org's agents, keep the run
hanzo clusters create|list|pools|rm      # the compute you rent, and BYO clusters
hanzo cloud list|accounts                # bring your own DigitalOcean / AWS / GCP
hanzo engine serve <model>               # serve a model locally on an OpenAI-compatible endpoint
hanzo up cloud|iam|kms|gateway|storage|pubsub      # bring a Hanzo service up on your machine
```

`engine serve` needs the Hanzo engine binary on PATH (or `HANZO_ENGINE_BIN`); it is not
part of this install and the command says so when it is missing.

### Network & wallet

```bash
hanzo network list|current|use <name>|add <name> …   # sovereign L1: network_id == chain_id
hanzo wallet show|address|create [--local]|import|use|list   # PQ cloud custody (KMS/MPC) or local keychain
```

### Fabric & fleet — hanzo.network

```bash
hanzo fabric …                  # run/join the L1 fabric with hanzod, query its model cluster
hanzo runner …                  # provide this machine as a CI runner
hanzo link                      # put this shell on the fabric so the console can drive it
```

`hanzo-node` is not a second program: the installer writes it as a symlink to this same
binary, because cloud's Go control binary resolves that name first and delegates to it.
One build, two names, so the two can never be different versions.

### Build & ship

```bash
hanzo init [template]           # scaffold a new project
hanzo share <port|host:port|url># public https://<token>.share.hanzo.ai URL on the zero-trust fabric
hanzo scan <path>               # find exposed secrets before you commit (non-zero on a find)
hanzo mcp                       # every tool your org can call, in one list
```

## Configuration

- `hanzo config list | get <key> | set <key> <value>` — non-secret local settings,
  stored at `~/.config/hanzo/config.toml` (network selection, active identity index,
  custom networks). Never holds token material.
- `~/.hanzo/settings.json` — coding-agent defaults: `model`, `smallFastModel`,
  `autoApprove`, `mcp`, `contextWindow`. Every key is optional; an explicit CLI flag
  or process env always wins.

Global flags valid on any subcommand: `--config <file>`, `--verbose`.

## Build from source

```bash
cargo build            # build gate
cargo test             # full suite: unit tests + shipped-binary tests (assert_cmd)
cargo clippy --bin hanzo
```

`tests/cli.rs` runs the real binary with real argv and exit codes; state is isolated
behind `--config <tempdir>`. The live auth flow is gated on `HANZO_E2E_TOKEN` (a real
`hanzo.id` bearer) and honestly skips when absent.

## Contributing

Issues and PRs welcome — see [CONTRIBUTING.md](CONTRIBUTING.md).

## License

MIT © Hanzo AI

## Support

- Docs: https://docs.hanzo.ai
- Issues: https://github.com/hanzoai/cli/issues
- Email: support@hanzo.ai

## Hanzo — the Open AI Cloud

Open source · every language · on-chain settlement. [hanzo.ai](https://hanzo.ai) · [docs.hanzo.ai](https://docs.hanzo.ai)

**SDKs in every language** — [Python](https://github.com/hanzoai/python-sdk) (flagship) · [TypeScript](https://github.com/hanzo-js/sdk) · [Go](https://github.com/hanzo-go/sdk) · [Rust](https://github.com/hanzo-rs/sdk) · [C++](https://github.com/hanzo-cpp/sdk) · [Swift](https://github.com/hanzo-swift/sdk) · [Kotlin](https://github.com/hanzo-kt/sdk) · [umbrella](https://github.com/hanzoai/sdk)
