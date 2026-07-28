use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::repo::run_git;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitState {
    pub head: Option<String>,
    pub branch: Option<String>,
}

impl GitState {
    pub fn detect(cwd: &Path) -> GitState {
        if let Some(out) = run_git(cwd, &["rev-parse", "HEAD", "--abbrev-ref", "HEAD"]) {
            let mut lines = out.lines();
            let head = lines.next().map(String::from);
            let branch = lines.next().map(String::from).filter(|b| b != "HEAD");
            return GitState { head, branch };
        }
        GitState {
            head: None,
            branch: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CargoBuildState {
    pub profile: Option<String>,
    pub target: Option<String>,
}

impl CargoBuildState {
    pub fn parse(args: &[String]) -> CargoBuildState {
        let mut profile: Option<String> = None;
        let mut target: Option<String> = None;

        let mut i = 0;
        while i < args.len() {
            let arg = &args[i];
            if arg == "--release" || arg == "-r" {
                profile = Some("release".to_string());
            } else if arg == "--profile" {
                if i + 1 < args.len() {
                    profile = Some(args[i + 1].clone());
                    i += 1;
                }
            } else if let Some(p) = arg.strip_prefix("--profile=") {
                profile = Some(p.to_string());
            } else if arg == "--target" {
                if i + 1 < args.len() {
                    target = Some(args[i + 1].clone());
                    i += 1;
                }
            } else if let Some(t) = arg.strip_prefix("--target=") {
                target = Some(t.to_string());
            }
            i += 1;
        }

        if profile.is_none() {
            profile = Some("debug".to_string());
        }

        CargoBuildState { profile, target }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SlotMeta {
    pub head: Option<String>,
    pub branch: Option<String>,
    pub profile: Option<String>,
    pub target: Option<String>,
    pub timestamp: u64,
}

impl SlotMeta {
    pub fn read(slot_path: &Path) -> Option<SlotMeta> {
        let content = fs::read_to_string(slot_path.join(".carpe-meta")).ok()?;
        let mut meta = SlotMeta::default();
        for line in content.lines() {
            let line = line.trim();
            if let Some(val) = line.strip_prefix("head=") {
                meta.head = Some(val.to_string());
            } else if let Some(val) = line.strip_prefix("branch=") {
                meta.branch = Some(val.to_string());
            } else if let Some(val) = line.strip_prefix("profile=") {
                meta.profile = Some(val.to_string());
            } else if let Some(val) = line.strip_prefix("target=") {
                meta.target = Some(val.to_string());
            } else if let Some(val) = line.strip_prefix("timestamp=") {
                if let Ok(ts) = val.parse::<u64>() {
                    meta.timestamp = ts;
                }
            }
        }
        Some(meta)
    }

    pub fn write(slot_path: &Path, git_state: &GitState, build_state: &CargoBuildState) {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut out = String::new();
        if let Some(h) = &git_state.head {
            out.push_str(&format!("head={h}\n"));
        }
        if let Some(b) = &git_state.branch {
            out.push_str(&format!("branch={b}\n"));
        }
        if let Some(p) = &build_state.profile {
            out.push_str(&format!("profile={p}\n"));
        }
        if let Some(t) = &build_state.target {
            out.push_str(&format!("target={t}\n"));
        }
        out.push_str(&format!("timestamp={ts}\n"));
        let _ = fs::write(slot_path.join(".carpe-meta"), out);
    }

    pub fn score(&self, current_git: &GitState, current_build: &CargoBuildState) -> u32 {
        let mut score = 0;
        if let (Some(h1), Some(h2)) = (&self.head, &current_git.head) {
            if h1 == h2 {
                score += 100;
            }
        }
        if let (Some(p1), Some(p2)) = (&self.profile, &current_build.profile) {
            if p1 == p2 {
                score += 40;
            }
        }
        if let (Some(t1), Some(t2)) = (&self.target, &current_build.target) {
            if t1 == t2 {
                score += 40;
            }
        }
        if let (Some(b1), Some(b2)) = (&self.branch, &current_git.branch) {
            if b1 == b2 {
                score += 30;
            }
        }
        score
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_rand_nonce;
    use std::env;

    #[test]
    fn test_cargo_build_state_parse() {
        let bs1 = CargoBuildState::parse(&["build".into(), "--release".into()]);
        assert_eq!(bs1.profile, Some("release".to_string()));
        assert_eq!(bs1.target, None);

        let bs2 = CargoBuildState::parse(&[
            "build".into(),
            "--target".into(),
            "wasm32-unknown-unknown".into(),
            "--profile".into(),
            "custom".into(),
        ]);
        assert_eq!(bs2.profile, Some("custom".to_string()));
        assert_eq!(bs2.target, Some("wasm32-unknown-unknown".to_string()));
    }

    #[test]
    fn test_slot_meta_read_write() {
        let sys_temp = env::temp_dir();
        let slot_path = sys_temp.join(format!("carpe_meta_test_{}", test_rand_nonce()));
        fs::create_dir_all(&slot_path).unwrap();

        let g_state = GitState {
            head: Some("a1b2c3d4e5f6".into()),
            branch: Some("feature-x".into()),
        };
        let b_state = CargoBuildState {
            profile: Some("release".into()),
            target: Some("wasm32-unknown-unknown".into()),
        };

        SlotMeta::write(&slot_path, &g_state, &b_state);

        let read_meta = SlotMeta::read(&slot_path).expect("meta should be readable");
        assert_eq!(read_meta.head, Some("a1b2c3d4e5f6".into()));
        assert_eq!(read_meta.branch, Some("feature-x".into()));
        assert_eq!(read_meta.profile, Some("release".into()));
        assert_eq!(read_meta.target, Some("wasm32-unknown-unknown".into()));
        assert_eq!(read_meta.score(&g_state, &b_state), 210);

        let _ = fs::remove_dir_all(&slot_path);
    }
}
