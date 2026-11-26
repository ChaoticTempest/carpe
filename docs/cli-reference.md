# CLI reference

This reference describes the behavior implemented in
[`src/main.rs`](../src/main.rs), [`src/utils.rs`](../src/utils.rs), and
[`src/prune.rs`](../src/prune.rs).

## Invocation and argument normalization

```
carpe <cargo-args...>
carpe status
carpe info
carpe toggle-override | carpe override
carpe prune [options]
carpe -h | carpe --help
carpe -V | carpe --version
```

Before dispatch, `carpe`:

1. Drops a leading literal `carpe` argument (arguments passed by Cargo-style
   subcommand invocation).
2. Calls `normalize_args`: if there is exactly **one** argument containing a
   space, it is split with shell-like quoting rules (`shell-words`).
3. Splits single tokens of the form `--features …`, `--package …`,
   `--target …`, `--profile …`, and `--manifest-path …` (flag followed by a
   space) into two separate arguments. Other tokens are unchanged.

Dispatch then looks only at the **first** normalized argument. Anything that is
not a built-in subcommand, `-h`, or `-V` is treated as Cargo arguments, even if
a later token happens to be a subcommand name.

## `carpe <cargo-args...>` (default)

Runs Cargo with a coordinated `CARGO_TARGET_DIR`:

- Resolves the base directory from `--manifest-path` (or `--manifest-path=…`)
  by taking its parent; otherwise uses the current directory. This determines
  workspace/repo detection outside git.
