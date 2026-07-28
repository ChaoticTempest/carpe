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

use dialoguer::{theme::ColorfulTheme, MultiSelect};

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        None | Some("-h") | Some("--help") => {
            print_usage();
            std::process::exit(if args.is_empty() { 2 } else { 0 });
        }
        Some("-V") | Some("--version") => {
            println!("carpe 0.1.0");
        }
        Some("status") => {
            run_status();
        }
        Some("prune") => {
            run_prune(&args[1..]);
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
    eprintln!("  carpe <cargo-args...>      run cargo with a coordinated CARGO_TARGET_DIR");
    eprintln!("  carpe status               show target-dir slots for the current repo");
    eprintln!("  carpe prune [options]      remove target-dir slots not held by any cargo process");
    eprintln!();
    eprintln!("prune options:");
    eprintln!("  -i, --interactive          choose interactively which slots to prune");
    eprintln!(
        "  --lru <num>                keep the <num> newest unlocked slots and prune older ones"
    );
    eprintln!(
        "  -a, --all                  prune across all repository pools under ~/.cache/carpe"
    );
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

    let (slot_idx, slot_path, _lock) = select_slot(&identity, &root);

    if let Some(existing_env) = env::var_os("CARGO_TARGET_DIR").filter(|s| !s.is_empty()) {
        eprintln!(
            "carpe: warning: CARGO_TARGET_DIR is set in environment ({}), overriding with carpe slot {}",
            existing_env.to_string_lossy(),
            slot_path.display()
        );
    }

    if has_explicit_target_dir_flag(args) {
        eprintln!(
            "carpe: warning: explicit --target-dir flag passed in arguments; Cargo CLI flag will take precedence over carpe slot {}",
            slot_path.display()
        );
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
        .expect("carpe: failed to spawn cargo (is it in PATH?)");

    // _lock is still held here, for the whole duration of the cargo run.
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(code) = status.code() {
            std::process::exit(code);
        } else if let Some(signal) = status.signal() {
            std::process::exit(128 + signal);
        } else {
            std::process::exit(1);
        }
    }

    #[cfg(not(unix))]
    {
        std::process::exit(status.code().unwrap_or(1));
    }
}

fn select_slot(identity: &RepoIdentity, root: &Path) -> (usize, PathBuf, lockfile::Lock) {
    let dir_name = identity.pool_name();
    let preferred = identity.read_preferred_slot();

    let mut candidates = existing_slot_indices(root, &dir_name);
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

    let (slot_idx, slot_path, lock) = chosen.unwrap_or_else(|| {
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

    (slot_idx, slot_path, lock)
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

#[derive(Clone)]
struct SlotCandidate {
    name: String,
    path: PathBuf,
    size_bytes: u64,
    mtime: std::time::SystemTime,
    is_busy: bool,
    is_preferred: bool,
}

fn run_prune(args: &[String]) {
    let mut interactive = false;
    let mut all_pools = false;
    let mut lru_keep: Option<usize> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-i" | "--interactive" => {
                interactive = true;
            }
            "-a" | "--all" => {
                all_pools = true;
            }
            "--lru" => {
                i += 1;
                if i < args.len() {
                    if let Ok(num) = args[i].parse::<usize>() {
                        lru_keep = Some(num);
                    } else {
                        eprintln!("carpe prune error: --lru requires a valid non-negative integer");
                        std::process::exit(1);
                    }
                } else {
                    eprintln!("carpe prune error: --lru requires an integer argument");
                    std::process::exit(1);
                }
            }
            arg if arg.starts_with("--lru=") => {
                let rest = &arg["--lru=".len()..];
                if let Ok(num) = rest.parse::<usize>() {
                    lru_keep = Some(num);
                } else {
                    eprintln!("carpe prune error: --lru requires a valid non-negative integer");
                    std::process::exit(1);
                }
            }
            "-h" | "--help" => {
                print_usage();
                return;
            }
            other => {
                eprintln!("carpe prune error: unknown argument '{other}'");
                print_usage();
                std::process::exit(1);
            }
        }
        i += 1;
    }

    let cwd = env::current_dir().expect("carpe: cannot read current directory");
    let identity = RepoIdentity::detect(&cwd);
    let root = cache_root();

    if !root.is_dir() {
        println!("No carpe cache directory found at {}.", root.display());
        return;
    }

    let current_pool = identity.pool_name();
    let preferred = identity.read_preferred_slot();

    let filter = if all_pools {
        None
    } else {
        Some(current_pool.as_str())
    };
    let mut slots = collect_slots(&root, filter, preferred);

    if slots.is_empty() {
        println!("No target-dir slots found to prune.");
        return;
    }

    if interactive {
        run_interactive_prune(&mut slots);
    } else if let Some(keep) = lru_keep {
        run_lru_prune(&mut slots, keep);
    } else {
        run_default_prune(&mut slots);
    }
}

