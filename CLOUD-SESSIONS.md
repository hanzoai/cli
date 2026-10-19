# Cloud coding sessions

`hanzo --cloud` runs work on Hanzo's Go cloud coding service. The Rust CLI submits
the task and returns a session ID. Closing the terminal does not stop the run.
Ordinary `hanzo` runs locally; its cloud session stream is remote observation,
not remote execution.

`--metered` selects Hanzo's metered models for a local session. This replaces
the old `--cloud` model-routing spelling: `--cloud` now means remote execution.

```sh
git push -u origin HEAD
hanzo --cloud "Fix the flaky cache test and run the suite 30 times"
hanzo --cloud "Check the API documentation against the server" --watch
hanzo --cloud --repo github.com/acme/api --base main --mode plan "Explain the cache"
hanzo --cloud                         # list coding sessions
hanzo --cloud --resume SESSION_ID     # inspect status and recent events
hanzo --cloud --resume SESSION_ID --watch
hanzo -p "Add regression tests" --cloud SESSION_ID
hanzo --teleport SESSION_ID
```

`hanzo code --cloud` accepts the same flags. `--claude` and `--codex` select a
cloud harness; the default is Hanzo's `dev` agent. Availability and connected
accounts are checked by the service. `--model` names a model for harnesses that
support it. `--mode plan` reads without pushing; `--mode setup` inspects the
repository's environment. The normal mode is `build`.

Without `--repo`, the CLI reads origin and the current branch, refuses known
unpushed commits or a missing upstream, and warns about uncommitted files.
Only pushed files reach the sandbox. GitHub and Hanzo forge origins are supported;
there is no local repository bundle upload. An explicit `--repo` targets that
remote directly and does not upload or inspect the local checkout.

Follow-up tasks require a finished or paused run. They use the service's `after`
field to continue from prior work on a new branch. This CLI does not currently
redirect a running cloud sandbox. Ctrl-C while watching only detaches the viewer.
The terminal shows recent events; the browser session holds the transcript.

`--teleport` checks the repository identity and clean working tree, fetches the
session branch, and creates `cloud/SESSION_ID`. It never forces a checkout or
overwrites an existing branch. The transcript remains linked in the browser;
native harness conversation import is not implemented.

Cloud runs use the authenticated identity from `hanzo auth login`. Repository
access, environments, sandbox isolation, credentials, execution limits and pull
request creation are enforced by the service. An accepted session means queued
or started, not successfully completed: use `--watch` and inspect its result.
An error result exits nonzero.

Existing resource commands expose the supporting services:

```sh
hanzo provider github user get             # connection status
hanzo provider github user connect --help  # connect your GitHub identity
hanzo provider github installations        # repository installations
hanzo environment list
hanzo environment set --help
hanzo agent coding changes SESSION_ID
hanzo agent coding artifacts SESSION_ID
hanzo agent sessions pause SESSION_ID
hanzo agent sessions stop SESSION_ID
hanzo auto automations --help
```

## Verification and remaining parity work

The CLI workspace tests and spec derivation gates pass. A live read-only run,
`sess_c5ef4906c227756ab6055b729d0f51b8`, cloned GitHub, executed the agent, and
completed successfully. Live session listing, completed-session inspection,
GitHub connection status, and environment listing also answered successfully.

The delegated-identity 402 failure found during live testing is fixed in production
(API v8.5.838 and later). IAM's confined inference token carries a validated
billing organization even though its role is deliberately limited to member.
The API now preserves that validated organization without granting admin access.
A production cloud completion test passed after deployment. Token login and a
local dev task that writes and verifies a file also passed against production.
See [E2E.md](E2E.md) for repeatable source and public-installer checks.

Still unimplemented here: uploading a local repository bundle, importing the
native harness conversation into a local session, and delivering follow-up
instructions to a running cloud sandbox. Scheduling, environment configuration,
pause/stop, PR creation/merge, browser/mobile handoff, and CI auto-fix were not
verified end to end in this change. Published API routes alone do not prove those
workflows.
