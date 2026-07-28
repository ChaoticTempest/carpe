use std::collections::hash_map::DefaultHasher;
use std::env;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use dialoguer::console::style;

pub fn cyan(s: impl std::fmt::Display) -> String {
    style(s).cyan().to_string()
}

pub fn green(s: impl std::fmt::Display) -> String {
    style(s).green().to_string()
}

pub fn yellow(s: impl std::fmt::Display) -> String {
    style(s).yellow().to_string()
}

pub fn bold(s: impl std::fmt::Display) -> String {
    style(s).bold().to_string()
}

pub fn dim(s: impl std::fmt::Display) -> String {
    style(s).dim().to_string()
}

pub fn status_header(action: &str) -> String {
    style(format!("{action:>12}")).green().bold().to_string()
}

pub fn warning_header(action: &str) -> String {
    style(format!("{action:>12}")).yellow().bold().to_string()
}

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

pub fn extract_manifest_path(args: &[String]) -> Option<PathBuf> {
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--manifest-path" {
            if i + 1 < args.len() {
                let p = PathBuf::from(&args[i + 1]);
                return p.parent().map(Path::to_path_buf);
            }
        } else if let Some(path_str) = args[i].strip_prefix("--manifest-path=") {
            let p = PathBuf::from(path_str);
            return p.parent().map(Path::to_path_buf);
        }
        i += 1;
    }
    None
}

pub fn normalize_args(raw_args: Vec<String>) -> Vec<String> {
    let args = if raw_args.len() == 1 && raw_args[0].contains(' ') {
        shell_words::split(&raw_args[0]).unwrap_or(raw_args)
    } else {
        raw_args
    };

    let mut normalized = Vec::new();
    for arg in args {
        if let Some(rest) = arg.strip_prefix("--features ") {
            normalized.push("--features".to_string());
            normalized.push(rest.trim().to_string());
        } else if let Some(rest) = arg.strip_prefix("--package ") {
            normalized.push("--package".to_string());
            normalized.push(rest.trim().to_string());
        } else if let Some(rest) = arg.strip_prefix("--target ") {
            normalized.push("--target".to_string());
            normalized.push(rest.trim().to_string());
        } else if let Some(rest) = arg.strip_prefix("--profile ") {
            normalized.push("--profile".to_string());
            normalized.push(rest.trim().to_string());
        } else if let Some(rest) = arg.strip_prefix("--manifest-path ") {
            normalized.push("--manifest-path".to_string());
            normalized.push(rest.trim().to_string());
        } else {
            normalized.push(arg);
        }
    }
    normalized
}

pub fn cargo_bin_dir() -> PathBuf {
    if let Ok(cargo_home) = env::var("CARGO_HOME") {
        if !cargo_home.trim().is_empty() {
            return PathBuf::from(cargo_home).join("bin");
        }
    }

    let default_home_bin = dirs::home_dir()
        .unwrap_or_else(env::temp_dir)
        .join(".cargo")
        .join("bin");

    if default_home_bin.exists() {
        return default_home_bin;
    }

    if let Ok(path_os) = env::var("PATH") {
        for dir in env::split_paths(&path_os) {
            let candidate = dir.join(if cfg!(windows) { "cargo.exe" } else { "cargo" });
            if candidate.is_file() {
                return dir;
            }
        }
    }

    default_home_bin
}

pub fn cargo_shim_path() -> PathBuf {
    cargo_bin_dir().join(if cfg!(windows) { "cargo.exe" } else { "cargo" })
}

pub fn cargo_backup_path() -> PathBuf {
    cargo_bin_dir().join(if cfg!(windows) {
        "cargo.real.exe"
    } else {
        "cargo.real"
    })
}

pub fn is_override_enabled() -> bool {
    let shim_path = cargo_shim_path();
    if let Ok(content) = fs::read_to_string(&shim_path) {
        content.contains("# carpe-shim") || content.contains("rem carpe-shim")
    } else if let Ok(target) = fs::read_link(&shim_path) {
        target.to_string_lossy().contains("carpe")
    } else {
        false
    }
}

pub fn run_toggle_override() {
    let shim_path = cargo_shim_path();
    let backup_path = cargo_backup_path();

    if is_override_enabled() {
        if backup_path.exists() {
            let _ = fs::rename(&backup_path, &shim_path);
        } else {
            let _ = fs::remove_file(&shim_path);
        }
        println!("disabling cargo=carpe");
    } else {
        if shim_path.exists() && !backup_path.exists() {
            let _ = fs::rename(&shim_path, &backup_path);
        }

        let cur_exe = env::current_exe()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "carpe".to_string());

        if let Some(parent) = shim_path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let script = format!("#!/bin/sh\n# carpe-shim\nexec \"{cur_exe}\" \"$@\"\n");
            if fs::write(&shim_path, script).is_ok() {
                let _ = fs::set_permissions(&shim_path, fs::Permissions::from_mode(0o755));
            }
        }

        #[cfg(not(unix))]
        {
            let script = format!("@echo off\r\nrem carpe-shim\r\n\"{cur_exe}\" %*\r\n");
            let cmd_path = shim_path.with_extension("cmd");
            let _ = fs::write(&cmd_path, &script);
            let _ = fs::write(&shim_path, &script);
        }

        println!("enabling cargo=carpe");
    }
}

pub fn extract_toolchain(args: &[String]) -> (Option<String>, &[String]) {
    if let Some(first) = args.first() {
        if first.starts_with('+') && first.len() > 1 {
            let toolchain = first[1..].to_string();
            return (Some(toolchain), &args[1..]);
        }
    }
    (None, args)
}

