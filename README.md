# carpe

`carpe` multiplexes `CARGO_TARGET_DIR` across git worktrees. All worktrees of a
repo share a small pool of target directories, so you can build in two
worktrees at once instead of blocking on Cargo's own build-directory lock,
while reusing incremental artifacts.

Arguments after `carpe` are passed to `cargo`: carpe selects a slot, sets
`CARGO_TARGET_DIR`, and execs the real cargo. See the
[CLI reference](docs/cli-reference.md), [architecture](docs/architecture.md),
and [contributing guide](docs/contributing.md) for details.

## Quick start

| Command | Purpose |
| --- | --- |
| `carpe build` | Run `cargo build` with an allocated target-dir slot |
| `carpe test --release` | Any cargo arguments work (`carpe check`, `carpe +nightly build`, `--manifest-path`, ...) |
| `carpe status` | Show pool, preferred slot, slot sizes, and busy state |
| `carpe info` | Show workspace, pool, cache root, and slot statistics |
| `carpe toggle-override` | Install/remove the global `cargo` shim that routes through carpe (alias: `carpe override`) |
| `carpe prune` | Remove unlocked slots in the current repo's pool |
| `carpe prune -n` | Preview deletions without removing files |
| `carpe prune -i` | Choose slots interactively (disabled in CI/non-interactive terminals) |
| `carpe prune --lru 2` | Keep the two newest unlocked slots in the current pool |
| `carpe prune --auto` | Prune oldest unlocked slots across all pools until under the storage budget |
| `carpe prune -a` | Prune unlocked slots across all pools |
| `carpe -h` / `carpe --help` | Print usage (exit `0`); no arguments prints usage with exit `2` |
| `carpe -V` / `carpe --version` | Print `carpe 0.1.0` |

A leading literal `carpe` token is stripped from the arguments before dispatch,
so invocations that include the subcommand name work as expected
(`cargo carpe build` with a `cargo-carpe` executable on `PATH`). Non-building
Cargo subcommands (`fmt`, `add`, `remove`, `rm`, `metadata`, `tree`, `new`,
`init`, `publish`, `search`, `login`, `logout`, `owner`, `vendor`, `yank`) are
forwarded straight to cargo without allocating a slot.

## Slot selection & affinity

Slots are ranked holistically across all existing slots in the pool; only
unlocked slots can be chosen.

| Signal | Source | Points |
| --- | --- | --- |
| Git `HEAD` match | `head` in `.carpe-meta` vs `git rev-parse HEAD` | `+100` |
| Preferred slot for this worktree | `carpe-slot` marker in the worktree's git dir | `+75` |
| Toolchain match | Leading `+toolchain` argument | `+50` |
| Profile match | `--release` / `-r` / `--profile` / `--profile=...` (default `debug`) | `+40` |
| Target match | `--target` / `--target=...` | `+40` |
| Features match | `--features` / `-F` / `--features=...` | `+40` |
| Branch match | `branch` in `.carpe-meta` vs current branch (detached `HEAD` never matches) | `+20` |

