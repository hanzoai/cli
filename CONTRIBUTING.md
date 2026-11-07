# Contributing to Hanzo CLI

We welcome contributions to Hanzo CLI! Please review the following
guidelines before submitting a pull request.

## Code of Conduct

Please read and adhere to the
[Contributor Covenant Code of Conduct](https://www.contributor-covenant.org/).

## Issues and Pull Requests

- **Before opening a new issue or pull request, please search existing
  issues to see if it has already been addressed.**
- **Clearly describe the problem or feature you are working on.**
- **Provide steps to reproduce the issue (if applicable).**
- **Write clear and concise commit messages.**
- **Pull requests should be small and focused.**
- **Test your changes thoroughly before submitting a pull request.**

## Development Workflow

1. **Clone the repository:**

   ```bash
   git clone https://github.com/hanzoai/cli
   cd cli
   ```

2. **Build:**

   ```bash
   cargo build
   ```

3. **Test:**

   ```bash
   cargo test
   ```

   This runs unit tests plus the shipped-binary tests in `tests/cli.rs`
   (real binary, real argv and exit codes, state isolated behind
   `--config <tempdir>`).

4. **Lint:**

   ```bash
   cargo clippy --bin hanzo
   ```

### Running a Single Test

```bash
cargo test <test_name>
cargo test --test cli <test_name>
```

### Where Tests Live

- `tests/cli.rs` — end-to-end tests against the real binary.
- Unit tests beside the code they cover (e.g.
  `src/commands/product/tests.rs`).
- `crates/client/tests/contract.rs` — client contract tests.

### Pull Request Process

1. Open an issue first for large or significant changes.
2. Keep the PR small and focused; describe what changed and why.
3. Run `cargo build`, `cargo test`, and `cargo clippy --bin hanzo`
   before submitting.
4. Issues and PRs are welcome at
   [hanzoai/cli](https://github.com/hanzoai/cli/issues).
