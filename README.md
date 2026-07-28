# carpe

Multiplexes `CARGO_TARGET_DIR` across git worktrees, so several worktrees of
the same repo can share a small pool of pre-built target directories instead
of each maintaining (and rebuilding) its own — while still letting you build
in two worktrees at once instead of blocking on cargo's own build-dir lock.

```
carpe <cargo-args...>      # e.g. `carpe build`, `carpe test --release`, `carpe check`
carpe status                # show target-dir slots for the current repo and whether they're busy
carpe info                  # display detailed system, workspace root, pool, and cache diagnostics
carpe prune                 # clean up target slots for the current repo not held by any build
carpe prune -n / --dry-run  # preview target slots to prune without deleting files
carpe prune -i              # interactive menu to choose which target slots to delete
carpe prune --lru <num>     # keep the <num> newest unlocked target slots and prune older ones
carpe prune -a              # prune unlocked target slots across all repositories under ~/.cache/carpe
```

Everything after `carpe` is passed straight through to `cargo`, unmodified.
The only thing `carpe` does is pick a `CARGO_TARGET_DIR` and set it before
exec'ing `cargo`.

## How slot selection works

1. **Identify the repo or workspace.** `git rev-parse --git-common-dir` gives the same
   answer in every worktree of a given repo, so it's used as the pool's
   identity. Outside of Git repositories, `carpe` automatically traverses parent directories
   to locate the top-level Cargo workspace root (`Cargo.toml`).
2. **Identify this worktree.** `git rev-parse --git-dir` gives a path that's
   *different per worktree* (each linked worktree has its own private git
   dir under `<main>/.git/worktrees/<name>`). That's where carpe stores a
   one-line marker recording which slot this worktree used last time —
   nothing in `~/.cache` needs to track worktrees itself, and when you
   `git worktree remove` a worktree, its marker disappears with it. No
   central state file, no stale entries to garbage-collect.
3. **Smart Affinity Scoring.** Reusing a target dir is what makes incremental compilation pay off.
   When searching free slots, `carpe` scores slots using a multi-dimensional affinity algorithm:
   - **Preferred Worktree Slot**: Selected immediately if free.
   - **Git Commit Match (`head`)**: +100 points
   - **Cargo Profile Match (`debug`/`release`/custom)**: +40 points
   - **Target Triple Match (`--target`)**: +40 points
   - **Git Branch Match (`branch`)**: +30 points
   - **Fallback**: If all candidate slots are locked, a new slot (`<pool>-N+1`) is created.
4. **Advisory Locking.** The chosen slot's lock is held by the `carpe` process for the entire
   `cargo` invocation using [`fd-lock`](https://crates.io/crates/fd-lock) (`flock` on Unix, `LockFileEx` on Windows), then released automatically on exit.

### Environment & Flag Warnings

- If `$CARGO_TARGET_DIR` is set in your environment, `carpe` prints a warning and overrides it with the selected slot.
- If `--target-dir` is explicitly passed in Cargo CLI arguments, `carpe` warns that Cargo's explicit CLI flag will take precedence over the slot.

### Naming

Slots live under standard platform cache directories via [`dirs`](https://crates.io/crates/dirs):
- **Linux/BSD**: `~/.cache/carpe/<pool>-<n>`
- **macOS**: `~/Library/Caches/carpe/<pool>-<n>` (or `$XDG_CACHE_HOME/carpe/`)
- **Windows**: `%LOCALAPPDATA%\carpe\<pool>-<n>`

`<pool>` is `<basename-of-repo>-<8-hex-char-hash-of-the-identity-path>` —
the hash exists purely to keep two unrelated repos that happen to share a
directory name (very common: `api`, `server`, `worker`, ...) from colliding.

## Example: two worktrees, one busy build

```
$ cd ~/src/myrepo               # main worktree
$ carpe build
carpe: using /home/you/.cache/carpe/myrepo-a1b2c3d4-0
   Compiling myrepo v0.1.0
   ...

# meanwhile, in another terminal:
$ cd ~/src/myrepo-feature-x     # linked worktree, first time
$ carpe build
carpe: using /home/you/.cache/carpe/myrepo-a1b2c3d4-1
   Compiling myrepo v0.1.0
   ...

# next time you build in myrepo-feature-x, slot 1 is free and preferred, so it's reused:
$ carpe build
carpe: using /home/you/.cache/carpe/myrepo-a1b2c3d4-1
```

```
$ carpe status
Pool:      myrepo-a1b2c3d4
Marker:    /home/you/src/myrepo/.git/carpe-slot
Preferred: 0

Slots:
  myrepo-a1b2c3d4-0         1.2 GB     [free, preferred] (modified: just now)
  myrepo-a1b2c3d4-1         850.5 MB   [free]            (modified: 10m ago)

Total pool size: 2.05 GB
```

## Build & install

`carpe` is published on crates.io and can be installed via Cargo:

```bash
cargo install carpe
```

Or install from local source:

```bash
cargo install --path .
```

## License

Dual-licensed under either of:

- Apache License, Version 2.0 ([`LICENSE-APACHE`](LICENSE-APACHE))
- MIT License ([`LICENSE-MIT`](LICENSE-MIT))

at your option.