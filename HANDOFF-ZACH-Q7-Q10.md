# Handoff: Q7 (forge clone on routed runs) and Q10 (per-machine agent instructions)

Both need a server-side contract change. The CLI was inspected in this clean
worktree at `2f059ca77`; neither can be fixed safely in the CLI alone. No
production config, keys or network state were changed.

## Q7 — routed coding run fails `git clone git@ssh.hanzo.ai:… Permission denied (publickey)`

### Evidence

- The runner already supports a per-run key. `hanzo-gen` rev
  `8807647e5ec41f51ad143685e2cda273c390d87d`:
  - `src/runner/mod.rs:76-78` — `RoutedRun { deploy_key: Secret, known_hosts: String }`.
  - `src/runner/mod.rs:264` — the run is decoded from the **claim response**, not
    from any CLI argument: `serde_json::from_str(&text).context("decoding claimed routed run")`.
  - `src/runner/mod.rs:540-552` — `execute_workspace_run` calls
    `Access::make(work_dir, &run.deploy_key.0, &run.known_hosts)` **only when
    `deploy_key` is non-empty**; otherwise there is no SSH override.
  - `src/runner/git.rs:16-30` — `Access::make` writes a 0600 private key and
    `known_hosts`, then builds
    `ssh -i <key> -o IdentitiesOnly=yes -o UserKnownHostsFile=<hosts> -o StrictHostKeyChecking=yes -o BatchMode=yes`;
    `prepare_git_workspace` sets it as `core.sshCommand` for clone **and push** and
    the directory is removed on drop. The runner side is done.
- The failure is therefore a **missing value in the cloud-issued claim**, not a
  missing CLI feature. A plain `git clone git@ssh.hanzo.ai:…` falls back to the
  machine user's own SSH key; on a machine with no forge key it fails exactly as reported.

### Why a CLI-only fix is not viable

- `POST /v1/git/keys` (`git.keyIn`) takes a **public** key (`key`, `repo`,
  `readOnly`, `title`). It registers a deploy key on the repo; it does not put a
  **private** key into the run claim the runner reads. Registering a key from the
  CLI cannot make the clone use it.
- `POST /v1/agent/targets` in the pinned spec has no `deployKey`/`knownHosts`
  field; there is no client-writable slot for the secret on the target record.
- The claim is assembled server-side and consumed inside Gen; the CLI never
  sees or writes that payload. No CLI flag can reach it.

### Ask (cloud / hanzo-inc/cloud)

1. **Provision a per-run credential at dispatch time** for any routed run whose
   `repo` resolves to a forge (`ssh.hanzo.ai`) and whose target has no standing
   forge credential:
   - generate an ed25519 keypair scoped to that repo, register the PUBLIC half
     with `readOnly: true` and a title containing the session id (audit trail);
   - put the PRIVATE half and the forge's host key line in the claim's
     `deployKey` / `knownHosts` fields; the runner already creates and destroys
     the 0600 files;
   - revoke the deploy key (or let the short-lived key expire) when the run
     reports terminal. Never echo the private half into run events or logs —
     events are readable by the whole org.
2. **Alternative if forge HTTPS is preferred**: send an HTTPS `cloneUrl` plus a
   short-lived per-run token in the claim, and add a parallel branch in
   `gen:src/runner/git.rs::prepare_git_workspace` that installs a credential
   helper (never `-c http.extraHeader=<token>` in a loggable argv). This is a Gen
   change, then a rev bump here — not a CLI-only change.
3. **Preflight, independent of 1/2**: the dispatch gate should refuse with an
   actionable error when a run is routed to a machine with no forge credential
   ("no forge key on target X; pick a sandbox target or install a deploy key"),
   instead of failing later inside `git clone`. A preflight makes the next
   occurrence diagnosable from the run record.
4. **Tests on the server**: claim for a forge repo contains non-empty
   `deployKey`/`knownHosts`; runner clone+push succeeds through `core.sshCommand`;
   private key absent from events/logs; key revoked on terminal; non-forge repos
   unchanged.

When the API exists, a contained CLI follow-up could be `hanzo link forge-key`
(generate keypair locally, register public half via the existing `git keys create`
generated command, seal the private half in KMS) — but cloud must first expose a
way to read that private half into the claim. Do not ship the CLI half alone.

## Q10 — one person's agent instructions identical across linked machines

### Evidence

- `hanzo link` subcommands are exactly `host`, `dial`, `status`, `rm`, `grant`,
  `revoke`, `folders` (`src/main.rs:789-845`). Folders are granted per machine
  via `/v1/agent/targets/{id}/folders` (`src/commands/link/folders.rs`).
- Machine registration (`POST /v1/agent/targets`, spec field list) carries
  capacity/host/kind/label/metrics/spec/status. **No instructions field, no hash,
  no read route for instructions.**
- The per-run fields `instructions` / `developerInstructions` exist on
  `agent.CodingStartIn` and `agent.talkIn`, and `RoutedRun.instructions` is
  delivered in the claim — but they are **per-run**, supplied by the caller, and
  are not stored on the machine record. The runner cannot read another machine's
  local `AGENTS.md`.

### Why a CLI-only fix would be misleading

A local-only `hanzo link instructions` could hash `AGENTS.md` / `CLAUDE.md` in
the current checkout, but that is not "identical across linked machines": there
is no route to write it to, or read it from, the other targets. Shipping a hash
command that cannot show or reconcile the siblings would give a false answer.

### Ask (cloud / hanzo-inc/cloud)

1. Add to the target record (owner-only): `instructions: string` (cap 128 KiB),
   `instructionsHash: string` (SHA-256 of the exact bytes), `updatedAt`.
2. Accept writes on `POST /v1/agent/targets` and a `PATCH /v1/agent/targets/{id}`
   (or a dedicated `PUT /v1/agent/targets/{id}/instructions`). Only the machine's
   owner may write; other org members get 403. Audit each write.
3. Include `instructionsHash` (not the text) in `GET /v1/agent/targets` and
   `GET /v1/agent/targets/{id}` so a mismatch is visible without reading every
   machine's instructions. `GET /v1/agent/targets/{id}/instructions` returns the
   text to the owner only — instructions are executable text and may carry
   tokens, so treat them as secrets in list views and logs.
4. At run dispatch, merge the machine's stored instructions into the claim's
   `RoutedRun.instructions` deterministically (machine instructions first, then
   the caller's per-run instructions), capped at 128 KiB. Keep the current
   behaviour when the machine has none.
5. Re-pin the spec (`make openapi` in cloud, `make spec` / `genspec` +
   `genproduct` here) so the new fields become generated commands/flags. Then a
   contained CLI addition is possible:
   - `hanzo link instructions` — print, per linked machine, its id/host and
     stored hash, plus the local file's hash and a `match`/`drift` marker;
   - `hanzo link instructions set [--all]` — upload the local instructions to
     this machine / every owned target, after showing the hash to be written;
   - refuse to print instruction text on a shared terminal unless explicitly
     asked, and never print it to stdout by default.
6. Tests: owner-only write/read, hash stability across updates, drift detection
   across two targets, run claim contains the stored instructions, size cap,
   text redacted from list endpoints and logs.