fn collect_slots(
    root: &Path,
    filter_pool: Option<&str>,
    preferred: Option<usize>,
) -> Vec<SlotCandidate> {
    let mut candidates = Vec::new();
    let Ok(entries) = fs::read_dir(root) else {
        return candidates;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();

        if name == "state" {
            continue;
        }

        if let Some(pool) = filter_pool {
            if !name.starts_with(&format!("{pool}-")) {
                continue;
            }
        }

        let lock_path = path.join(".carpe-lock");
        let is_busy = !matches!(lockfile::try_lock(&lock_path), Ok(Some(_)));

        let size = dir_size(&path);
        let mtime = latest_mtime(&path);

        let is_preferred = if let Some(pool) = filter_pool {
            if let Some(pref_idx) = preferred {
                name == format!("{pool}-{pref_idx}")
            } else {
                false
            }
        } else {
            false
        };

        candidates.push(SlotCandidate {
            name: name.into_owned(),
            path,
            size_bytes: size,
            mtime,
            is_busy,
            is_preferred,
        });
    }

    candidates.sort_by(|a, b| a.name.cmp(&b.name));
    candidates
}

fn run_default_prune(slots: &mut [SlotCandidate]) {
    let mut pruned_count = 0;
    let mut total_reclaimed = 0u64;

    for slot in slots.iter() {
        if slot.is_busy {
            println!("Skipping {} (currently locked/busy)", slot.name);
            continue;
        }
        let lock_path = slot.path.join(".carpe-lock");
        match lockfile::try_lock(&lock_path) {
            Ok(Some(_lock)) => {
                let size = slot.size_bytes;
                if fs::remove_dir_all(&slot.path).is_ok() {
                    println!("Pruned {} ({})", slot.name, format_size(size));
                    pruned_count += 1;
                    total_reclaimed += size;
                } else {
                    eprintln!("Failed to remove {}", slot.path.display());
                }
            }
            _ => {
                println!("Skipping {} (busy)", slot.name);
            }
        }
    }

    if pruned_count == 0 {
        println!("No unlocked target-dir slots were found to prune.");
    } else {
        println!(
            "Reclaimed {} across {} target slot(s).",
            format_size(total_reclaimed),
            pruned_count
        );
    }
}

fn run_lru_prune(slots: &mut [SlotCandidate], keep_count: usize) {
    let free_slots: Vec<&SlotCandidate> = slots.iter().filter(|s| !s.is_busy).collect();

    if free_slots.len() <= keep_count {
        println!(
            "Only {} unlocked slot(s) exist; keeping all {} requested slot(s).",
            free_slots.len(),
            keep_count
        );
        return;
    }

    let mut sorted_free = free_slots;
    // Sort free slots by mtime descending (newest first)
    sorted_free.sort_by_key(|b| std::cmp::Reverse(b.mtime));

    let to_prune = &sorted_free[keep_count..];
    let mut pruned_count = 0;
    let mut total_reclaimed = 0u64;

    for slot in to_prune {
        let lock_path = slot.path.join(".carpe-lock");
        match lockfile::try_lock(&lock_path) {
            Ok(Some(_lock)) => {
                let size = slot.size_bytes;
                if fs::remove_dir_all(&slot.path).is_ok() {
                    println!("LRU Pruned {} ({})", slot.name, format_size(size));
                    pruned_count += 1;
                    total_reclaimed += size;
                } else {
                    eprintln!("Failed to remove {}", slot.path.display());
                }
            }
            _ => {
                println!("Skipping {} (busy)", slot.name);
            }
        }
    }

    println!(
        "LRU Prune complete. Kept {} newest slot(s), reclaimed {} across {} slot(s).",
        keep_count,
        format_size(total_reclaimed),
        pruned_count
    );
}

