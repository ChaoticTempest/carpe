use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::lockfile;
use crate::meta::{CargoBuildState, GitState, SlotMeta};
use crate::repo::RepoIdentity;
use crate::utils::{dir_size, latest_mtime};

#[derive(Debug, Clone)]
pub struct SlotCandidate {
    pub name: String,
    pub path: PathBuf,
    pub size_bytes: u64,
    pub mtime: Option<SystemTime>,
    pub is_busy: bool,
    pub is_preferred: bool,
}

pub fn select_slot(
    identity: &RepoIdentity,
    root: &Path,
    cwd: &Path,
    args: &[String],
) -> (usize, PathBuf, lockfile::Lock) {
    let dir_name = identity.pool_name();
    let preferred = identity.read_preferred_slot();
    let git_state = GitState::detect(cwd);
    let build_state = CargoBuildState::parse(args);

    let mut candidates = existing_slot_indices(root, &dir_name);

    if let Some(p) = preferred {
        if let Some(pos) = candidates.iter().position(|&i| i == p) {
            candidates.remove(pos);
            candidates.insert(0, p);
        }
    }

    // Rank non-preferred candidate slots by affinity score (commit/profile/target/branch match), then timestamp
    if candidates.len() > 1 {
        let start_idx = if preferred.is_some() { 1 } else { 0 };
        if start_idx < candidates.len() {
            candidates[start_idx..].sort_by(|&a, &b| {
                let path_a = root.join(format!("{dir_name}-{a}"));
                let path_b = root.join(format!("{dir_name}-{b}"));
                let meta_a = SlotMeta::read(&path_a).unwrap_or_default();
                let meta_b = SlotMeta::read(&path_b).unwrap_or_default();
                let score_a = meta_a.score(&git_state, &build_state);
                let score_b = meta_b.score(&git_state, &build_state);

                score_b
                    .cmp(&score_a)
                    .then_with(|| meta_b.timestamp.cmp(&meta_a.timestamp))
                    .then_with(|| a.cmp(&b))
            });
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

    SlotMeta::write(&slot_path, &git_state, &build_state);

    (slot_idx, slot_path, lock)
}

pub fn existing_slot_indices(root: &Path, pool_name: &str) -> Vec<usize> {
    let prefix = format!("{pool_name}-");
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let s = name.to_string_lossy();
            if let Some(rest) = s.strip_prefix(&prefix) {
                if let Ok(idx) = rest.parse::<usize>() {
                    out.push(idx);
                }
            }
        }
    }
    out
}

pub fn collect_slots(
    root: &Path,
    filter_pool: Option<&str>,
    preferred: Option<usize>,
) -> Vec<SlotCandidate> {
    let mut candidates = Vec::new();
    let entries = match fs::read_dir(root) {
        Ok(e) => e,
        Err(_) => return candidates,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let name = path.file_name().unwrap_or_default().to_string_lossy();

        if let Some(pool) = filter_pool {
            let prefix = format!("{pool}-");
            if !name.starts_with(&prefix) {
                continue;
            }
            let rest = &name[prefix.len()..];
            if rest.parse::<usize>().is_err() {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::process::Command;

    fn rand_nonce() -> u64 {
        use std::time::UNIX_EPOCH;
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64
    }

    fn setup_test_repo() -> Option<(PathBuf, PathBuf, PathBuf)> {
        let sys_temp = env::temp_dir();
        let test_run_id = format!("carpe_test_{}_{}", std::process::id(), rand_nonce());
        let root_dir = sys_temp.join(test_run_id);
        fs::create_dir_all(&root_dir).ok()?;

        let main_repo = root_dir.join("main_repo");
        fs::create_dir_all(&main_repo).ok()?;

        let status = Command::new("git")
            .arg("init")
            .arg(&main_repo)
            .status()
            .ok()?;
        if !status.success() {
            return None;
        }

        let _ = Command::new("git")
            .arg("-C")
            .arg(&main_repo)
            .args(["config", "user.email", "test@example.com"])
            .status();
        let _ = Command::new("git")
            .arg("-C")
            .arg(&main_repo)
            .args(["config", "user.name", "Test User"])
            .status();

        let readme = main_repo.join("README.md");
        let _ = fs::write(&readme, "# Test Repo");
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

    #[test]
    fn test_git_worktree_shared_pool_and_isolated_markers() {
        let Some((root_dir, main_repo, wt_repo)) = setup_test_repo() else {
            eprintln!("Skipping git test: git command not available");
            return;
        };

        let main_identity = RepoIdentity::detect(&main_repo);
        let wt_identity = RepoIdentity::detect(&wt_repo);

        assert_eq!(
            main_identity.pool_name(),
            wt_identity.pool_name(),
            "All worktrees of the same repo must share the same target slot pool name"
        );

        assert_ne!(
            main_identity.marker_dir, wt_identity.marker_dir,
            "Each worktree must store its slot preference in its own private git dir"
        );

        main_identity.write_preferred_slot(0);
        wt_identity.write_preferred_slot(1);

        assert_eq!(main_identity.read_preferred_slot(), Some(0));
        assert_eq!(wt_identity.read_preferred_slot(), Some(1));

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

        let (slot_a, path_a, lock_a) = select_slot(&main_identity, &cache_temp, &main_repo, &[]);
        assert_eq!(slot_a, 0, "First worktree build should allocate slot 0");
        assert!(path_a.ends_with(format!("{}-0", main_identity.pool_name())));

        let (slot_b, path_b, lock_b) = select_slot(&wt_identity, &cache_temp, &wt_repo, &[]);
        assert_eq!(
            slot_b, 1,
            "Second worktree should fall back to slot 1 when slot 0 is locked"
        );
        assert!(path_b.ends_with(format!("{}-1", wt_identity.pool_name())));

        assert_eq!(wt_identity.read_preferred_slot(), Some(1));

        drop(lock_a);
        drop(lock_b);

        let (slot_b2, _, lock_b2) = select_slot(&wt_identity, &cache_temp, &wt_repo, &[]);
        assert_eq!(
            slot_b2, 1,
            "Worktree B should reuse preferred slot 1 when free"
        );

        let (slot_a2, _, lock_a2) = select_slot(&main_identity, &cache_temp, &main_repo, &[]);
        assert_eq!(
            slot_a2, 0,
            "Worktree A should reuse preferred slot 0 when free"
        );

        drop(lock_b2);
        drop(lock_a2);
        let _ = fs::remove_dir_all(root_dir);
    }
}
