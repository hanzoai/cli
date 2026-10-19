# CLI end-to-end checks

`make test` runs the Rust workspace, command/parser checks, installer fixtures and API-spec drift checks. Live checks are opt-in: unit tests alone do not prove production works.

Run the production checks using your existing login:

```sh
HANZO_E2E_TOKEN="$(hanzo auth token)" make e2e
```

This exercises token login through stdin, identity, authenticated usage JSON, a local `dev` task that writes and verifies a file in a temporary Git repository, and a read-only cloud task that clones a real repository and completes inference. It requires `dev` on PATH. The local and cloud tasks consume normal usage; the cloud test leases a sandbox. It does not log out your existing account or modify the source repository.

Test the public installer and its actual downloaded binaries:

```sh
HANZO_E2E_TOKEN="$(hanzo auth token)" make e2e-installed
```

The installer runs in a temporary directory, verifies release checksums, and installs `hanzo`, `hanzo-node`, `dev`, `gen`, and MCP. Tests use that directory first on PATH. It leaves your normal installation intact. A missing token is an error, and every coding test must actually complete; HTTP errors and a missing output file fail the run.

The tests cover the machine and platform on which they run. They do not replace macOS/Windows installation checks or interactive browser OAuth testing. Token login uses a real existing IAM session; it does not simulate a fresh browser sign-in.