fn run_interactive_prune(slots: &mut [SlotCandidate]) {
    let free_slots: Vec<&SlotCandidate> = slots.iter().filter(|s| !s.is_busy).collect();

    if free_slots.is_empty() {
        println!("No unlocked target-dir slots available to prune.");
        return;
    }

    let items: Vec<String> = free_slots
        .iter()
        .map(|s| {
            let pref = if s.is_preferred { " <- preferred" } else { "" };
            format!(
                "{} ({}, {}) [free]{}",
                s.name,
                format_size(s.size_bytes),
                format_time_ago(s.mtime),
                pref
            )
        })
        .collect();

    let selections = match MultiSelect::with_theme(&ColorfulTheme::default())
        .with_prompt(
            "Select target-dir slots to prune (Space to toggle, Enter to confirm, Esc/q to cancel)",
        )
        .items(&items)
        .interact_opt()
    {
        Ok(Some(selected)) => selected,
        Ok(None) | Err(_) => {
            println!("Interactive prune cancelled.");
            return;
        }
    };

    if selections.is_empty() {
        println!("No target-dir slots selected. Aborting.");
        return;
    }

    let mut pruned_count = 0;
    let mut total_reclaimed = 0u64;

    for idx in selections {
        let slot = free_slots[idx];
        let lock_path = slot.path.join(".carpe-lock");
        match lockfile::try_lock(&lock_path) {
            Ok(Some(_lock)) => {
                let size = slot.size_bytes;
                if fs::remove_dir_all(&slot.path).is_ok() {
                    println!("Pruned {} ({})", slot.name, format_size(size));
                    pruned_count += 1;
                    total_reclaimed += size;
                } else {
                    eprintln!("Failed to remove {}", slot.path.display());
                }
            }
            _ => {
                println!("Skipping {} (busy)", slot.name);
            }
        }
    }

    println!(
        "Reclaimed {} across {} selected slot(s).",
        format_size(total_reclaimed),
        pruned_count
    );
}

fn dir_size(path: &Path) -> u64 {
    let mut total = 0;
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if let Ok(meta) = entry.metadata() {
                if meta.is_dir() {
                    total += dir_size(&p);
                } else {
                    total += meta.len();
                }
            }
        }
    }
    total
}

fn latest_mtime(path: &Path) -> std::time::SystemTime {
    let mut newest = fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or(std::time::UNIX_EPOCH);

    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                let sub = latest_mtime(&p);
                if sub > newest {
                    newest = sub;
                }
            } else if let Ok(meta) = entry.metadata() {
                if let Ok(m) = meta.modified() {
                    if m > newest {
                        newest = m;
                    }
                }
            }
        }
    }
    newest
}

fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

