# Contributing to Hanzo CLI

Thank you for contributing to the Hanzo CLI! We welcome contributions in the form of bug reports, feature requests, documentation improvements, and pull requests.

## Bug Reports and Feature Requests

Please submit bug reports and feature requests via GitHub Issues:
https://github.com/hanzoai/cli/issues

When filing an issue, please include:
- A clear, descriptive title
- Steps to reproduce the problem
- Expected vs. actual behavior
- Your OS and CLI version (`hanzo --version`)

## Development Setup

The Hanzo CLI is built in Rust (edition 2021). You will need a working Rust toolchain (Rust 1.75+ recommended) installed via [rustup](https://rustup.rs).

### 1. Clone the Repository

```bash
git clone https://github.com/hanzoai/cli.git
cd cli
```

### 2. Build from Source

```bash
cargo build
```

This builds the `hanzo` binary in `target/debug/hanzo`.

### 3. Run Linter

```bash
cargo clippy --bin hanzo
```

## Running Tests

The test suite consists of unit tests and integration tests that run the compiled binary:

### Run Full Test Suite

```bash
cargo test
```

### Test Locations

- **Integration tests:** `tests/cli.rs` runs the compiled CLI binary with real command-line arguments and exit codes, isolating state behind `--config <tempdir>`.
- **Unit tests:** Unit tests live alongside the implementation files, including `src/commands/product/tests.rs` and inline `#[cfg(test)]` modules throughout `src/`.

### Run Specific Tests

To run a single test or a subset of tests:

```bash
# Run a specific unit test by name
cargo test test_product_command

# Run all integration tests in tests/cli.rs
cargo test --test cli

# Run a specific test inside tests/cli.rs
cargo test --test cli test_help
```

## Pull Request Guidelines

1. **Discuss major changes:** For large architectural changes or new commands, please open an issue first to discuss the design before submitting code.
2. **Keep PRs focused:** Submit smaller, focused pull requests rather than large multi-purpose changes.
3. **Verify checks pass:** Before opening a PR, ensure that:
   - Code builds without warnings: `cargo build`
   - All tests pass: `cargo test`
   - Clippy passes cleanly: `cargo clippy --bin hanzo`
4. **Follow commit conventions:** Write clear commit messages explaining what was changed and why.

## Documentation & Community

- Documentation: https://docs.hanzo.ai
- Homepage: https://hanzo.ai
