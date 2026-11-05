# @hanzo/cli

The Hanzo CLI — unified entrypoint for Hanzo AI cloud, `dev` coding agent, `hanzo-mcp`, `hanzo-engine` local inference, `hanzo-node` (`hanzod`), and `hanzo-vm`.

```bash
npm i -g hanzo
hanzo --help
```

`postinstall` stays lightweight by downloading only the `hanzo` CLI binary for
your platform from the GitHub release matching this package's version and
verifying it against its `.sha256` sidecar. Companion binaries (`dev`,
`hanzo-mcp`, `hanzo-engine`, `hanzod`, `zt`, `hanzo-vm`) are downloaded and
verified **lazily on first use** into `bin/`, and `bin/hanzo.js` prepends its
own `bin/` directory to `PATH` (handling Windows `Path` case-insensitively) so
companion executables are always discoverable.

Prebuilt for macOS (arm64, x64), Linux (arm64, x64) and Windows (x64). On any
other platform the install is skipped with a warning rather than failing.

The binary is Rust and lives at https://github.com/hanzoai/cli.