fn format_time_ago(time: std::time::SystemTime) -> String {
    let Ok(elapsed) = time.elapsed() else {
        return "recently".to_string();
    };
    let secs = elapsed.as_secs();
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
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
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .ok()?;
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
    dirs::cache_dir()
        .unwrap_or_else(env::temp_dir)
        .join("carpe")
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

fn has_explicit_target_dir_flag(args: &[String]) -> bool {
    for arg in args {
        if arg == "--target-dir" || arg.starts_with("--target-dir=") {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_test_repo() -> Option<(PathBuf, PathBuf, PathBuf)> {
        let sys_temp = env::temp_dir();
        let test_run_id = format!("carpe_test_{}_{}", std::process::id(), rand_nonce());
        let root_dir = sys_temp.join(test_run_id);
        fs::create_dir_all(&root_dir).ok()?;

        let main_repo = root_dir.join("main_repo");
        fs::create_dir_all(&main_repo).ok()?;

        // git init main_repo
        let status = Command::new("git")
            .arg("init")
            .arg(&main_repo)
            .status()
            .ok()?;
        if !status.success() {
            return None;
        }

        // Config dummy git user for commit
        let _ = Command::new("git")
            .arg("-C")
            .arg(&main_repo)
            .args(["config", "user.name", "Carpe Test"])
            .status();
        let _ = Command::new("git")
            .arg("-C")
            .arg(&main_repo)
            .args(["config", "user.email", "test@example.com"])
            .status();

        // Create initial commit
        fs::write(main_repo.join("README.md"), "test").ok()?;
        let _ = Command::new("git")
            .arg("-C")
            .arg(&main_repo)
            .args(["add", "."])
            .status();
        let _ = Command::new("git")
            .arg("-C")
            .arg(&main_repo)
            .args(["commit", "-m", "init"])
            .status();

        // Create linked worktree
        let wt_repo = root_dir.join("wt_repo");
        let status = Command::new("git")
            .arg("-C")
            .arg(&main_repo)
            .args([
                "worktree",
                "add",
                "-b",
                "feature",
                wt_repo.to_str().unwrap(),
            ])
            .status()
            .ok()?;
        if !status.success() {
            return None;
        }

        Some((root_dir, main_repo, wt_repo))
    }

    fn rand_nonce() -> u64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64
    }

    #[test]
    fn test_git_worktree_shared_pool_and_isolated_markers() {
        let Some((root_dir, main_repo, wt_repo)) = setup_test_repo() else {
            eprintln!("Skipping git test: git command not available");
            return;
        };

        let main_identity = RepoIdentity::detect(&main_repo);
        let wt_identity = RepoIdentity::detect(&wt_repo);

        // 1. Both worktrees must share the exact same pool name
        assert_eq!(
            main_identity.pool_name(),
            wt_identity.pool_name(),
            "All worktrees of the same repo must share the same target slot pool name"
        );

        // 2. Each worktree must have a distinct private marker directory
        assert_ne!(
            main_identity.marker_dir, wt_identity.marker_dir,
            "Each worktree must store its slot preference in its own private git dir"
        );

        // 3. Slot preferences written in one worktree must not bleed into another
        main_identity.write_preferred_slot(0);
        wt_identity.write_preferred_slot(1);

        assert_eq!(main_identity.read_preferred_slot(), Some(0));
        assert_eq!(wt_identity.read_preferred_slot(), Some(1));

        // Cleanup
        let _ = fs::remove_dir_all(root_dir);
    }

    #[test]
    fn test_worktree_slot_contention_and_fallback() {
        let Some((root_dir, main_repo, wt_repo)) = setup_test_repo() else {
            eprintln!("Skipping git test: git command not available");
            return;
        };

        let cache_temp = root_dir.join("carpe_cache");
        fs::create_dir_all(&cache_temp).unwrap();

        let main_identity = RepoIdentity::detect(&main_repo);
        let wt_identity = RepoIdentity::detect(&wt_repo);

        // Worktree A starts a build and acquires slot 0
        let (slot_a, path_a, lock_a) = select_slot(&main_identity, &cache_temp);
        assert_eq!(slot_a, 0, "First worktree build should allocate slot 0");
        assert!(path_a.ends_with(format!("{}-0", main_identity.pool_name())));

        // Worktree B starts a build while Worktree A is still building (lock_a held)
        let (slot_b, path_b, lock_b) = select_slot(&wt_identity, &cache_temp);
        assert_eq!(
            slot_b, 1,
            "Second worktree should fall back to slot 1 when slot 0 is locked"
        );
        assert!(path_b.ends_with(format!("{}-1", wt_identity.pool_name())));

        // Worktree B records slot 1 as its preferred slot
        assert_eq!(wt_identity.read_preferred_slot(), Some(1));

        // Worktree A finishes build and releases lock_a
        drop(lock_a);
        // Worktree B finishes build and releases lock_b
        drop(lock_b);

        // Worktree B runs build again; slot 1 is free and preferred for B, so B reuses slot 1
        let (slot_b2, _, lock_b2) = select_slot(&wt_identity, &cache_temp);
        assert_eq!(
            slot_b2, 1,
            "Worktree B should reuse preferred slot 1 when free"
        );

        // Worktree A runs build again; slot 0 is free and preferred for A, so A reuses slot 0
        let (slot_a2, _, lock_a2) = select_slot(&main_identity, &cache_temp);
        assert_eq!(
            slot_a2, 0,
            "Worktree A should reuse preferred slot 0 when free"
        );

        drop(lock_b2);
        drop(lock_a2);
        let _ = fs::remove_dir_all(root_dir);
    }

    #[test]
    fn test_utility_functions() {
        assert_eq!(sanitize("my_repo.v1"), "my_repo-v1");
        assert_eq!(sanitize("my-normal-repo"), "my-normal-repo");
        assert_eq!(short_hash("hello"), short_hash("hello"));
        assert_ne!(short_hash("hello"), short_hash("world"));
        assert_eq!(short_hash("hello").len(), 8);
    }

    #[test]
    fn test_format_size_helper() {
        assert_eq!(format_size(500), "500 B");
        assert_eq!(format_size(1536), "1.5 KB");
        assert_eq!(format_size(1048576 * 5), "5.0 MB");
        assert_eq!(format_size(1073741824 * 2), "2.00 GB");
    }

    #[test]
    fn test_lru_pruning_selection() {
        let sys_temp = env::temp_dir();
        let test_dir = sys_temp.join(format!("carpe_lru_test_{}", rand_nonce()));
        fs::create_dir_all(&test_dir).unwrap();

        let pool_name = "testrepo-12345678";
        let path0 = test_dir.join(format!("{pool_name}-0"));
        let path1 = test_dir.join(format!("{pool_name}-1"));
        let path2 = test_dir.join(format!("{pool_name}-2"));

        fs::create_dir_all(&path0).unwrap();
        fs::create_dir_all(&path1).unwrap();
        fs::create_dir_all(&path2).unwrap();

        // Write dummy file content
        fs::write(path0.join("file.txt"), "0").unwrap();
        fs::write(path1.join("file.txt"), "111").unwrap();
        fs::write(path2.join("file.txt"), "222222").unwrap();

        let now = std::time::SystemTime::now();
        let oldest_time = now - std::time::Duration::from_secs(300);
        let middle_time = now - std::time::Duration::from_secs(100);
        let newest_time = now;

        let mut slots = vec![
            SlotCandidate {
                name: format!("{pool_name}-0"),
                path: path0.clone(),
                size_bytes: 1,
                mtime: oldest_time,
                is_busy: false,
                is_preferred: false,
            },
            SlotCandidate {
                name: format!("{pool_name}-1"),
                path: path1.clone(),
                size_bytes: 3,
                mtime: middle_time,
                is_busy: false,
                is_preferred: false,
            },
            SlotCandidate {
                name: format!("{pool_name}-2"),
                path: path2.clone(),
                size_bytes: 6,
                mtime: newest_time,
                is_busy: false,
                is_preferred: false,
            },
        ];

        // Keep 1 newest -> should prune slot 0 and slot 1, keeping slot 2
        run_lru_prune(&mut slots, 1);

        assert!(!path0.exists(), "Oldest slot 0 should be pruned");
        assert!(!path1.exists(), "Middle slot 1 should be pruned");
        assert!(path2.exists(), "Newest slot 2 should be kept");

        let _ = fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn test_cache_root_resolution() {
        let root = cache_root();
        assert!(
            root.ends_with("carpe"),
            "cache_root path must end with 'carpe'"
        );
    }

    #[test]
    fn test_explicit_target_dir_flag_detection() {
        assert!(has_explicit_target_dir_flag(&[
            "build".into(),
            "--target-dir".into(),
            "/tmp/target".into()
        ]));
        assert!(has_explicit_target_dir_flag(&[
            "build".into(),
            "--target-dir=/tmp/target".into()
        ]));
        assert!(!has_explicit_target_dir_flag(&[
            "build".into(),
            "--release".into()
        ]));
    }
}
