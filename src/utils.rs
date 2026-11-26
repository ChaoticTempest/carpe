use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
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
    const OFFSET_BASIS: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;

    let mut hash = OFFSET_BASIS;
    for byte in s.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    format!("{hash:016x}")[..8].to_string()
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
    let parent = |path: &str| PathBuf::from(path).parent().map(Path::to_path_buf);
    for (i, arg) in args.iter().enumerate() {
        if arg == "--manifest-path"
            && let Some(next) = args.get(i + 1)
        {
            return parent(next);
        }
        if let Some(path_str) = arg.strip_prefix("--manifest-path=") {
            return parent(path_str);
        }
    }
    None
}

pub fn normalize_args(raw_args: Vec<String>) -> Vec<String> {
    const SPLIT_FLAGS: &[&str] = &[
        "--features",
        "--package",
        "--target",
        "--profile",
        "--manifest-path",
    ];

    let args = if raw_args.len() == 1 && raw_args[0].contains(' ') {
        shell_words::split(&raw_args[0]).unwrap_or(raw_args)
    } else {
        raw_args
    };

    let mut normalized = Vec::new();
    for arg in args {
        if let Some((flag, rest)) = arg.split_once(' ')
            && SPLIT_FLAGS.contains(&flag)
        {
            normalized.push(flag.to_string());
            normalized.push(rest.trim().to_string());
        } else {
            normalized.push(arg);
        }
    }
    normalized
}

pub fn cargo_bin_dir() -> PathBuf {
    if let Ok(cargo_home) = env::var("CARGO_HOME")
        && !cargo_home.trim().is_empty()
    {
        return PathBuf::from(cargo_home).join("bin");
    }

    let default_home_bin = dirs::home_dir()
        .unwrap_or_else(env::temp_dir)
        .join(".cargo")
        .join("bin");

    if default_home_bin.exists() {
        return default_home_bin;
    }

    if let Ok(path_os) = env::var("PATH") {
        let cargo_name = if cfg!(windows) { "cargo.exe" } else { "cargo" };
        if let Some(dir) = env::split_paths(&path_os).find(|dir| dir.join(cargo_name).is_file()) {
            return dir;
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

pub fn which_bin(name: &str) -> Option<PathBuf> {
    let filename = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    let path_os = env::var("PATH").ok()?;
    env::split_paths(&path_os)
        .find(|dir| dir.join(&filename).is_file())
        .map(|dir| dir.join(&filename))
}

pub fn real_cargo_cmd(args: &[String]) -> Command {
    if let Ok(path) = env::var("CARPE_CARGO_PATH") {
        return Command::new(path);
    }

    let backup = cargo_backup_path();
    if backup.is_file() {
        return Command::new(backup);
    }

    let (toolchain_opt, _) = extract_toolchain(args);
    if let Some(toolchain) = toolchain_opt
        && let Some(rustup_path) = which_bin("rustup")
    {
        let mut cmd = Command::new(rustup_path);
        cmd.arg(format!("+{toolchain}")).arg("cargo");
        return cmd;
    }

    // Both remaining strategies must avoid selecting carpe itself as cargo.
    let Ok(cur_exe) = env::current_exe() else {
        return Command::new("cargo");
    };
    let cur_canon = canonicalize_best_effort(&cur_exe);

    if let Ok(out) = Command::new("rustup").arg("which").arg("cargo").output()
        && out.status.success()
        && let Ok(path_str) = String::from_utf8(out.stdout)
    {
        let path = PathBuf::from(path_str.trim());
        if path.is_file() && canonicalize_best_effort(&path) != cur_canon {
            return Command::new(path);
        }
    }

    let cargo_name = if cfg!(windows) { "cargo.exe" } else { "cargo" };
    if let Ok(path_os) = env::var("PATH")
        && let Some(candidate) = env::split_paths(&path_os)
            .map(|dir| dir.join(cargo_name))
            .find(|candidate| {
                candidate.is_file() && canonicalize_best_effort(candidate) != cur_canon
            })
    {
        return Command::new(candidate);
    }

    Command::new("cargo")
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
    if let Some(first) = args.first()
        && first.starts_with('+')
        && first.len() > 1
    {
        let toolchain = first[1..].to_string();
        return (Some(toolchain), &args[1..]);
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

/// CI detection with an injectable environment reader, so it can be tested
/// without mutating process-wide environment variables.
fn is_ci_env_with(get: impl Fn(&str) -> Option<OsString>) -> bool {
    let truthy = |key: &str| {
        get(key)
            .and_then(|value| value.into_string().ok())
            .is_some_and(|value| value == "true" || value == "1")
    };
    truthy("CI") || truthy("CARPE_CI") || get("CONTINUOUS_INTEGRATION").is_some()
}

pub fn is_ci_environment() -> bool {
    is_ci_env_with(|key| env::var_os(key))
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
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| {
            let p = entry.path();
            // One metadata call per entry (fs::metadata follows symlinks, as
            // Path::is_dir and Path::metadata did before).
            if let Ok(meta) = fs::metadata(&p) {
                if meta.is_dir() {
                    dir_size(&p)
                } else {
                    meta.len()
                }
            } else {
                0
            }
        })
        .sum()
}

pub fn slot_mtime(path: &Path) -> Option<SystemTime> {
    if let Some(ts) = crate::meta::SlotMeta::read(path)
        .filter(|meta| meta.timestamp > 0)
        .map(|meta| meta.timestamp)
    {
        return Some(UNIX_EPOCH + std::time::Duration::from_secs(ts));
    }
    fs::metadata(path).ok().and_then(|m| m.modified().ok())
}

/// Storage budget in GB, shared by the status/info warning and auto-prune.
pub fn max_storage_gb() -> f64 {
    env::var("CARPE_MAX_STORAGE_GB")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(128.0)
}

pub fn check_storage_budget(total_bytes: u64) {
    let max_gb = max_storage_gb();
    let max_bytes = (max_gb * 1024.0 * 1024.0 * 1024.0) as u64;

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
    fn test_short_hash_known_answer() {
        // FNV-1a 64 of "hello" is a430d84680aabd0b; must stay stable across toolchains.
        assert_eq!(short_hash("hello"), "a430d846");
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
        // The total is supplied by the caller's single cache scan.
        check_storage_budget(0);
        check_storage_budget(1024 * 1024);
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
        let ci_with = |vars: &[(&str, &str)]| {
            is_ci_env_with(|key| {
                vars.iter()
                    .find(|(name, _)| *name == key)
                    .map(|(_, value)| OsString::from(*value))
            })
        };
        assert!(ci_with(&[("CI", "true")]));
        assert!(ci_with(&[("CI", "1")]));
        assert!(ci_with(&[("CARPE_CI", "true")]));
        assert!(ci_with(&[("CARPE_CI", "1")]));
        assert!(ci_with(&[("CONTINUOUS_INTEGRATION", "")]));
        assert!(!ci_with(&[("CI", "false")]));
        assert!(!ci_with(&[]));
        assert_eq!(is_ci_environment(), is_ci_env_with(|key| env::var_os(key)));
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

    #[test]
    fn test_real_cargo_cmd() {
        let cmd = real_cargo_cmd(&["build".into()]);
        assert!(!cmd.get_program().to_string_lossy().is_empty());
    }
}
