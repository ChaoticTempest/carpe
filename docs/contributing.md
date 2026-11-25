# Contributing

## Development setup

```bash
git clone https://github.com/ChaoticTempest/carpe
cd carpe
cargo build --locked
cargo run -- status          # inspect the pool for this checkout
```

The binary is `carpe` (`src/main.rs`); there is no library crate. `carpe` is not
published on crates.io, so install from git or a local checkout:

```bash
cargo install --git https://github.com/ChaoticTempest/carpe
cargo install --path .
```

Useful optional tools for the checks below:

```bash
cargo install cargo-audit cargo-llvm-cov
```

## Code checks

Run these before opening a pull request:

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo audit --deny warnings
cargo test --locked
cargo llvm-cov --html       # optional coverage report
```

If you need to apply formatting first, run `cargo fmt --all`. The release
profile uses LTO, a single codegen unit, panic=abort, and stripped symbols; use
`cargo build --release` for a production binary.

## Guidelines

- Keep documentation truthful to the source. Weights, flags, environment
  variables, and defaults in [README](../README.md),
  [cli-reference](cli-reference.md), and [architecture](architecture.md) must
  match `src/*.rs`; do not document behavior that does not exist.
- Keep modules focused: `main.rs` dispatches, `repo.rs` identifies
  repos/workspaces, `slots.rs` allocates, `meta.rs` scores, `lockfile.rs`
  locks, `prune.rs` reclaims space, and `utils.rs` holds shared helpers.
- Hold a slot lock for the entire cargo invocation; any new selection logic
  must remain non-blocking and skip busy slots rather than wait.
- Tests create temporary directories and use a time-based nonce helper
  (`test_rand_nonce`) to avoid collisions. Clean up after yourself, and be
  careful with tests that mutate process-wide environment variables.
- Non-building Cargo subcommands must keep bypassing slot selection.
- Preserve the marker and `.carpe-meta` formats, or update both the reader and
  writer together along with the documentation.
