//! carpe: multiplex cargo target dirs across git worktrees.
//!
//! `carpe <cargo-args...>` picks a `CARGO_TARGET_DIR` from a small pool of
//! directories under `~/.cache/carpe/`, shared by every worktree of the
//! same repo, then execs `cargo` with that env var set.
//!
//! Selection rules:
//!   1. Identify the repo by its git *common* dir (shared across all
//!      worktrees) and the current worktree by its own *private* git dir.
//!   2. Each worktree remembers which slot it used last time (stored inside
//!      that worktree's own private git dir, so it's cleaned up
//!      automatically by `git worktree remove`), and prefers it next time —
//!      reusing a slot means incremental compilation actually helps.
//!   3. Before use, a slot must be lockable (see lockfile.rs). If the
//!      preferred slot is busy (another build running against it, in this
//!      or another worktree), carpe falls through to any other free slot,
//!      then creates a new one if all existing slots are busy. This is what
//!      lets two worktrees build in parallel instead of blocking on cargo's
//!      own lock.

mod lockfile;

use std::collections::hash_map::DefaultHasher;
use std::env;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        None | Some("-h") | Some("--help") => {
            print_usage();
            std::process::exit(if args.is_empty() { 2 } else { 0 });
        }
        Some("status") => {
            run_status();
        }
        _ => {
            run_cargo(&args);
        }
    }
}

fn print_usage() {
    eprintln!("carpe: multiplex cargo target dirs across git worktrees");
    eprintln!();
    eprintln!("usage:");
    eprintln!("  carpe <cargo-args...>   run cargo with a coordinated CARGO_TARGET_DIR");
    eprintln!("  carpe status            show target-dir slots for the current repo");
}

/// Identifies the repo-wide pool and this worktree's private marker location.
struct RepoIdentity {
    /// Directory whose basename seeds the pool name (main worktree root, or
    /// cwd itself outside of git).
    name_source: PathBuf,
    /// Stable, canonicalized identity for the whole repo (same value in
    /// every worktree of the same repo). Used only for hashing.
    repo_identity: PathBuf,
    /// Where this *worktree's* "preferred slot" marker is stored. For git
    /// repos this is the worktree's own private git-dir (e.g.
    /// `.git/worktrees/<name>` for a linked worktree, or `.git` for the
    /// main one), which git deletes on `git worktree remove` -- so stale
    /// markers can't accumulate. Outside of git, falls back to a
    /// cwd-keyed directory under the carpe cache root.
    marker_dir: PathBuf,
}

impl RepoIdentity {
    fn detect(cwd: &Path) -> RepoIdentity {
        if let (Some(common_dir), Some(worktree_git_dir)) = (git_common_dir(cwd), git_dir(cwd)) {
            let common_dir = canonicalize_best_effort(&common_dir);
            // common_dir is normally "<repo>/.git" (or the bare repo dir
            // itself). Its parent is the shared, worktree-independent
            // project root we want to name the pool after.
            let name_source = common_dir
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| common_dir.clone());
            let marker_dir = canonicalize_best_effort(&worktree_git_dir);
            RepoIdentity {
                name_source,
                repo_identity: common_dir,
                marker_dir,
            }
        } else {
            let canon = canonicalize_best_effort(cwd);
            let marker_dir = cache_root()
                .join("state")
                .join(short_hash(&canon.to_string_lossy()));
            RepoIdentity {
                name_source: canon.clone(),
                repo_identity: canon,
                marker_dir,
            }
        }
    }

    /// The `{directory-name}` prefix slots are named after, e.g.
    /// `myrepo-a1b2c3d4`. The hash suffix disambiguates same-named repos
    /// that live in different locations (a plain basename alone would
    /// collide for anyone with two directories called `api` or `server`).
    fn pool_name(&self) -> String {
        let base = self
            .name_source
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "project".to_string());
        format!(
            "{}-{}",
            sanitize(&base),
            short_hash(&self.repo_identity.to_string_lossy())
        )
    }

    fn marker_path(&self) -> PathBuf {
        self.marker_dir.join("carpe-slot")
    }

    fn read_preferred_slot(&self) -> Option<usize> {
        fs::read_to_string(self.marker_path())
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    fn write_preferred_slot(&self, idx: usize) {
        if fs::create_dir_all(&self.marker_dir).is_ok() {
            let _ = fs::write(self.marker_path(), idx.to_string());
        }
    }
}

