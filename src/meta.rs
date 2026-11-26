use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::repo::run_git;
use crate::utils::extract_toolchain;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitState {
    pub head: Option<String>,
    pub branch: Option<String>,
}

impl GitState {
    pub fn detect(cwd: &Path) -> GitState {
        let Some(out) = run_git(cwd, &["rev-parse", "HEAD", "--abbrev-ref", "HEAD"]) else {
            return GitState::default();
        };
        let mut lines = out.lines();
        let head = lines.next().map(String::from);
        let branch = lines.next().map(String::from).filter(|b| b != "HEAD");
        GitState { head, branch }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CargoBuildState {
    pub toolchain: Option<String>,
    pub profile: Option<String>,
    pub target: Option<String>,
    pub features: Option<String>,
}

impl CargoBuildState {
    pub fn parse(args: &[String]) -> CargoBuildState {
        let (toolchain, sub_args) = extract_toolchain(args);
        let mut profile: Option<String> = None;
        let mut target: Option<String> = None;
        let mut features: Option<String> = None;

        let mut i = 0;
        while i < sub_args.len() {
            let arg = &sub_args[i];
            if arg == "--release" || arg == "-r" {
                profile = Some("release".to_string());
            } else if arg == "--profile"
                && let Some(val) = sub_args.get(i + 1)
            {
                profile = Some(val.clone());
                i += 1;
            } else if let Some(p) = arg.strip_prefix("--profile=") {
                profile = Some(p.to_string());
            } else if arg == "--target"
                && let Some(val) = sub_args.get(i + 1)
            {
                target = Some(val.clone());
                i += 1;
            } else if let Some(t) = arg.strip_prefix("--target=") {
                target = Some(t.to_string());
            } else if (arg == "--features" || arg == "-F")
                && let Some(val) = sub_args.get(i + 1)
            {
                features = Some(val.clone());
                i += 1;
            } else if let Some(f) = arg.strip_prefix("--features=") {
                features = Some(f.to_string());
            }
            i += 1;
        }

        if profile.is_none() {
            profile = Some("debug".to_string());
        }

        CargoBuildState {
            toolchain,
            profile,
            target,
            features,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SlotMeta {
    pub head: Option<String>,
    pub branch: Option<String>,
    pub toolchain: Option<String>,
    pub profile: Option<String>,
    pub target: Option<String>,
    pub features: Option<String>,
    pub timestamp: u64,
}

impl SlotMeta {
    pub fn read(slot_path: &Path) -> Option<SlotMeta> {
        let content = fs::read_to_string(slot_path.join(".carpe-meta")).ok()?;
        let mut meta = SlotMeta::default();
        for line in content.lines() {
            let line = line.trim();
            let Some((key, val)) = line.split_once('=') else {
                continue;
            };
            match key {
                "head" => meta.head = Some(val.to_string()),
                "branch" => meta.branch = Some(val.to_string()),
                "toolchain" => meta.toolchain = Some(val.to_string()),
                "profile" => meta.profile = Some(val.to_string()),
                "target" => meta.target = Some(val.to_string()),
                "features" => meta.features = Some(val.to_string()),
                "timestamp" => {
                    if let Ok(ts) = val.parse::<u64>() {
                        meta.timestamp = ts;
                    }
                }
                _ => {}
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
        if let Some(tc) = &build_state.toolchain {
            out.push_str(&format!("toolchain={tc}\n"));
        }
        if let Some(p) = &build_state.profile {
            out.push_str(&format!("profile={p}\n"));
        }
        if let Some(t) = &build_state.target {
            out.push_str(&format!("target={t}\n"));
        }
        if let Some(f) = &build_state.features {
            out.push_str(&format!("features={f}\n"));
        }
        out.push_str(&format!("timestamp={ts}\n"));
        let _ = fs::write(slot_path.join(".carpe-meta"), out);
    }

    pub fn score(&self, current_git: &GitState, current_build: &CargoBuildState) -> u32 {
        [
            (eq_some(&self.head, &current_git.head), 100),
            (eq_some(&self.toolchain, &current_build.toolchain), 50),
            (eq_some(&self.profile, &current_build.profile), 40),
            (eq_some(&self.target, &current_build.target), 40),
            (eq_some(&self.features, &current_build.features), 40),
            (eq_some(&self.branch, &current_git.branch), 20),
        ]
        .into_iter()
        .filter_map(|(same, weight)| same.then_some(weight))
        .sum()
    }
}

/// Two optional values contribute affinity only when both are present and equal.
fn eq_some(a: &Option<String>, b: &Option<String>) -> bool {
    matches!((a, b), (Some(a), Some(b)) if a == b)
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
            toolchain: None,
            profile: Some("release".into()),
            target: Some("wasm32-unknown-unknown".into()),
            features: None,
        };

        SlotMeta::write(&slot_path, &g_state, &b_state);

        let read_meta = SlotMeta::read(&slot_path).expect("meta should be readable");
        assert_eq!(read_meta.head, Some("a1b2c3d4e5f6".into()));
        assert_eq!(read_meta.branch, Some("feature-x".into()));
        assert_eq!(read_meta.profile, Some("release".into()));
        assert_eq!(read_meta.target, Some("wasm32-unknown-unknown".into()));
        assert_eq!(read_meta.score(&g_state, &b_state), 200);

        let _ = fs::remove_dir_all(&slot_path);
    }
}
