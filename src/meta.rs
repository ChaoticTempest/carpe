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
        let head = run_git(cwd, &["rev-parse", "HEAD"]);
        let branch = run_git(cwd, &["rev-parse", "--abbrev-ref", "HEAD"]).filter(|b| b != "HEAD");
        GitState { head, branch }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SlotMeta {
    pub head: Option<String>,
    pub branch: Option<String>,
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
            } else if let Some(val) = line.strip_prefix("timestamp=") {
                if let Ok(ts) = val.parse::<u64>() {
                    meta.timestamp = ts;
                }
            }
        }
        Some(meta)
    }

    pub fn write(slot_path: &Path, git_state: &GitState) {
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
        out.push_str(&format!("timestamp={ts}\n"));
        let _ = fs::write(slot_path.join(".carpe-meta"), out);
    }

    pub fn score(&self, current_git: &GitState) -> u32 {
        let mut score = 0;
        if let (Some(h1), Some(h2)) = (&self.head, &current_git.head) {
            if h1 == h2 {
                score += 100;
            }
        }
        if let (Some(b1), Some(b2)) = (&self.branch, &current_git.branch) {
            if b1 == b2 {
                score += 50;
            }
        }
        score
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    fn rand_nonce() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64
    }

    #[test]
    fn test_slot_meta_read_write() {
        let sys_temp = env::temp_dir();
        let slot_path = sys_temp.join(format!("carpe_meta_test_{}", rand_nonce()));
        fs::create_dir_all(&slot_path).unwrap();

        let state = GitState {
            head: Some("a1b2c3d4e5f6".into()),
            branch: Some("feature-x".into()),
        };

        SlotMeta::write(&slot_path, &state);

        let read_meta = SlotMeta::read(&slot_path).expect("meta should be readable");
        assert_eq!(read_meta.head, Some("a1b2c3d4e5f6".into()));
        assert_eq!(read_meta.branch, Some("feature-x".into()));
        assert_eq!(read_meta.score(&state), 150);

        let _ = fs::remove_dir_all(&slot_path);
    }
}