fn run_cargo(args: &[String]) {
    let cwd = env::current_dir().expect("carpe: cannot read current directory");
    let identity = RepoIdentity::detect(&cwd);
    let root = cache_root();
    fs::create_dir_all(&root).expect("carpe: cannot create ~/.cache/carpe");

    let dir_name = identity.pool_name();
    let preferred = identity.read_preferred_slot();

    let mut candidates = existing_slot_indices(&root, &dir_name);
    candidates.sort_unstable();
    if let Some(p) = preferred {
        if let Some(pos) = candidates.iter().position(|&i| i == p) {
            candidates.remove(pos);
            candidates.insert(0, p);
        }
    }

    let mut chosen: Option<(usize, PathBuf, lockfile::Lock)> = None;
    for idx in &candidates {
        let slot_path = root.join(format!("{dir_name}-{idx}"));
        let lock_path = slot_path.join(".carpe-lock");
        match lockfile::try_lock(&lock_path) {
            Ok(Some(lock)) => {
                chosen = Some((*idx, slot_path, lock));
                break;
            }
            Ok(None) => continue, // busy, try the next candidate
            Err(e) => {
                eprintln!(
                    "carpe: warning: couldn't lock {} ({e}), skipping",
                    lock_path.display()
                );
            }
        }
    }

    let (slot_idx, slot_path, _lock) = chosen.unwrap_or_else(|| {
        let next = candidates.iter().max().map_or(0, |m| m + 1);
        let slot_path = root.join(format!("{dir_name}-{next}"));
        fs::create_dir_all(&slot_path).expect("carpe: cannot create new target-dir slot");
        let lock_path = slot_path.join(".carpe-lock");
        let lock = lockfile::try_lock(&lock_path)
            .expect("carpe: I/O error locking new slot")
            .expect("carpe: brand-new slot was unexpectedly already locked");
        (next, slot_path, lock)
    });

    if preferred != Some(slot_idx) {
        identity.write_preferred_slot(slot_idx);
    }

    match preferred {
        Some(p) if p != slot_idx => eprintln!(
            "carpe: preferred slot {dir_name}-{p} is busy, using {dir_name}-{slot_idx} instead (parallel build)"
        ),
        _ => eprintln!("carpe: using {}", slot_path.display()),
    }

    let status = Command::new("cargo")
        .args(args)
        .env("CARGO_TARGET_DIR", &slot_path)
        .status()
        .expect("carpe: failed to spawn cargo (is it on your PATH?)");

    // _lock is still held here, for the whole duration of the cargo run.
    std::process::exit(status.code().unwrap_or(1));
}

fn run_status() {
    let cwd = env::current_dir().expect("carpe: cannot read current directory");
    let identity = RepoIdentity::detect(&cwd);
    let root = cache_root();
    let dir_name = identity.pool_name();
    let preferred = identity.read_preferred_slot();

    let mut slots = existing_slot_indices(&root, &dir_name);
    slots.sort_unstable();

    println!("pool: {dir_name}");
    println!("root: {}", root.display());
    if slots.is_empty() {
        println!("  (no target-dir slots built yet)");
        return;
    }
    for idx in slots {
        let slot_path = root.join(format!("{dir_name}-{idx}"));
        let lock_path = slot_path.join(".carpe-lock");
        let state = match lockfile::try_lock(&lock_path) {
            Ok(Some(_)) => "free", // lock dropped immediately at end of this arm's scope
            Ok(None) => "busy",
            Err(_) => "unknown",
        };
        let marker = if Some(idx) == preferred {
            "  <- preferred for this worktree"
        } else {
            ""
        };
        println!("  {dir_name}-{idx}  [{state}]{marker}");
    }
}

fn existing_slot_indices(root: &Path, dir_name: &str) -> Vec<usize> {
    let prefix = format!("{dir_name}-");
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.flatten() {
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(rest) = name.strip_prefix(&prefix) {
                if let Ok(n) = rest.parse::<usize>() {
                    out.push(n);
                }
            }
        }
    }
    out
}

fn git_common_dir(cwd: &Path) -> Option<PathBuf> {
    run_git(cwd, &["rev-parse", "--git-common-dir"]).map(|s| resolve(cwd, &s))
}

fn git_dir(cwd: &Path) -> Option<PathBuf> {
    run_git(cwd, &["rev-parse", "--git-dir"]).map(|s| resolve(cwd, &s))
}

fn run_git(cwd: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(cwd).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout)
        .ok()
        .map(|s| s.trim().to_string())
}

fn resolve(cwd: &Path, raw: &str) -> PathBuf {
    let p = PathBuf::from(raw);
    if p.is_absolute() {
        p
    } else {
        cwd.join(p)
    }
}

fn canonicalize_best_effort(p: &Path) -> PathBuf {
    fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

fn cache_root() -> PathBuf {
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .expect("carpe: $HOME is not set");
    home.join(".cache").join("carpe")
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

fn short_hash(s: &str) -> String {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    format!("{:016x}", h.finish())[..8].to_string()
}