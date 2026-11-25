# Architecture

`carpe` is a single binary (no library crate). All logic lives in the modules
below, declared in [`src/main.rs`](../src/main.rs).

| Module | Responsibility |
| --- | --- |
| `src/main.rs` | Argument dispatch, usage/version, `status` and `info` rendering, setting `CARGO_TARGET_DIR`, exec'ing cargo |
| `src/repo.rs` | Git/workspace identity detection, pool naming, `carpe-slot` marker I/O |
| `src/slots.rs` | Slot discovery, affinity ranking, allocation, `status`/`prune` slot listing |
| `src/meta.rs` | Git/build state extraction, `.carpe-meta` read/write, affinity score |
| `src/lockfile.rs` | Non-blocking advisory whole-file locking via `fd-lock` |
| `src/prune.rs` | Prune modes (default, interactive, LRU, auto), deletion with re-locking |
| `src/utils.rs` | Cache paths, argument normalization, cargo/rustup resolution, shim installation, warnings, formatting helpers |

## Repo and workspace detection

[`RepoIdentity::detect`](../src/repo.rs) has two paths:

1. **Inside git:** runs `git rev-parse --git-common-dir --git-dir`. Relative
   results are resolved against the current directory and canonicalized.
   - `repo_identity` = canonicalized `--git-common-dir`, identical in every
     worktree of a repo, so all worktrees share one pool.
   - `name_source` = parent of the common git dir (normally the main worktree
     root), used for the human-readable name prefix.
   - `marker_dir` = canonicalized `--git-dir`, which is *different* for each
     linked worktree (typically `<common-git-dir>/worktrees/<name>`).
2. **Outside git:** walks from the current directory toward `/`, recording the
   topmost directory containing a `Cargo.toml`, and promoting any directory
   whose manifest text contains the literal string `[workspace]`. If no
   manifest is found, the canonicalized current directory is used. This root
   serves as both identity and name source; the marker directory becomes
   `<cache-root>/state/<8-hex-hash-of-root>/`.

`pool_name()` is `<sanitized-basename>-<8-hex-hash>`:

- `basename` is `name_source.file_name()` (or `project` if unavailable).
- `sanitize` keeps ASCII alphanumerics, `-`, and `_`, replacing every other
  character with `-`.
- The hash is the first 8 hex digits of `DefaultHasher` applied to the
  canonicalized identity path string. It only prevents collisions between
  unrelated repos with the same directory name; it is not a portable/stable
  hash format across Rust releases.

Slots are directories named `<pool>-<index>` inside the cache root. The cache
root is `dirs::cache_dir()/carpe`: `$XDG_CACHE_HOME/carpe` on Linux/BSD,
`~/Library/Caches/carpe` on macOS, `%LOCALAPPDATA%\carpe` on Windows, with the
system temp dir as a last-resort fallback.

## Markers and slot metadata

**`<marker-dir>/carpe-slot`** contains one decimal slot index (for example
`1`) with no trailing newline. `status` and `info` display it as `Preferred`.
Because it is stored in the worktree's private git dir, `git worktree remove`
removes it automatically; outside git it lives under the cache root's `state/`
directory.

**`<slot>/.carpe-meta`** is rewritten on every selection with `key=value` lines
that are present only when the value exists:

```
head=<git rev-parse HEAD>
branch=<abbrev-ref HEAD, omitted when detached>
toolchain=<leading +toolchain argument>
profile=<--release/-r/--profile value, or "debug">
target=<--target value>
features=<--features value>
timestamp=<unix seconds>
```

`timestamp` is used both as the affinity tie-breaker and as the slot's
"modified" time in `status`/`info`/`prune` (falling back to the slot directory's
mtime when the file is absent).

## Affinity scoring and fallback allocation

[`select_slot`](../src/slots.rs) discovers `<pool>-<n>` directories, reads each
`.carpe-meta` (missing file = default metadata with timestamp `0`), and scores
it against the current git/build state using
[`SlotMeta::score`](../src/meta.rs):