Ties are broken by **score descending → timestamp descending → slot index
ascending**. The chosen slot's `.carpe-meta` is rewritten and the worktree's
preferred-slot marker is updated. If every candidate is locked, a new slot is
created at the highest existing index + 1. Each slot's `.carpe-lock` file is
locked non-blockingly with [`fd-lock`](https://crates.io/crates/fd-lock)
(`flock` on Unix, `LockFileEx` on Windows) and held until cargo exits.

## Cache layout

Pool and slot names are `<sanitized-basename>-<8-hex-hash>` and
`<pool>-<slot>` respectively. The basename comes from the main worktree root
(or the Cargo workspace root outside git); the hash is produced by `short_hash`
— a stable 64-bit FNV-1a hash of the canonicalized repo/workspace identity
path, truncated to the first 8 hex digits — so unrelated repos sharing a
directory name (common: `api`, `server`, `worker`, ...) do not collide, and the
pool name stays the same across Rust releases and toolchains.

Upgrading from a version that used std's `DefaultHasher` changes the hash and
therefore creates a new pool once; the old directories can be reclaimed with
`carpe prune -a`.

| OS | Cache root |
| --- | --- |
| Linux / BSD | `$XDG_CACHE_HOME/carpe/` if set, otherwise `~/.cache/carpe/` |
| macOS | `~/Library/Caches/carpe/` (the XDG variable is not used on macOS) |
| Windows | `%LOCALAPPDATA%\carpe\` |

`carpe` falls back to the system temp directory if the platform cache directory
cannot be determined. Per-repo metadata lives in the worktree's own git dir
(or under `<cache-root>/state/` outside git), and is removed with
`git worktree remove`.

## Environment & warnings

| Variable | Effect |
| --- | --- |
| `CARPE_MAX_STORAGE_GB` | Storage budget in GiB for `prune --auto` and the warnings in `status`/`info` (default `128.0`) |
| `CARPE_CARGO_PATH` | Explicit path to the real cargo executable (highest resolution precedence) |
| `CARPE_CI`, `CI`, `CONTINUOUS_INTEGRATION` | Mark the environment as CI; disables interactive pruning |
| `CARGO_HOME` | Cargo bin dir location used by `toggle-override` (`cargo` / `cargo.real`) |
| `CARGO_TARGET_DIR`, `CARGO_BUILD_TARGET_DIR` | If non-empty, carpe warns and overrides them with the chosen slot |
| `PATH` | Used to locate `cargo`, `cargo.real`, and `rustup` |

Passing an explicit `--target-dir` also produces a warning: Cargo's CLI flag
wins over carpe's slot. A full list of variables, warnings, exit codes, and
argument normalization rules is in the
[CLI reference](docs/cli-reference.md).

## Examples

Two worktrees building in parallel:

```console
$ cd ~/src/myrepo                 # main worktree
$ carpe build
      Carpe using <cache-root>/carpe/myrepo-a1b2c3d4-0
   Compiling myrepo v0.1.0

# meanwhile, in another terminal:
$ cd ~/src/myrepo-feature-x       # linked worktree, first build
$ carpe build
      Carpe preferred slot myrepo-a1b2c3d4-0 is busy, using myrepo-a1b2c3d4-1 instead (parallel build)
   Compiling myrepo v0.1.0

# later, slot 1 is free and preferred, so it is reused:
$ carpe build
      Carpe using <cache-root>/carpe/myrepo-a1b2c3d4-1
    Finished `dev` profile
```

Inspect the pool:

```console
$ carpe status
Pool:      myrepo-a1b2c3d4
Marker:    /home/you/src/myrepo/.git/carpe-slot
Preferred: 0

Slots:
  myrepo-a1b2c3d4-0         1.20 GB    [free, preferred] (modified: just now)
  myrepo-a1b2c3d4-1       850.0 MB    [free]            (modified: 10m ago)

Total pool size: 2.05 GB

$ carpe info
Carpe System & Workspace Info:
------------------------------
CWD:            /home/you/src/myrepo
Workspace Root: /home/you/src/myrepo
Pool Name:      myrepo-a1b2c3d4
Cache Root:     /home/you/.cache/carpe
Marker Path:    /home/you/src/myrepo/.git/carpe-slot
Preferred Slot: 0
Git Common Dir: /home/you/src/myrepo/.git
Git Worktree:   /home/you/src/myrepo/.git

Slot Pool Stats: 2 total slots (2 free, 0 busy), total size 2.05 GB
```

## Build & install

`carpe` is not published on crates.io. Install from this repository or from a
local checkout:

```bash
cargo install --git https://github.com/ChaoticTempest/carpe
cargo install --path .
```

## License

Dual-licensed under either of:

- Apache License, Version 2.0 ([`LICENSE-APACHE`](LICENSE-APACHE))
- MIT License ([`LICENSE-MIT`](LICENSE-MIT))

at your option.