pub fn is_non_building_subcommand(args: &[String]) -> bool {
    let (_, sub_args) = extract_toolchain(args);
    let Some(first) = sub_args.first() else {
        return false;
    };
    let cmd = first.as_str();
    matches!(
        cmd,
        "fmt"
            | "add"
            | "remove"
            | "rm"
            | "metadata"
            | "tree"
            | "new"
            | "init"
            | "publish"
            | "search"
            | "login"
            | "logout"
            | "owner"
            | "vendor"
            | "yank"
    )
}

pub fn is_ci_environment() -> bool {
    env::var("CI")
        .map(|s| s == "true" || s == "1")
        .unwrap_or(false)
        || env::var("CARPE_CI")
            .map(|s| s == "true" || s == "1")
            .unwrap_or(false)
        || env::var_os("CONTINUOUS_INTEGRATION").is_some()
}

#[cfg(not(unix))]
pub fn exit_with_status(status: std::process::ExitStatus) -> ! {
    std::process::exit(status.code().unwrap_or(1));
}

#[cfg(test)]
pub fn test_rand_nonce() -> u64 {
    use std::time::UNIX_EPOCH;
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
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

pub fn slot_mtime(path: &Path) -> Option<SystemTime> {
    let meta_path = path.join(".carpe-meta");
    if let Ok(content) = fs::read_to_string(&meta_path) {
        for line in content.lines() {
            if let Some(val) = line.strip_prefix("timestamp=") {
                if let Ok(ts) = val.parse::<u64>() {
                    return Some(UNIX_EPOCH + std::time::Duration::from_secs(ts));
                }
            }
        }
    }
    fs::metadata(path).ok().and_then(|m| m.modified().ok())
}

pub fn check_storage_budget(root: &Path) {
    if !root.exists() {
        return;
    }

    let max_gb: f64 = env::var("CARPE_MAX_STORAGE_GB")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(128.0);

    let max_bytes = (max_gb * 1024.0 * 1024.0 * 1024.0) as u64;

    let mut total_bytes = 0u64;
    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                total_bytes += dir_size(&path);
            }
        }
    }

    if total_bytes > max_bytes {
        eprintln!(
            "\n{}",
            yellow(format!(
                "carpe: warning: total cache size across all pools is {} (exceeds {:.1} GB storage budget)",
                format_size(total_bytes),
                max_gb
            ))
        );
        eprintln!("        run 'carpe prune -a' or 'carpe prune --lru 2' to reclaim disk space.");
    }
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

    #[test]
    fn test_extract_manifest_path() {
        let p1 = extract_manifest_path(&[
            "build".into(),
            "--manifest-path".into(),
            "/my/project/Cargo.toml".into(),
        ]);
        assert_eq!(p1, Some(PathBuf::from("/my/project")));

        let p2 = extract_manifest_path(&[
            "build".into(),
            "--manifest-path=/my/project/Cargo.toml".into(),
        ]);
        assert_eq!(p2, Some(PathBuf::from("/my/project")));

        let p3 = extract_manifest_path(&["build".into(), "--release".into()]);
        assert_eq!(p3, None);
    }

    #[test]
    fn test_is_non_building_subcommand() {
        assert!(is_non_building_subcommand(&["fmt".into()]));
        assert!(is_non_building_subcommand(&["add".into(), "serde".into()]));
        assert!(is_non_building_subcommand(&["metadata".into()]));
        assert!(is_non_building_subcommand(&["tree".into()]));
        assert!(!is_non_building_subcommand(&["build".into()]));
        assert!(!is_non_building_subcommand(&["test".into()]));
        assert!(!is_non_building_subcommand(&["check".into()]));
    }

    #[test]
    fn test_check_storage_budget() {
        let sys_temp = env::temp_dir();
        let root = sys_temp.join(format!("carpe_budget_test_{}", test_rand_nonce()));
        fs::create_dir_all(&root).unwrap();

        // Should execute smoothly without crashing
        check_storage_budget(&root);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn test_extract_toolchain() {
        let args1 = vec!["+1.93.0".to_string(), "check".to_string()];
        let (tc1, rest1) = extract_toolchain(&args1);
        assert_eq!(tc1, Some("1.93.0".to_string()));
        assert_eq!(rest1, &["check"]);

        let args2 = vec!["build".to_string(), "--release".to_string()];
        let (tc2, rest2) = extract_toolchain(&args2);
        assert_eq!(tc2, None);
        assert_eq!(rest2, &["build", "--release"]);
    }

    #[test]
    fn test_normalize_args() {
        let raw1 = vec!["--features test-feature,debug-page".to_string()];
        let norm1 = normalize_args(raw1);
        assert_eq!(norm1, vec!["--features", "test-feature,debug-page"]);

        let raw2 = vec!["build --release --features foo".to_string()];
        let norm2 = normalize_args(raw2);
        assert_eq!(norm2, vec!["build", "--release", "--features", "foo"]);
    }

    #[test]
    fn test_is_ci_environment() {
        env::set_var("CARPE_CI", "true");
        assert!(is_ci_environment());
        env::remove_var("CARPE_CI");
    }

    #[test]
    fn test_toggle_override_helpers() {
        assert_eq!(
            cargo_shim_path().file_name().unwrap(),
            if cfg!(windows) { "cargo.exe" } else { "cargo" }
        );
        assert_eq!(
            cargo_backup_path().file_name().unwrap(),
            if cfg!(windows) {
                "cargo.real.exe"
            } else {
                "cargo.real"
            }
        );
    }
}
