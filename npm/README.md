# @hanzo/cli

The Hanzo CLI — manage Hanzo AI cloud resources from your terminal.

```bash
npm i -g @hanzo/cli
hanzo --help
```

`postinstall` downloads the `hanzo` binary for your platform from the GitHub
release matching this package's version (along with the companion `dev`,
`hanzo-mcp`, `gen`, and `zt` binaries), and **verifies each against the
`.sha256` published beside it**. `bin/hanzo.js` prepends its own `bin/`
directory to `PATH` so `hanzo dev` and companion tools are always discoverable.
If you install with `--ignore-scripts`, run
`node node_modules/@hanzo/cli/install.js` yourself — the wrapper will tell you so
rather than failing with a confusing error.

Prebuilt for macOS (arm64, x64), Linux (arm64, x64) and Windows (x64). On any
other platform the install is skipped with a warning rather than failing.

The binary is Rust and lives at https://github.com/hanzoai/cli.
