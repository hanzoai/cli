# hanzo

One lightweight install for the entire Hanzo stack — CLI, client SDK, `dev` coding agent, `mcp` server, `engine` (high-performance local inference), and `node`.

```sh
npm install -g hanzo     # unified CLI + lazy-loaded stack (cli, dev, mcp, engine, node)
npm install hanzo        # TypeScript client SDK (hanzo, hanzo/ai, hanzo/ui)
```

```js
import { Configuration, AdminApi } from 'hanzo'      // the Open AI Cloud
import { Agent } from 'hanzo/ai'                     // agents and inference
import { Button } from 'hanzo/ui'                    // the component library
```

```sh
hanzo --version
hanzo dev                # coding agent + MCP (fetched lazily on first run)
hanzo mcp                # Hanzo MCP server (fetched lazily on first run)
hanzo engine serve <id>  # high-perf local LLM inference (fetched lazily on first run)
hanzo node up            # Hanzo L1 chain node (hanzod)
```

## What it is

A lightweight front door that installs fast and fetches heavy native binaries lazily on first use:

| surface | comes from | what it is |
|---|---|---|
| `hanzo` (import) | `hanzoai` | the full Open AI Cloud client, generated from `hanzo-inc/cloud` |
| `hanzo/ai` (import) | `@hanzo/ai` | the AI and agents library |
| `hanzo/ui` (import) | `@hanzo/ui` | the component library (optional peer dependency) |
| `hanzo` (command) | `@hanzo/cli` | the Rust CLI (`hanzoai/cli`), fetched for your platform on install |
| `hanzo dev` / `hanzo-dev` | `hanzoai/dev` | terminal AI coding agent, fetched lazily on first run |
| `hanzo mcp` / `hanzo-mcp` | `hanzoai/mcp` | Model Context Protocol server, fetched lazily on first run |
| `hanzo engine` / `hanzo-engine` | `hanzoai/engine` | high-performance local inference engine, fetched lazily on first run |
| `hanzo node` / `hanzo-node` | `hanzoai/node` | Hanzo network validator/chain node (`hanzod`), fetched lazily on first run |

Nothing is vendored and nothing is reimplemented here — there is one downloader,
one generated client and one agents library, each in the package that owns it.
Depending on any of them directly still works; `hanzo` is the single name you
reach for so you never have to remember which `@hanzo/*` package to install.