- Detects the repo or workspace, selects and locks a slot from the bounded pool
  of indices `0..16` (see
  [architecture](architecture.md#affinity-scoring-and-fallback-allocation)), prints which
  slot is in use, then execs the real cargo with `CARGO_TARGET_DIR` set to that
  slot. If none of the 16 slots can be locked, carpe reports
  `carpe: no free slot after 16 attempts` and exits `1` without running cargo.
- On Unix carpe `exec`s cargo, so cargo's exit code and signals are the
  process's own. On Windows carpe waits and exits with the child's code, or
  `1` if there is no code.

**Non-building subcommands bypass slot selection entirely.** After removing a
leading `+toolchain`, if the first argument is one of `fmt`, `add`, `remove`,
`rm`, `metadata`, `tree`, `new`, `init`, `publish`, `search`, `login`, `logout`,
`owner`, `vendor`, or `yank`, it is forwarded directly to the real cargo with
no slot, no marker update, and no warnings. Everything else (`build`, `test`,
`check`, `clippy`, `run`, ...) goes through slot selection.

## `carpe status`

Prints the pool name, the marker path, the preferred slot (or `none`), and all
slots in the current pool with size, busy state, and last-modified time, plus
the total pool size. If the cache root has not been created yet, or the pool
has no slots, it prints a short `(none exist …)` message instead. A storage
budget warning is printed when the cache across **all** pools exceeds
`CARPE_MAX_STORAGE_GB`. `status` uses the current directory, not
`--manifest-path`.

## `carpe info`

Prints `Carpe System & Workspace Info:` followed by: current directory,
workspace root (the repo/workspace name source), pool name, cache root, marker
path, preferred slot, git common dir (or `(none - non-git cargo workspace)`),
and git worktree dir when in git. It then reports slot counts (free/busy),
total pool size, and the same storage budget warning as `status`. Like
`status`, it uses the current directory.

## `carpe toggle-override` / `carpe override`

Both spellings run the same shim installer:

- The shim location is `<cargo-bin-dir>/cargo` (`cargo.exe` on Windows), where
  `<cargo-bin-dir>` is `$CARGO_HOME/bin` when `CARGO_HOME` is non-empty, else
  `~/.cargo/bin` if it exists, else the first `PATH` entry containing a `cargo`
  executable, else `~/.cargo/bin`.
- **Enable:** if a non-carpe `cargo` exists it is renamed to `cargo.real`
  (`cargo.real.exe` on Windows); then a shim is written. On Unix this is a
  `#!/bin/sh` script containing `# carpe-shim` that `exec`s the current carpe
  executable with all arguments, mode `0755`. On Windows a `.cmd` file
  containing `rem carpe-shim` is written alongside the shim path. Prints
  `enabling cargo=carpe`.
- **Disable:** if `cargo.real` exists it is renamed back to `cargo`; otherwise
  the shim is removed. Prints `disabling cargo=carpe`.
- Override state is detected by looking for `# carpe-shim` / `rem carpe-shim`
  in the shim file, or `carpe` in a symlink target.

While the override is installed, [`real_cargo_cmd`](../src/utils.rs) prefers
`cargo.real`, so the shim does not recurse. Every real cargo command it spawns
also receives `_CARPE_RECURSION_GUARD=1` in its environment; no branch in
`src/` currently reads that variable.

## `carpe prune [options]`

| Option | Behavior |
| --- | --- |
| *(none)* | Prune every unlocked slot in the current repo's pool |
| `-i`, `--interactive` | Multi-select prompt; slots that are locked or preferred are unchecked by default |
| `-n`, `--dry-run` | Print what would be removed without deleting anything (works with all modes) |
| `--auto` | Treat **all pools** as candidates; prune oldest first until total size is within `CARPE_MAX_STORAGE_GB` |
| `--lru <num>` | Keep the `<num>` newest **unlocked** slots in the selected pool(s); prune older ones |
| `-a`, `--all` | Select slots across all pools instead of only the current repo's pool |

If multiple modes are supplied, precedence is interactive, then `--auto`, then
`--lru`, then the default prune. Locked slots are always skipped; just before
deletion a slot's `.carpe-lock` is re-locked to avoid races, and the entire
slot directory is removed. If the cache root does not exist, `prune` prints
`No carpe cache directory found.`; if no candidate slots exist, it prints
`No target slots found.`

`-i` prints an error and returns without pruning when `is_ci_environment()` is
true or stdout is not a terminal. `--lru` requires a non-negative integer;
missing or invalid values, and unknown options, are errors (exit `1`).

## `carpe -h` / `carpe --help`

Prints the usage summary to stderr and exits `0`. Running `carpe` with **no
arguments** prints the same usage and exits `2`.

## `carpe -V` / `carpe --version`

Prints `carpe 0.1.0` to stdout.

## Warnings and messages

| Condition | Message / behavior |
| --- | --- |
| `CARGO_TARGET_DIR` or `CARGO_BUILD_TARGET_DIR` is set and non-empty | `Warning target directory is set in environment (…), overriding with carpe slot …`; carpe overrides it for cargo |
| `--target-dir` or `--target-dir=…` passed | `Warning explicit --target-dir flag passed in arguments; Cargo CLI flag will take precedence over carpe slot …` |
| Preferred slot is busy | `Carpe preferred slot <pool>-<n> is busy, using <pool>-<m> instead (parallel build)` |
| All 16 bounded slots are contended | `carpe: no free slot after 16 attempts`; no slot `16` is created, cargo is not run, and exit status is `1` |
| I/O error locking a slot | `carpe: warning: couldn't lock <path> (…), skipping`; the next ranked candidate is tried |
| Cache exceeds `CARPE_MAX_STORAGE_GB` | `carpe: warning: total cache size across all pools is … (exceeds … GB storage budget)`, with a suggestion to run `carpe prune -a` or `carpe prune --lru 2` |
| Interactive prune in CI or a non-terminal | `carpe: interactive prompt disabled in non-interactive / CI environment.`; no pruning occurs |
| Failed slot directory removal | `Failed to remove <path>` on stderr |

## Environment variables

| Variable | Meaning |
| --- | --- |
| `CARPE_MAX_STORAGE_GB` | Storage budget in GiB (floating point), default `128.0`; used by `prune --auto` and the `status`/`info` budget warning |
| `CARPE_CARGO_PATH` | Explicit real-cargo executable path; highest precedence in [`real_cargo_cmd`](../src/utils.rs) |
| `CARPE_CI` | If `true` or `1`, treated as CI |
| `CI` | If `true` or `1`, treated as CI |
| `CONTINUOUS_INTEGRATION` | Any value (including empty) marks CI |
| `CARGO_HOME` | Determines the bin dir used by `toggle-override` when non-empty |
| `CARGO_TARGET_DIR` | If non-empty, warns and is overridden with the selected slot |
| `CARGO_BUILD_TARGET_DIR` | Same as above (checked when `CARGO_TARGET_DIR` is absent) |
| `XDG_CACHE_HOME` | Read by the `dirs` crate on Linux/BSD to locate `<dir>/carpe`; not used on macOS or Windows |
| `LOCALAPPDATA` | Read by the `dirs` crate on Windows to locate `<dir>\carpe` |
| `PATH` | Searched for `cargo`, `cargo.real`'s bin dir fallback, and `rustup` |
| `_CARPE_RECURSION_GUARD` | Always set to `1` on child cargo commands; informational in the current code |

## Exit codes

| Code | When |
| --- | --- |
| `0` | Successful `status`, `info`, `toggle-override`, `prune`, help/version; Cargo's success when carpe execs it |
| `1` | Prune usage errors (unknown option, missing/invalid `--lru`), slot allocation exhaustion (`carpe: no free slot after 16 attempts`), failure to exec cargo, or (off Unix) a child with no exit code |
| `2` | `carpe` invoked with no arguments |
| cargo's own | On Unix cargo replaces the carpe process via `exec`, so its exit status/signal is returned directly |

Slot allocation failures are reported cleanly via `select_slot`'s `io::Result`
with the message above and exit `1`. Other unexpected internal I/O failures
(for example, an unreadable current directory) still panic rather than using
the codes above.
