# @hanzo/cli

The official Hanzo AI CLI for container management and development tools.

## Installation

```bash
npm install -g @hanzo/cli
```

Or with yarn:
```bash
yarn global add @hanzo/cli
```

Or with pnpm:
```bash
pnpm add -g @hanzo/cli
```

## Quick Start

Run any container with a single command:

```bash
# Run nginx
hanzo run nginx

# Run with port mapping
hanzo run nginx -p 8080:80

# Run Supabase stack
hanzo run supabase/supabase

# Run with environment variables
hanzo run postgres -e POSTGRES_PASSWORD=secret

# Run in detached mode
hanzo run redis -d
```

## Features

- 🐳 **Container Management** - Run any OCI container with automatic image pulling
- 📦 **Stack Support** - Deploy complex stacks like Supabase with one command
- 🔍 **Runtime Detection** - Automatically detects Docker Desktop, Colima, Podman, etc.
- 🚀 **Zero Config** - Works out of the box with sensible defaults
- 📊 **Workload Tracking** - Integrated with hanzod for monitoring

## Commands

### Container Commands

```bash
# Run a container
hanzo run <image> [options]
  -d, --detach              Run in background
  -p, --port <mapping>      Port mapping (e.g., 8080:80)
  -e, --env <var>          Environment variable
  -v, --volume <mapping>    Volume mount

# List containers
hanzo ps
  -a, --all                Show all containers

# Stop a container
hanzo stop <container-id>

# View logs
hanzo logs <container-id>
  -f, --follow             Follow log output

# Execute command in container
hanzo exec <container-id> <command>

# Pull an image
hanzo pull <image>

# Show available runtimes
hanzo runtimes
```

### Development Commands

```bash
# Initialize a new project
hanzo init [template]

# Start development server
hanzo dev
  --port <port>           Port to use (default: 3000)
  --hot                   Enable hot reload

# Build project
hanzo build
  --prod                  Production build

# Deploy to Hanzo Cloud
hanzo deploy

# Manage authentication
hanzo auth login
hanzo auth logout
hanzo auth whoami
```

### AI Commands

```bash
# Chat with AI
hanzo chat "Your question here"

# Generate code
hanzo generate <type> <name>

# Analyze code
hanzo analyze [path]
```

## Examples

### Running Nginx
```bash
hanzo run nginx -p 8080:80
```

### Running PostgreSQL
```bash
hanzo run postgres \
  -e POSTGRES_PASSWORD=mysecret \
  -v postgres_data:/var/lib/postgresql/data \
  -p 5432:5432
```

### Running Supabase
```bash
hanzo run supabase/supabase
```

This will:
1. Clone the Supabase repository
2. Pull all required images
3. Start the entire stack
4. Provide you with URLs for Studio, API, and Database

### Running Redis
```bash
hanzo run redis -d
```

## Runtime Support

Hanzo CLI automatically detects and uses available container runtimes:

- Docker Desktop
- Colima
- Podman
- Containerd
- Rancher Desktop

## Configuration

Configuration file is stored at `~/.hanzo/config.toml`:

```toml
[defaults]
runtime = "auto"  # auto, docker, colima, podman
registry = "docker.io"

[auth]
token = "your-auth-token"

[telemetry]
enabled = false
```

## Building from Source

If you want to build from source:

```bash
# Clone the repository
git clone https://github.com/hanzoai/cli
cd cli

# Build with Cargo
cargo build --release

# Install globally
cargo install --path .
```

## Contributing

We welcome contributions! Please see our [Contributing Guide](CONTRIBUTING.md).

## License

MIT © Hanzo AI

## Support

- Documentation: https://docs.hanzo.ai/cli
- Discord: https://discord.gg/hanzoai
- GitHub Issues: https://github.com/hanzoai/cli/issues
- Email: support@hanzo.ai

---

Made with ❤️ by [Hanzo AI](https://hanzo.ai)