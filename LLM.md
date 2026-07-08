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
- `login` / `whoami` / `logout` — IAM OIDC PKCE S256 (`src/iam/*`, HIP-0111).
- `network list|current|use <name>|add <name> …` — the network model (below).
- `wallet show|address|create|import <secret>|use <addr>|list` — wallet (below).
- `node up|status|join <network>|stop` — run/join hanzo.network with hanzod.
- `cluster topology|models|route|placement|chat|search` — talk to a hanzo node
  (default node URL = the active network's `api`).
- `deploy` — targets the active network; the active wallet signs (auto-provisions
  one if none, on a real deploy).
- `agent`, `build`, `dev`, `init`, `docs|mdx|ui|mcp` (TS proxies).

## Network model (`src/commands/network.rs`) — same as the console + fabric
Sovereign L1 ⇒ `network_id == chain_id`. Built-ins mirror the console selector
and the node's `hanzo-mining` `NetworkType`:

| name    | network_id / chain_id | rpc                                  | api            |
|---------|-----------------------|--------------------------------------|----------------|
| mainnet | 36963                 | https://rpc.hanzo.network            | api.hanzo.ai   |
| testnet | 36964                 | https://rpc.hanzo-test.network       | api.hanzo.ai   |
| devnet  | 36965                 | https://rpc.hanzo-dev.network        | api.hanzo.ai   |
| local   | 31337                 | http://localhost:9650/ext/bc/C/rpc   | localhost:3690 |

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
- `src/iam/*` — IAM OIDC client + OS-keychain token store (the keychain pattern
  wallet secrets reuse).
