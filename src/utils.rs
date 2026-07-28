use std::collections::hash_map::DefaultHasher;
use std::env;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub fn canonicalize_best_effort(p: &Path) -> PathBuf {
    fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

pub fn cache_root() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(env::temp_dir)
        .join("carpe")
}

pub fn sanitize(s: &str) -> String {
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

pub fn short_hash(s: &str) -> String {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    format!("{:016x}", h.finish())[..8].to_string()
}

pub fn has_explicit_target_dir_flag(args: &[String]) -> bool {
    for arg in args {
        if arg == "--target-dir" || arg.starts_with("--target-dir=") {
            return true;
        }
    }
    false
}

pub fn dir_size(path: &Path) -> u64 {
    let mut size = 0;
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                size += dir_size(&p);
            } else if let Ok(meta) = p.metadata() {
                size += meta.len();
            }
        }
    }
    size
}

pub fn latest_mtime(path: &Path) -> Option<SystemTime> {
    let mut max_time: Option<SystemTime> = None;
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if let Ok(meta) = p.metadata() {
                if let Ok(t) = meta.modified() {
                    max_time = match max_time {
                        Some(current) => Some(current.max(t)),
                        None => Some(t),
                    };
                }
            }
            if p.is_dir() {
                if let Some(child_max) = latest_mtime(&p) {
                    max_time = match max_time {
                        Some(current) => Some(current.max(child_max)),
                        None => Some(child_max),
                    };
                }
            }
        }
    }
    max_time
}

pub fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

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

pub fn format_time_ago(time: Option<SystemTime>) -> String {
    let Some(t) = time else {
        return "never".to_string();
    };
    let Ok(elapsed) = t.elapsed() else {
        return "recently".to_string();
    };

    let secs = elapsed.as_secs();
    if secs < 60 {
        "just now".to_string()
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
