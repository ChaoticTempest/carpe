use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::meta::GitState;
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
        identity_from_git_dirs(cwd, git_dirs(cwd))
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

fn identity_from_git_dirs(cwd: &Path, dirs: Option<(PathBuf, PathBuf)>) -> RepoIdentity {
    if let Some((common_dir, worktree_git_dir)) = dirs {
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
        let root = find_cargo_workspace_root(cwd).unwrap_or_else(|| canonicalize_best_effort(cwd));
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

/// Detects repo identity and git state with a single `git rev-parse` process.
///
/// A failing command can still emit usable git-dir lines, so partial stdout is
/// used for identity. HEAD/branch are only trusted when git succeeded: on an
/// unborn HEAD the remaining arguments are echoed instead of resolved.
pub fn detect_context(cwd: &Path) -> (RepoIdentity, GitState) {
    let out = git_output(
        cwd,
        &[
            "rev-parse",
            "--git-common-dir",
            "--git-dir",
            "HEAD",
            "--abbrev-ref",
            "HEAD",
        ],
    );

    let dirs = out.as_ref().and_then(|(stdout, _)| {
        let mut lines = stdout.lines();
        let common_raw = lines.next()?;
        let worktree_raw = lines.next()?;
        Some((resolve(cwd, common_raw), resolve(cwd, worktree_raw)))
    });
    let identity = identity_from_git_dirs(cwd, dirs);

    let git_state = out
        .as_ref()
        .and_then(|(stdout, success)| {
            if !*success {
                return None;
            }
            let lines: Vec<&str> = stdout.lines().collect();
            if lines.len() < 4 {
                return None;
            }
            Some(GitState {
                head: Some(lines[2].to_string()),
                branch: Some(lines[3].to_string()).filter(|b| b != "HEAD"),
            })
        })
        .unwrap_or_default();

    (identity, git_state)
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
    let (out, success) = git_output(cwd, args)?;
    if success {
        Some(out)
    } else {
        None
    }
}

/// Returns git's stdout together with its exit status. Callers that need
/// stdout from a partially failing command (see `detect_context`) use this
/// directly instead of `run_git`.
fn git_output(cwd: &Path, args: &[&str]) -> Option<(String, bool)> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .ok()?;
    let stdout = String::from_utf8(out.stdout).ok()?.trim().to_string();
    Some((stdout, out.status.success()))
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

    #[test]
    fn test_detect_context_matches_identity_and_populates_git_state() {
        let sys_temp = std::env::temp_dir();
        let root = sys_temp.join(format!("carpe_detect_context_test_{}", test_rand_nonce()));
        let repo = root.join("repo");
        if fs::create_dir_all(&repo).is_err() {
            return;
        }

        let git = |args: &[&str]| -> bool {
            Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        };

        if !git(&["init"]) {
            let _ = fs::remove_dir_all(&root);
            eprintln!("Skipping git test: git command not available");
            return;
        }
        let _ = git(&["config", "user.email", "test@example.com"]);
        let _ = git(&["config", "user.name", "Test User"]);

        // Unborn HEAD: identity must still resolve, git state stays empty.
        let (unborn_identity, unborn_state) = detect_context(&repo);
        let unborn_pool_name = RepoIdentity::detect(&repo).pool_name();

        let _ = fs::write(repo.join("README.md"), "# Test Repo");
        let _ = git(&["add", "."]);
        if !git(&["-c", "commit.gpgsign=false", "commit", "-m", "init"]) {
            let _ = fs::remove_dir_all(&root);
            eprintln!("Skipping git test: initial commit failed");
            return;
        }

        let (identity, git_state) = detect_context(&repo);
        let pool_name = RepoIdentity::detect(&repo).pool_name();

        // Detached HEAD: HEAD resolves, but there is no branch name.
        assert!(git(&["checkout", "--detach", "HEAD"]));
        let (detached_identity, detached_state) = detect_context(&repo);

        // A non-git directory has no git state at all.
        let plain = root.join("not-a-repo");
        fs::create_dir_all(&plain).unwrap();
        let (_, plain_state) = detect_context(&plain);

        // Clean up before asserting so a failing assertion cannot leak the repo.
        let _ = fs::remove_dir_all(&root);

        assert_eq!(unborn_identity.pool_name(), unborn_pool_name);
        assert_eq!(unborn_state, GitState::default());
        assert_eq!(identity.pool_name(), pool_name);
        assert!(
            git_state.head.is_some(),
            "HEAD should be populated after the initial commit"
        );
        assert!(
            git_state.branch.is_some(),
            "branch should be populated after the initial commit"
        );
        assert_ne!(
            git_state.branch.as_deref(),
            Some("HEAD"),
            "branch should be a real branch name, not detached HEAD"
        );

        assert_eq!(detached_identity.pool_name(), pool_name);
        assert!(
            detached_state.head.is_some(),
            "HEAD should still resolve on a detached HEAD"
        );
        assert!(
            detached_state.branch.is_none(),
            "a detached HEAD must not report a branch"
        );
        assert_eq!(
            plain_state,
            GitState::default(),
            "a non-git directory must have no git state"
        );
    }
}
