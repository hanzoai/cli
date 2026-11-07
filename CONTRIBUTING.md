# Contributing to Hanzo CLI

Thank you for your interest in contributing to the Hanzo CLI! This document outlines the process for developing, testing, and submitting contributions to the project.

## Development Setup

The Hanzo CLI is written in Rust. Make sure you have a modern Rust toolchain installed (stable edition 2021 or newer) via [rustup](https://rustup.rs/).

### Clone the Repository

```bash
git clone https://github.com/hanzoai/cli.git
cd cli
```

### Build from Source

Compile the binary in debug mode:

```bash
cargo build
```

Or build an optimized release binary:

```bash
cargo build --release
```

Run the binary locally:

```bash
cargo run --bin hanzo -- --help
```

## Running Tests

Tests are organized into unit tests alongside source code and integration tests in `tests/`:

- Run all unit and integration tests:
  ```bash
  cargo test
  ```

- Run integration tests with isolated state:
  ```bash
  cargo test --test cli
  ```

- Run a specific test case:
  ```bash
  cargo test <test_name>
  ```

## Code Quality and Style

Before creating a pull request, ensure your code passes linting and formatting checks:

- Run Clippy:
  ```bash
  cargo clippy --bin hanzo
  ```

- Check code formatting:
  ```bash
  cargo fmt --check
  ```

To automatically format your code according to style conventions:
```bash
cargo fmt
```

## Pull Request Guidelines

1. **Branching:** Create a descriptive branch from `main` (e.g., `git checkout -b feat/my-feature` or `fix/issue-123`).
2. **Commit Messages:** Write concise, imperative commit messages describing the change and why it was made.
3. **Link Issues:** Reference relevant issues in your PR description (e.g., `Fixes #12`).
4. **CI Checks:** Ensure local builds and tests pass cleanly before pushing.
