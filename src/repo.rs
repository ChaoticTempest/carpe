use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::utils::{cache_root, canonicalize_best_effort, sanitize, short_hash};

/// Identifies the repo-wide pool and this worktree's private marker location.
pub struct RepoIdentity {
    /// Directory whose basename seeds the pool name (main worktree root, or
    /// cwd itself outside of git).
    pub name_source: PathBuf,
    /// Stable, canonicalized identity for the whole repo (same value in
    /// every worktree of the same repo). Used only for hashing.
    pub repo_identity: PathBuf,
    /// Where this *worktree's* "preferred slot" marker is stored. For git
    /// repos this is the worktree's own private git-dir (e.g.
    /// `.git/worktrees/<name>` for a linked worktree, or `.git` for the
    /// main one), which git deletes on `git worktree remove` -- so stale
    /// markers can't accumulate. Outside of git, falls back to a
    /// cwd-keyed directory under the carpe cache root.
    pub marker_dir: PathBuf,
}

impl RepoIdentity {
    pub fn detect(cwd: &Path) -> RepoIdentity {
        if let Some((common_dir, worktree_git_dir)) = git_dirs(cwd) {
            let common_dir = canonicalize_best_effort(&common_dir);
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
            let root =
                find_cargo_workspace_root(cwd).unwrap_or_else(|| canonicalize_best_effort(cwd));
            let marker_dir = cache_root()
                .join("state")
                .join(short_hash(&root.to_string_lossy()));
            RepoIdentity {
                name_source: root.clone(),
                repo_identity: root,
                marker_dir,
            }
        }
    }

    /// The `{directory-name}` prefix slots are named after, e.g. `myrepo-a1b2c3d4`.
    pub fn pool_name(&self) -> String {
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

    pub fn marker_path(&self) -> PathBuf {
        self.marker_dir.join("carpe-slot")
    }

    pub fn read_preferred_slot(&self) -> Option<usize> {
        fs::read_to_string(self.marker_path())
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    pub fn write_preferred_slot(&self, idx: usize) {
        if fs::create_dir_all(&self.marker_dir).is_ok() {
            let _ = fs::write(self.marker_path(), idx.to_string());
        }
    }
}

pub fn git_dirs(cwd: &Path) -> Option<(PathBuf, PathBuf)> {
    let out = run_git(cwd, &["rev-parse", "--git-common-dir", "--git-dir"])?;
    let mut lines = out.lines();
    let common_raw = lines.next()?;
    let worktree_raw = lines.next()?;
    Some((resolve(cwd, common_raw), resolve(cwd, worktree_raw)))
}

pub fn git_common_dir(cwd: &Path) -> Option<PathBuf> {
    git_dirs(cwd).map(|(common, _)| common)
}

pub fn git_dir(cwd: &Path) -> Option<PathBuf> {
    git_dirs(cwd).map(|(_, worktree)| worktree)
}

pub fn run_git(cwd: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
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

pub fn find_cargo_workspace_root(cwd: &Path) -> Option<PathBuf> {
    let mut current = canonicalize_best_effort(cwd);
    let mut top_root: Option<PathBuf> = None;

    loop {
        let manifest = current.join("Cargo.toml");
        if manifest.is_file() {
            if top_root.is_none() {
                top_root = Some(current.clone());
            }

            if let Ok(content) = fs::read_to_string(&manifest) {
                if content.contains("[workspace]") {
                    top_root = Some(current.clone());
                }
            }
        }

        if !current.pop() {
            break;
        }
    }

    top_root
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_rand_nonce;

    #[test]
    fn test_non_git_cargo_workspace_root_resolution() {
        let sys_temp = std::env::temp_dir();
        let root = sys_temp.join(format!("carpe_ws_test_{}", test_rand_nonce()));
        let workspace_dir = root.join("my_workspace");
        let subcrate_dir = workspace_dir.join("crates").join("subcrate");

        fs::create_dir_all(&subcrate_dir).unwrap();

        fs::write(
            workspace_dir.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]",
        )
        .unwrap();

        fs::write(
            subcrate_dir.join("Cargo.toml"),
            "[package]\nname = \"subcrate\"\nversion = \"0.1.0\"",
        )
        .unwrap();

        let identity_root = RepoIdentity::detect(&workspace_dir);
        let identity_subcrate = RepoIdentity::detect(&subcrate_dir);

        assert_eq!(
            identity_root.pool_name(),
            identity_subcrate.pool_name(),
            "Non-git subcrate and workspace root must resolve to the exact same pool name"
        );

        let _ = fs::remove_dir_all(&root);
    }

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("carpe_repo_{label}_{}", test_rand_nonce()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn test_git_helpers_in_and_outside_a_repository() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
        assert!(git_dirs(repo).is_some());
        assert!(git_common_dir(repo).is_some());
        assert!(git_dir(repo).is_some());

        let non_git = temp_dir("nogit");
        assert!(git_dirs(&non_git).is_none());
        assert!(git_common_dir(&non_git).is_none());
        assert!(git_dir(&non_git).is_none());
        assert!(run_git(&non_git, &["rev-parse", "HEAD"]).is_none());
        fs::remove_dir_all(&non_git).unwrap();
    }

    #[test]
    fn test_resolve_absolute_and_relative_git_paths() {
        assert_eq!(
            resolve(Path::new("/work/tree"), "/repo/.git/worktrees/x"),
            PathBuf::from("/repo/.git/worktrees/x")
        );
        assert_eq!(
            resolve(Path::new("/work/tree"), ".git"),
            PathBuf::from("/work/tree/.git")
        );
    }

    #[test]
    fn test_preferred_slot_marker_read_and_write() {
        let dir = temp_dir("marker");
        let marker_dir = dir.join("gitdir");
        let identity = RepoIdentity {
            name_source: PathBuf::from("/repos/project"),
            repo_identity: PathBuf::from("/repos/project"),
            marker_dir: marker_dir.clone(),
        };

        assert_eq!(identity.marker_path(), marker_dir.join("carpe-slot"));
        assert_eq!(
            identity.read_preferred_slot(),
            None,
            "missing marker -> None"
        );

        identity.write_preferred_slot(3);
        assert_eq!(identity.read_preferred_slot(), Some(3));

        fs::write(identity.marker_path(), "garbage").unwrap();
        assert_eq!(
            identity.read_preferred_slot(),
            None,
            "invalid marker -> None"
        );

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_non_git_fallback_without_cargo_toml() {
        let dir = temp_dir("fallback");
        assert!(find_cargo_workspace_root(&dir).is_none());

        let identity = RepoIdentity::detect(&dir);
        let canonical = canonicalize_best_effort(&dir);
        assert_eq!(identity.name_source, canonical);
        assert_eq!(identity.repo_identity, canonical);
        assert!(identity.marker_dir.starts_with(cache_root().join("state")));
        assert!(!identity.pool_name().is_empty());

        let no_name = RepoIdentity {
            name_source: PathBuf::from("/"),
            repo_identity: PathBuf::from("/"),
            marker_dir: dir.clone(),
        };
        assert!(no_name.pool_name().starts_with("project-"));

        fs::remove_dir_all(&dir).unwrap();
    }
}
