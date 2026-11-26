use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::lockfile;
use crate::meta::{CargoBuildState, GitState, SlotMeta};
use crate::repo::RepoIdentity;
use crate::utils::{dir_size, slot_mtime};

// Bound both new-slot attempts and the highest index we'll create, so a
// crowded (or racing) pool fails cleanly instead of growing without limit.
const MAX_NEW_SLOT_ATTEMPTS: usize = 16;

#[derive(Debug, Clone)]
pub struct SlotCandidate {
    pub name: String,
    pub path: PathBuf,
    pub size_bytes: u64,
    pub mtime: Option<SystemTime>,
    pub is_busy: bool,
    pub is_preferred: bool,
}

/// Lock a brand-new slot, re-scanning on every attempt: another process may
/// create (and lock) our chosen index between the snapshot and the lock call.
/// Prefer the next index past the highest existing slot, then reuse the lowest
/// unused, untried gap below [`MAX_NEW_SLOT_ATTEMPTS`].
fn allocate_new_slot(dir_name: &str, root: &Path) -> io::Result<(usize, PathBuf, lockfile::Lock)> {
    fs::create_dir_all(root)?;
    let mut min_next = 0usize;
    let mut tried = [false; MAX_NEW_SLOT_ATTEMPTS];
    for _ in 0..MAX_NEW_SLOT_ATTEMPTS {
        let indices = existing_slot_indices(root, dir_name);
        let mut next = indices
            .iter()
            .map(|idx| idx.saturating_add(1))
            .max()
            .unwrap_or(0)
            .max(min_next);
        if next >= MAX_NEW_SLOT_ATTEMPTS {
            next = match (0..MAX_NEW_SLOT_ATTEMPTS)
                .find(|idx| !tried[*idx] && !indices.contains(idx))
            {
                Some(idx) => idx,
                None => break,
            };
        }
        tried[next] = true;
        min_next = next.saturating_add(1);

        let slot_path = root.join(format!("{dir_name}-{next}"));
        // create_dir is atomic: it tells us whether we won the dir without an
        // exists()/create_dir_all() race window.
        let freshly_created = match fs::create_dir(&slot_path) {
            Ok(()) => true,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => false,
            Err(e) => return Err(e),
        };

        let lock_path = slot_path.join(".carpe-lock");
        match lockfile::try_lock(&lock_path) {
            Ok(Some(lock)) => return Ok((next, slot_path, lock)),
            Ok(None) => {} // Winner owns this dir now; leave it alone.
            Err(e) => {
                if freshly_created {
                    let _ = fs::remove_dir(&slot_path);
                }
                return Err(e);
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::WouldBlock,
        format!("no free slot after {MAX_NEW_SLOT_ATTEMPTS} attempts"),
    ))
}

pub fn select_slot(
    identity: &RepoIdentity,
    root: &Path,
    git_state: &GitState,
    args: &[String],
) -> io::Result<(usize, PathBuf, lockfile::Lock)> {
    let dir_name = identity.pool_name();
    let preferred = identity.read_preferred_slot();
    let build_state = CargoBuildState::parse(args);

    let candidates = existing_slot_indices(root, &dir_name);

    // Rank candidate slots holistically: affinity score + preferred slot weight (75 points)
    let mut ranked: Vec<(usize, u32, u64)> = candidates
        .iter()
        .map(|&idx| {
            let path = root.join(format!("{dir_name}-{idx}"));
            let meta = SlotMeta::read(&path).unwrap_or_default();
            let mut score = meta.score(git_state, &build_state);
            if preferred == Some(idx) {
                score += 75; // Baseline priority weight for this worktree's preferred slot
            }
            (idx, score, meta.timestamp)
        })
        .collect();

    ranked.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| b.2.cmp(&a.2))
            .then_with(|| a.0.cmp(&b.0))
    });

    let mut chosen: Option<(usize, PathBuf, lockfile::Lock)> = None;
    for &(idx, _, _) in &ranked {
        let slot_path = root.join(format!("{dir_name}-{idx}"));
        let lock_path = slot_path.join(".carpe-lock");
        match lockfile::try_lock(&lock_path) {
            Ok(Some(lock)) => {
                chosen = Some((idx, slot_path, lock));
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

    let (slot_idx, slot_path, lock) = match chosen {
        Some(chosen) => chosen,
        None => allocate_new_slot(&dir_name, root)?,
    };

    if preferred != Some(slot_idx) {
        identity.write_preferred_slot(slot_idx);
    }

    SlotMeta::write(&slot_path, git_state, &build_state);

    Ok((slot_idx, slot_path, lock))
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
        let mtime = slot_mtime(&path);

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
    use crate::utils::test_rand_nonce;
    use std::env;
    use std::process::Command;
    use std::sync::Barrier;

    fn setup_test_repo() -> Option<(PathBuf, PathBuf, PathBuf)> {
        let sys_temp = env::temp_dir();
        let test_run_id = format!("carpe_test_{}_{}", std::process::id(), test_rand_nonce());
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

        let (slot_a, path_a, lock_a) =
            select_slot(&main_identity, &cache_temp, &GitState::default(), &[])
                .expect("first slot allocation should succeed");
        assert_eq!(slot_a, 0, "First worktree build should allocate slot 0");
        assert!(path_a.ends_with(format!("{}-0", main_identity.pool_name())));

        let (slot_b, path_b, lock_b) =
            select_slot(&wt_identity, &cache_temp, &GitState::default(), &[])
                .expect("fallback slot allocation should succeed");
        assert_eq!(
            slot_b, 1,
            "Second worktree should fall back to slot 1 when slot 0 is locked"
        );
        assert!(path_b.ends_with(format!("{}-1", wt_identity.pool_name())));

        assert_eq!(wt_identity.read_preferred_slot(), Some(1));

        drop(lock_a);
        drop(lock_b);

        let (slot_b2, _, lock_b2) =
            select_slot(&wt_identity, &cache_temp, &GitState::default(), &[])
                .expect("reused preferred slot should be available");
        assert_eq!(
            slot_b2, 1,
            "Worktree B should reuse preferred slot 1 when free"
        );

        let (slot_a2, _, lock_a2) =
            select_slot(&main_identity, &cache_temp, &GitState::default(), &[])
                .expect("reused preferred slot should be available");
        assert_eq!(
            slot_a2, 0,
            "Worktree A should reuse preferred slot 0 when free"
        );

        drop(lock_b2);
        drop(lock_a2);
        let _ = fs::remove_dir_all(root_dir);
    }

    #[test]
    fn test_concurrent_select_slot_allocates_distinct_locked_slots() {
        let sys_temp = env::temp_dir();
        let root_dir = sys_temp.join(format!(
            "carpe_concurrent_slots_{}_{}",
            std::process::id(),
            test_rand_nonce()
        ));
        let cache_root = root_dir.join("cache");
        fs::create_dir_all(&cache_root).unwrap();

        let identity = RepoIdentity {
            name_source: root_dir.join("repo"),
            repo_identity: root_dir.clone(),
            marker_dir: root_dir.join("markers"),
        };
        let pool_name = identity.pool_name();
        let busy_count = 4;
        let mut busy_locks = Vec::new();
        for i in 0..busy_count {
            let slot_path = cache_root.join(format!("{pool_name}-{i}"));
            fs::create_dir_all(&slot_path).unwrap();
            let lock = lockfile::try_lock(&slot_path.join(".carpe-lock"))
                .expect("pre-lock should not hit an I/O error")
                .expect("pre-existing slot should be lockable");
            busy_locks.push(lock);
        }

        let thread_count = 4;
        let barrier = Barrier::new(thread_count);
        let barrier_ref = &barrier;
        let cache_ref = &cache_root;
        let git_state_ref = &GitState::default();

        let results = std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for i in 0..thread_count {
                let thread_identity = RepoIdentity {
                    name_source: identity.name_source.clone(),
                    repo_identity: identity.repo_identity.clone(),
                    marker_dir: root_dir.join(format!("marker-{i}")),
                };
                handles.push(scope.spawn(move || {
                    // Every caller sees slots 0..4 locked and must take the retry path.
                    barrier_ref.wait();
                    select_slot(&thread_identity, cache_ref, git_state_ref, &[])
                        .expect("a free slot must be allocated without panicking")
                }));
            }
            handles
                .into_iter()
                .map(|h| h.join().expect("select_slot must not panic"))
                .collect::<Vec<_>>()
        });

        let mut indices: Vec<usize> = results.iter().map(|(idx, _, _)| *idx).collect();
        indices.sort_unstable();
        let mut distinct = indices.clone();
        distinct.dedup();
        assert_eq!(
            distinct.len(),
            thread_count,
            "concurrent callers must get distinct slot indices, got {indices:?}"
        );
        assert!(
            indices.iter().all(|idx| *idx >= busy_count),
            "callers must skip the pre-locked slots, got {indices:?}"
        );
        for (idx, slot_path, _lock) in &results {
            assert_eq!(slot_path, &cache_root.join(format!("{pool_name}-{idx}")));
            let held = lockfile::try_lock(&slot_path.join(".carpe-lock"))
                .expect("lock check should not hit an I/O error");
            assert!(
                held.is_none(),
                "slot {idx} must remain locked for its caller"
            );
        }

        drop(busy_locks);
        let _ = fs::remove_dir_all(&root_dir);
    }

    #[test]
    fn test_allocate_new_slot_reuses_gap_when_highest_slot_busy() {
        let sys_temp = env::temp_dir();
        let root_dir = sys_temp.join(format!(
            "carpe_gap_reuse_slots_{}_{}",
            std::process::id(),
            test_rand_nonce()
        ));
        let cache_root = root_dir.join("cache");
        let identity = RepoIdentity {
            name_source: root_dir.join("repo"),
            repo_identity: root_dir.clone(),
            marker_dir: root_dir.join("markers"),
        };
        let pool_name = identity.pool_name();

        // Only the highest bounded index exists, and it is busy. Indices 0..15
        // are free but non-existent, so allocation must fall back to a gap.
        let busy_idx = MAX_NEW_SLOT_ATTEMPTS - 1;
        let busy_path = cache_root.join(format!("{pool_name}-{busy_idx}"));
        fs::create_dir_all(&busy_path).unwrap();
        let busy_lock = lockfile::try_lock(&busy_path.join(".carpe-lock"))
            .expect("pre-lock should not hit an I/O error")
            .expect("pre-existing slot should be lockable");

        let (idx, slot_path, lock) = select_slot(&identity, &cache_root, &GitState::default(), &[])
            .expect("a free gap below the busy highest slot must be allocated");
        assert!(
            idx < busy_idx,
            "allocation must reuse a gap instead of failing, got slot {idx}"
        );
        assert_eq!(idx, 0, "the lowest gap should be chosen first");
        assert_eq!(slot_path, cache_root.join(format!("{pool_name}-{idx}")));

        let contender = lockfile::try_lock(&slot_path.join(".carpe-lock"))
            .expect("lock check should not hit an I/O error");
        assert!(contender.is_none(), "returned slot must hold a valid lock");

        drop(lock);
        drop(busy_lock);
        let _ = fs::remove_dir_all(&root_dir);
    }

    #[test]
    fn test_select_slot_reports_exhaustion_without_panicking() {
        let sys_temp = env::temp_dir();
        let root_dir = sys_temp.join(format!(
            "carpe_exhausted_slots_{}_{}",
            std::process::id(),
            test_rand_nonce()
        ));
        let cache_root = root_dir.join("cache");
        fs::create_dir_all(&cache_root).unwrap();

        let identity = RepoIdentity {
            name_source: root_dir.join("repo"),
            repo_identity: root_dir.clone(),
            marker_dir: root_dir.join("markers"),
        };
        let pool_name = identity.pool_name();
        let mut held_locks = Vec::new();
        for i in 0..MAX_NEW_SLOT_ATTEMPTS {
            let slot_path = cache_root.join(format!("{pool_name}-{i}"));
            fs::create_dir_all(&slot_path).unwrap();
            held_locks.push(
                lockfile::try_lock(&slot_path.join(".carpe-lock"))
                    .expect("pre-lock should not hit an I/O error")
                    .expect("pre-existing slot should be lockable"),
            );
        }

        let err = select_slot(&identity, &cache_root, &GitState::default(), &[])
            .err()
            .expect("all bounded slot attempts are contended");
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(
            existing_slot_indices(&cache_root, &pool_name).len(),
            MAX_NEW_SLOT_ATTEMPTS
        );
        assert!(!cache_root
            .join(format!("{pool_name}-{MAX_NEW_SLOT_ATTEMPTS}"))
            .exists());

        drop(held_locks);
        let _ = fs::remove_dir_all(&root_dir);
    }
}