| Signal | Points |
| --- | --- |
| `head` equal (both present) | `+100` |
| Preferred slot for this worktree | `+75` (added in `slots.rs`, not part of `SlotMeta::score`) |
| `toolchain` equal | `+50` |
| `profile` equal | `+40` |
| `target` equal | `+40` |
| `features` equal | `+40` |
| `branch` equal (detached `HEAD` has no branch) | `+20` |

Sort order is **score descending → timestamp descending → slot index
ascending**. Candidates are tried in that order, skipping slots that cannot be
locked. If every candidate is busy (or none exist), a new slot is allocated at
`max(existing indices) + 1` (or `0` for an empty pool), created on disk, and
locked. The chosen slot becomes the new preferred slot and its `.carpe-meta` is
rewritten. Branch matching is deliberately excluded for detached `HEAD`
(`GitState::detect` filters the literal branch value `HEAD`).

## Locking

Each slot has a `.carpe-lock` file. [`try_lock`](../src/lockfile.rs) opens it
with `create(true)` and `truncate(false)`, takes a non-blocking exclusive write
lock with `fd-lock`, and returns `Ok(Some(Lock))`, `Ok(None)` (busy), or an I/O
error. `Lock` owns both the boxed `RwLock` and its write guard, so the lock is
held until the guard is dropped. `select_slot` returns the guard to
`run_cargo`, which keeps it alive for the whole invocation; on Unix cargo
`exec`s in place, so the OS releases the lock when the process exits. `status`
and `prune` use the same primitive to classify slots as busy, and `prune`
re-locks immediately before deleting a slot directory.

## Real cargo resolution and the override shim

Every cargo launch goes through [`real_cargo_cmd`](../src/utils.rs), which
resolves the executable in this order:

1. `$CARPE_CARGO_PATH`, if set.
2. `cargo.real` (`cargo.real.exe`) in the cargo bin dir, when it exists.
3. `rustup +<toolchain> cargo` if the arguments start with `+toolchain` and
   `rustup` is on `PATH`.
4. The path printed by `rustup which cargo`, if successful and different from
   carpe's own executable.
5. The first `cargo` (`cargo.exe`) on `PATH` whose canonical path differs from
   carpe's own executable.
6. The literal command `cargo` (resolved by `PATH`).

Every returned command gets `_CARPE_RECURSION_GUARD=1` in its environment.
Currently no code in `src/` reads that variable; recursion prevention comes
from the shim renaming the original `cargo` to `cargo.real` and from step 2
preferring that backup.

`carpe toggle-override` (alias `carpe override`) installs or removes the shim:

- **Install:** rename an existing cargo binary to `cargo.real`, then write a
  shim at the original path. On Unix it is
  `#!/bin/sh` / `# carpe-shim` / `exec "<carpe-exe>" "$@"`, mode `0755`; on
  Windows a `.cmd` containing `rem carpe-shim` is written too. The shim forwards
  all arguments to carpe, whose `real_cargo_cmd` then finds `cargo.real`.
- **Remove:** rename `cargo.real` back to `cargo`, or delete the shim if no
  backup exists.
- **Detection:** the shim file contains `# carpe-shim` or `rem carpe-shim`, or
  is a symlink whose target contains `carpe`.

The cargo bin dir comes from `$CARGO_HOME/bin` when `CARGO_HOME` is non-empty,
otherwise `~/.cargo/bin` if it exists, otherwise the first `PATH` directory
containing a `cargo` executable, otherwise `~/.cargo/bin`.

## Pass-through details

[`normalize_args`](../src/utils.rs) splits a single quoted argument using
`shell-words`, and separates merged `--features `, `--package `, `--target `,
`--profile `, and `--manifest-path ` tokens into flag/value pairs. Repo
detection uses the parent of `--manifest-path` when present; `status`, `info`,
and `prune` always use the current directory. Cargo subcommands that do not
build (`fmt`, `add`, `remove`, `rm`, `metadata`, `tree`, `new`, `init`,
`publish`, `search`, `login`, `logout`, `owner`, `vendor`, `yank`) are forwarded
directly, bypassing slots, markers, and warnings. See the
[CLI reference](cli-reference.md) for the complete command list, warnings,
environment variables, and exit codes.
