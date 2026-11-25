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
            } else if arg == "--profile" {
                if i + 1 < sub_args.len() {
                    profile = Some(sub_args[i + 1].clone());
                    i += 1;
                }
            } else if let Some(p) = arg.strip_prefix("--profile=") {
                profile = Some(p.to_string());
            } else if arg == "--target" {
                if i + 1 < sub_args.len() {
                    target = Some(sub_args[i + 1].clone());
                    i += 1;
                }
            } else if let Some(t) = arg.strip_prefix("--target=") {
                target = Some(t.to_string());
            } else if arg == "--features" || arg == "-F" {
                if i + 1 < sub_args.len() {
                    features = Some(sub_args[i + 1].clone());
                    i += 1;
                }
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
            if let Some(val) = line.strip_prefix("head=") {
                meta.head = Some(val.to_string());
            } else if let Some(val) = line.strip_prefix("branch=") {
                meta.branch = Some(val.to_string());
            } else if let Some(val) = line.strip_prefix("toolchain=") {
                meta.toolchain = Some(val.to_string());
            } else if let Some(val) = line.strip_prefix("profile=") {
                meta.profile = Some(val.to_string());
            } else if let Some(val) = line.strip_prefix("target=") {
                meta.target = Some(val.to_string());
            } else if let Some(val) = line.strip_prefix("features=") {
                meta.features = Some(val.to_string());
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
        let mut score = 0;
        if matches!((&self.head, &current_git.head), (Some(h1), Some(h2)) if h1 == h2) {
            score += 100;
        }
        if matches!((&self.toolchain, &current_build.toolchain), (Some(tc1), Some(tc2)) if tc1 == tc2)
        {
            score += 50;
        }
        if matches!((&self.profile, &current_build.profile), (Some(p1), Some(p2)) if p1 == p2) {
            score += 40;
        }
        if matches!((&self.target, &current_build.target), (Some(t1), Some(t2)) if t1 == t2) {
            score += 40;
        }
        if matches!((&self.features, &current_build.features), (Some(f1), Some(f2)) if f1 == f2) {
            score += 40;
        }
        if matches!((&self.branch, &current_git.branch), (Some(b1), Some(b2)) if b1 == b2) {
            score += 20;
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

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn test_parse_profile_target_and_features_variants() {
        let release_short = CargoBuildState::parse(&argv(&["build", "-r"]));
        assert_eq!(release_short.profile.as_deref(), Some("release"));

        let profile_split = CargoBuildState::parse(&argv(&["build", "--profile", "custom"]));
        assert_eq!(profile_split.profile.as_deref(), Some("custom"));

        let profile_eq = CargoBuildState::parse(&argv(&["build", "--profile=custom"]));
        assert_eq!(profile_eq.profile.as_deref(), Some("custom"));

        let target_split =
            CargoBuildState::parse(&argv(&["build", "--target", "wasm32-unknown-unknown"]));
        assert_eq!(
            target_split.target.as_deref(),
            Some("wasm32-unknown-unknown")
        );

        let target_eq =
            CargoBuildState::parse(&argv(&["build", "--target=wasm32-unknown-unknown"]));
        assert_eq!(target_eq.target.as_deref(), Some("wasm32-unknown-unknown"));

        let features_split = CargoBuildState::parse(&argv(&["build", "--features", "a,b"]));
        assert_eq!(features_split.features.as_deref(), Some("a,b"));

        let features_eq = CargoBuildState::parse(&argv(&["build", "--features=a,b"]));
        assert_eq!(features_eq.features.as_deref(), Some("a,b"));

        let features_short = CargoBuildState::parse(&argv(&["build", "-F", "serde"]));
        assert_eq!(features_short.features.as_deref(), Some("serde"));

        let toolchain = CargoBuildState::parse(&argv(&["+nightly", "build", "--features", "a"]));
        assert_eq!(toolchain.toolchain.as_deref(), Some("nightly"));
        assert_eq!(toolchain.features.as_deref(), Some("a"));
    }

    #[test]
    fn test_parse_missing_flag_values_and_defaults() {
        // A flag at the end of argv has no value to consume.
        assert_eq!(
            CargoBuildState::parse(&argv(&["build", "--profile"]))
                .profile
                .as_deref(),
            Some("debug")
        );
        assert_eq!(
            CargoBuildState::parse(&argv(&["build", "--target"])).target,
            None
        );
        assert_eq!(
            CargoBuildState::parse(&argv(&["build", "--features"])).features,
            None
        );
        assert_eq!(
            CargoBuildState::parse(&argv(&["build", "-F"])).features,
            None
        );

        // `--profile` consumes the following token even when it looks like a flag.
        let consumed = CargoBuildState::parse(&argv(&["build", "--profile", "--target"]));
        assert_eq!(consumed.profile.as_deref(), Some("--target"));
        assert_eq!(consumed.target, None);

        let defaults = CargoBuildState::parse(&argv(&["build"]));
        assert_eq!(
            defaults,
            CargoBuildState {
                toolchain: None,
                profile: Some("debug".into()),
                target: None,
                features: None,
            }
        );
    }

    #[test]
    fn test_slot_meta_read_missing_and_malformed() {
        let sys_temp = env::temp_dir();
        let slot = sys_temp.join(format!("carpe_meta_missing_{}", test_rand_nonce()));
        assert!(SlotMeta::read(&slot).is_none(), "missing meta file -> None");

        fs::create_dir_all(&slot).unwrap();
        fs::write(
            slot.join(".carpe-meta"),
            "head=abc\nbranch=main\nprofile=release\ntimestamp=not-a-number\nunknown=ignored\n",
        )
        .unwrap();
        let meta = SlotMeta::read(&slot).unwrap();
        assert_eq!(meta.head.as_deref(), Some("abc"));
        assert_eq!(meta.branch.as_deref(), Some("main"));
        assert_eq!(meta.profile.as_deref(), Some("release"));
        assert_eq!(meta.timestamp, 0, "malformed timestamp falls back to 0");
        assert_eq!(meta.target, None);
        assert_eq!(meta.features, None);

        fs::remove_dir_all(&slot).unwrap();
    }

    #[test]
    fn test_slot_meta_write_read_all_fields() {
        let sys_temp = env::temp_dir();
        let slot = sys_temp.join(format!("carpe_meta_full_{}", test_rand_nonce()));
        fs::create_dir_all(&slot).unwrap();

        let git = GitState {
            head: Some("deadbeef".into()),
            branch: Some("topic".into()),
        };
        let build = CargoBuildState {
            toolchain: Some("stable".into()),
            profile: Some("release".into()),
            target: Some("x86_64-apple-darwin".into()),
            features: Some("a,b".into()),
        };
        SlotMeta::write(&slot, &git, &build);
        let meta = SlotMeta::read(&slot).unwrap();
        assert_eq!(meta.head.as_deref(), Some("deadbeef"));
        assert_eq!(meta.branch.as_deref(), Some("topic"));
        assert_eq!(meta.toolchain.as_deref(), Some("stable"));
        assert_eq!(meta.profile.as_deref(), Some("release"));
        assert_eq!(meta.target.as_deref(), Some("x86_64-apple-darwin"));
        assert_eq!(meta.features.as_deref(), Some("a,b"));
        assert!(meta.timestamp > 0);

        fs::remove_dir_all(&slot).unwrap();
    }

    #[test]
    fn test_slot_meta_score_covers_every_field() {
        let git = GitState {
            head: Some("h".into()),
            branch: Some("b".into()),
        };
        let build = CargoBuildState {
            toolchain: Some("t".into()),
            profile: Some("p".into()),
            target: Some("x".into()),
            features: Some("f".into()),
        };
        let full = SlotMeta {
            head: Some("h".into()),
            branch: Some("b".into()),
            toolchain: Some("t".into()),
            profile: Some("p".into()),
            target: Some("x".into()),
            features: Some("f".into()),
            timestamp: 0,
        };
        assert_eq!(full.score(&git, &build), 290);

        let mut no_head = full.clone();
        no_head.head = None;
        assert_eq!(no_head.score(&git, &build), 190, "head is worth 100");

        let mut no_toolchain = full.clone();
        no_toolchain.toolchain = None;
        assert_eq!(
            no_toolchain.score(&git, &build),
            240,
            "toolchain is worth 50"
        );

        let mut no_profile = full.clone();
        no_profile.profile = None;
        assert_eq!(no_profile.score(&git, &build), 250, "profile is worth 40");

        let mut no_target = full.clone();
        no_target.target = None;
        assert_eq!(no_target.score(&git, &build), 250, "target is worth 40");

        let mut no_features = full.clone();
        no_features.features = None;
        assert_eq!(no_features.score(&git, &build), 250, "features is worth 40");

        let mut no_branch = full.clone();
        no_branch.branch = None;
        assert_eq!(no_branch.score(&git, &build), 270, "branch is worth 20");

        let mut mismatched = full;
        mismatched.head = Some("different".into());
        mismatched.features = Some("different".into());
        assert_eq!(mismatched.score(&git, &build), 150);
    }

    #[test]
    fn test_git_state_detect_in_and_outside_git() {
        let sys_temp = env::temp_dir();
        let non_git = sys_temp.join(format!("carpe_nongit_{}", test_rand_nonce()));
        fs::create_dir_all(&non_git).unwrap();
        assert_eq!(
            GitState::detect(&non_git),
            GitState {
                head: None,
                branch: None
            }
        );
        fs::remove_dir_all(&non_git).unwrap();

        let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
        let detected = GitState::detect(repo);
        assert!(detected.head.is_some(), "crate root is a git worktree");
    }
}
