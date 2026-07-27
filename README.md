# carpe

Multiplexes `CARGO_TARGET_DIR` across git worktrees, so several worktrees of
the same repo can share a small pool of pre-built target directories instead
of each maintaining (and rebuilding) its own — while still letting you build
in two worktrees at once instead of blocking on cargo's own build-dir lock.

```
carpe <cargo-args...>      # e.g. `carpe build`, `carpe test --release`, `carpe check`
carpe status                # show the slots for the current repo and whether they're busy
carpe prune                 # clean up target slots for the current repo not held by any build
carpe prune -i              # interactive menu to choose which target slots to delete
carpe prune --lru <num>     # keep the <num> newest unlocked target slots and prune older ones
carpe prune -a              # prune unlocked target slots across all repositories under ~/.cache/carpe
```

Everything after `carpe` is passed straight through to `cargo`, unmodified.
The only thing `carpe` does is pick a `CARGO_TARGET_DIR` and set it before
exec'ing `cargo`.

## How slot selection works

1. **Identify the repo.** `git rev-parse --git-common-dir` gives the same
   answer in every worktree of a given repo, so it's used as the pool's
   identity — that's the "detect if we're in the same worktree or not" part.
2. **Identify this worktree.** `git rev-parse --git-dir` gives a path that's
   *different per worktree* (each linked worktree has its own private git
   dir under `<main>/.git/worktrees/<name>`). That's where carpe stores a
   one-line marker recording which slot this worktree used last time —
   nothing in `~/.cache` needs to track worktrees itself, and when you
   `git worktree remove` a worktree, its marker disappears with it. No
   central state file, no stale entries to garbage-collect.
3. **Prefer the same slot as last time**, because reusing a target dir is
   what makes incremental compilation actually pay off. Before using it,
   carpe takes a non-blocking exclusive lock on `<slot>/.carpe-lock`.
   - Lock acquired → use this slot, done.
   - Lock contended (another `carpe`/build already running against it,
     from this worktree or another) → try the next existing slot for this
     repo, in order.
   - All existing slots busy → create a new one (`<pool>-N+1`) and use that.
     This is what lets two worktrees actually build in parallel instead of
     one blocking behind the other.
4. The chosen slot's lock is held by the `carpe` process for the entire
   `cargo` invocation, then released automatically when `carpe` exits (the
   OS releases `flock`/`LockFileEx` locks on process exit or crash, so
   there's never a stale lock file to clean up by hand).

### Why not just check cargo's own `.cargo-lock`?

Cargo already takes a lock on its target directory (that's what "Blocking
waiting for file lock on build directory" is). It would be simpler for
carpe to just check that lock rather than keeping one of its own — but that
file actually lives *inside* the profile subdirectory
(`target/debug/.cargo-lock`, `target/release/.cargo-lock`, a target-triple
subdir for cross builds, etc.), which cargo's own docs describe as an
"internal implementation detail... we can change this if needed." It also
won't exist yet on a brand-new slot before the first build. Rather than
parse cargo args to guess the right profile path and hope the layout
doesn't shift under us, carpe just takes its own lock at the slot root — same
underlying mechanism (`flock` / `LockFileEx`), fully under carpe's control.

### Naming

Slots live at `~/.cache/carpe/<pool>-<n>`, e.g. `~/.cache/carpe/myrepo-a1b2c3d4-0`.
`<pool>` is `<basename-of-repo>-<8-hex-char-hash-of-the-git-common-dir-path>` —
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
pool: myrepo-a1b2c3d4
root: /home/you/.cache/carpe
  myrepo-a1b2c3d4-0  [busy]  <- preferred for this worktree
  myrepo-a1b2c3d4-1  [free]
```

## Build & install

`carpe` uses [`fd-lock`](https://crates.io/crates/fd-lock) for cross-platform advisory file locking (`flock` on Unix, `LockFileEx` on Windows).

```
cargo build --release
install -Dm755 target/release/carpe ~/.local/bin/carpe   # or wherever's on your PATH
```

## Caveats / things you may want to tweak

- **Cross-worktree cache correctness is your call.** Sharing a target dir
  across worktrees is safe from cargo's perspective (it fingerprints by
  source content, not by which worktree built it) but if two worktrees are
  on very different branches, you'll pay for a lot of incremental
  recompilation the first time you switch — carpe doesn't try to be
  clever about this, it just gives you a pool to reuse when it *does* help.
- **NFS / network filesystems:** like cargo itself, `flock` doesn't work
  reliably on some network filesystems. If your `~/.cache` is on one of
  these, locking may silently not provide real exclusion.
- **Pruning old slots:** Target directories accumulate over time. Use `carpe prune` to safely delete unlocked slots, `carpe prune --lru <num>` to keep only the `<num>` newest slots, or `carpe prune -i` for an interactive selection prompt. Built-in locking ensures active build directories are never deleted.
- **Windows support is best-effort** (`LockFileEx`-based) and less tested
  than the Unix `flock` path.
- If you pass an explicit `--target-dir` yourself, carpe doesn't currently
  detect that and will still set `CARGO_TARGET_DIR` — cargo's own
  precedence rules mean your explicit flag wins, but it's a little
  redundant. Worth special-casing if it bugs you.