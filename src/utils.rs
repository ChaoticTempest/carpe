use std::env;
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

pub fn which_bin(name: &str) -> Option<PathBuf> {
    let filename = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    if let Ok(path_os) = env::var("PATH") {
        for dir in env::split_paths(&path_os) {
            let candidate = dir.join(&filename);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

pub fn real_cargo_cmd(args: &[String]) -> Command {
    if let Ok(path) = env::var("CARPE_CARGO_PATH") {
        let mut cmd = Command::new(path);
        cmd.env("_CARPE_RECURSION_GUARD", "1");
        return cmd;
    }

    let backup = cargo_backup_path();
    if backup.is_file() {
        let mut cmd = Command::new(backup);
        cmd.env("_CARPE_RECURSION_GUARD", "1");
        return cmd;
    }

    let (toolchain_opt, _) = extract_toolchain(args);
    if let Some(toolchain) = toolchain_opt {
        if let Some(rustup_path) = which_bin("rustup") {
            let mut cmd = Command::new(rustup_path);
            cmd.arg(format!("+{toolchain}")).arg("cargo");
            cmd.env("_CARPE_RECURSION_GUARD", "1");
            return cmd;
        }
    }

    if let Ok(rustup_cargo) = Command::new("rustup").arg("which").arg("cargo").output() {
        if rustup_cargo.status.success() {
            if let Ok(path_str) = String::from_utf8(rustup_cargo.stdout) {
                let path = PathBuf::from(path_str.trim());
                if let Ok(cur_exe) = env::current_exe() {
                    let cur_canon = canonicalize_best_effort(&cur_exe);
                    let path_canon = canonicalize_best_effort(&path);
                    if path_canon != cur_canon && path.is_file() {
                        let mut cmd = Command::new(path);
                        cmd.env("_CARPE_RECURSION_GUARD", "1");
                        return cmd;
                    }
                }
            }
        }
    }

    if let Ok(cur_exe) = env::current_exe() {
        let cur_canon = canonicalize_best_effort(&cur_exe);
        if let Ok(path_os) = env::var("PATH") {
            for dir in env::split_paths(&path_os) {
                let candidate = dir.join(if cfg!(windows) { "cargo.exe" } else { "cargo" });
                let candidate_canon = canonicalize_best_effort(&candidate);
                if candidate_canon != cur_canon && candidate.is_file() {
                    let mut cmd = Command::new(candidate);
                    cmd.env("_CARPE_RECURSION_GUARD", "1");
                    return cmd;
                }
            }
        }
    }

    let mut cmd = Command::new("cargo");
    cmd.env("_CARPE_RECURSION_GUARD", "1");
    cmd
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
            // One metadata call per entry (fs::metadata follows symlinks, as
            // Path::is_dir and Path::metadata did before).
            if let Ok(meta) = fs::metadata(&p) {
                if meta.is_dir() {
                    size += dir_size(&p);
                } else {
                    size += meta.len();
                }
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

pub fn check_storage_budget(total_bytes: u64) {
    let max_gb: f64 = env::var("CARPE_MAX_STORAGE_GB")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(128.0);

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
        let sys_temp = env::temp_dir();
        let root = sys_temp.join(format!("carpe_budget_test_{}", test_rand_nonce()));
        fs::create_dir_all(&root).unwrap();

        // The total is supplied by the caller's single cache scan.
        check_storage_budget(dir_size(&root));
        check_storage_budget(1024 * 1024);

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

    const CHILD_CASE: &str = "CARPE_UTILS_CHILD_CASE";
    const CHILD_DIR: &str = "CARPE_UTILS_CHILD_DIR";

    /// Runs one test function in a fresh process with a fully controlled
    /// environment. Unit tests therefore never mutate env vars while other
    /// tests are running in parallel.
    fn run_child(test: &str, case: &str, envs: &[(&str, &str)], removes: &[&str]) {
        let exe = env::current_exe().expect("current test executable");
        let mut cmd = Command::new(exe);
        cmd.args(["--exact", test, "--nocapture"])
            .env(CHILD_CASE, case);
        for (key, value) in envs {
            cmd.env(key, value);
        }
        for key in removes {
            cmd.env_remove(key);
        }
        let out = cmd.output().expect("spawn isolated child test");
        assert!(
            out.status.success(),
            "child case {case} failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn temp_dir(label: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!("carpe_utils_{label}_{}", test_rand_nonce()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn test_is_ci_environment() {
        if let Ok(case) = env::var(CHILD_CASE) {
            let expected = !matches!(case.as_str(), "none" | "ci_false");
            assert_eq!(is_ci_environment(), expected, "case {case}");
            return;
        }

        let test = "utils::tests::test_is_ci_environment";
        run_child(
            test,
            "none",
            &[],
            &["CI", "CARPE_CI", "CONTINUOUS_INTEGRATION"],
        );
        run_child(
            test,
            "ci",
            &[("CI", "true")],
            &["CARPE_CI", "CONTINUOUS_INTEGRATION"],
        );
        run_child(
            test,
            "ci_one",
            &[("CI", "1")],
            &["CARPE_CI", "CONTINUOUS_INTEGRATION"],
        );
        run_child(
            test,
            "carpe_ci",
            &[("CARPE_CI", "true")],
            &["CI", "CONTINUOUS_INTEGRATION"],
        );
        run_child(
            test,
            "ci_false",
            &[("CI", "false")],
            &["CARPE_CI", "CONTINUOUS_INTEGRATION"],
        );
        run_child(
            test,
            "continuous",
            &[("CONTINUOUS_INTEGRATION", "1")],
            &["CI", "CARPE_CI"],
        );
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
    fn test_toggle_override_in_isolated_cargo_home() {
        if let Ok(case) = env::var(CHILD_CASE) {
            let before = is_override_enabled();
            run_toggle_override();
            let after = is_override_enabled();
            match case.as_str() {
                "enable" => {
                    assert!(!before);
                    assert!(after, "shim must be detected after enabling");
                    assert!(cargo_backup_path().exists());
                }
                "disable" => {
                    assert!(before);
                    assert!(!after, "shim must be gone after disabling");
                    assert!(!cargo_backup_path().exists());
                }
                other => panic!("unknown case {other}"),
            }
            return;
        }

        let dir = temp_dir("toggle");
        let bin = dir.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let shim = bin.join(if cfg!(windows) { "cargo.exe" } else { "cargo" });
        let original = "ORIGINAL REAL CARGO\n";
        fs::write(&shim, original).unwrap();

        let home = dir.to_str().unwrap();
        let test = "utils::tests::test_toggle_override_in_isolated_cargo_home";
        run_child(
            test,
            "enable",
            &[("CARGO_HOME", home)],
            &["CARPE_CARGO_PATH"],
        );
        run_child(
            test,
            "disable",
            &[("CARGO_HOME", home)],
            &["CARPE_CARGO_PATH"],
        );

        assert_eq!(fs::read_to_string(&shim).unwrap(), original);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_real_cargo_cmd() {
        use std::ffi::OsStr;

        if let Ok(case) = env::var(CHILD_CASE) {
            let args: Vec<String> = if case == "rustup_toolchain" {
                vec!["+nightly".into(), "build".into()]
            } else {
                vec!["build".into()]
            };
            let cmd = real_cargo_cmd(&args);
            let guard_set = cmd.get_envs().any(|(key, value)| {
                key == OsStr::new("_CARPE_RECURSION_GUARD") && value == Some(OsStr::new("1"))
            });
            assert!(guard_set, "real cargo must carry the recursion guard");
            match case.as_str() {
                "override" => assert_eq!(cmd.get_program(), Path::new("/fake/cargo")),
                "backup" => assert_eq!(cmd.get_program(), cargo_backup_path()),
                "fallback" => assert_eq!(cmd.get_program(), Path::new("cargo")),
                #[cfg(unix)]
                "rustup_toolchain" => {
                    assert_eq!(cmd.get_program(), which_bin("rustup").unwrap())
                }
                #[cfg(unix)]
                "rustup_which" => {
                    let expected = Path::new(&env::var(CHILD_DIR).unwrap()).join("fake-cargo");
                    assert_eq!(cmd.get_program(), expected);
                }
                other => panic!("unknown case {other}"),
            }
            return;
        }

        let test = "utils::tests::test_real_cargo_cmd";

        // CARPE_CARGO_PATH takes precedence over everything else.
        run_child(
            test,
            "override",
            &[("CARPE_CARGO_PATH", "/fake/cargo")],
            &["CARGO_HOME"],
        );

        // A backed-up shim (cargo.real) is used next.
        let backup_home = temp_dir("backup");
        let backup = backup_home.join("bin").join(if cfg!(windows) {
            "cargo.real.exe"
        } else {
            "cargo.real"
        });
        fs::create_dir_all(backup.parent().unwrap()).unwrap();
        fs::write(&backup, "#!/bin/sh\n").unwrap();
        run_child(
            test,
            "backup",
            &[("CARGO_HOME", backup_home.to_str().unwrap())],
            &["CARPE_CARGO_PATH"],
        );

        // With an empty PATH and no backup, fall back to the name "cargo".
        let empty_home = temp_dir("emptyhome");
        run_child(
            test,
            "fallback",
            &[("CARGO_HOME", empty_home.to_str().unwrap()), ("PATH", "")],
            &["CARPE_CARGO_PATH"],
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let tool_dir = temp_dir("rustup");
            let rustup = tool_dir.join("rustup");
            fs::write(
                &rustup,
                format!(
                    "#!/bin/sh\necho {}\n",
                    tool_dir.join("fake-cargo").display()
                ),
            )
            .unwrap();
            fs::set_permissions(&rustup, fs::Permissions::from_mode(0o755)).unwrap();
            fs::write(tool_dir.join("fake-cargo"), "#!/bin/sh\necho fake\n").unwrap();

            for case in ["rustup_toolchain", "rustup_which"] {
                run_child(
                    test,
                    case,
                    &[
                        ("CARGO_HOME", empty_home.to_str().unwrap()),
                        ("PATH", tool_dir.to_str().unwrap()),
                        (CHILD_DIR, tool_dir.to_str().unwrap()),
                    ],
                    &["CARPE_CARGO_PATH"],
                );
            }
            fs::remove_dir_all(&tool_dir).unwrap();
        }

        fs::remove_dir_all(&backup_home).unwrap();
        fs::remove_dir_all(&empty_home).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn test_cargo_bin_dir_falls_back_to_path() {
        if env::var(CHILD_CASE).is_ok() {
            assert_eq!(
                cargo_bin_dir(),
                Path::new(&env::var(CHILD_DIR).unwrap()).join("bin")
            );
            return;
        }
        let dir = temp_dir("bindir");
        let bin = dir.join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join("cargo"), "#!/bin/sh\n").unwrap();
        run_child(
            "utils::tests::test_cargo_bin_dir_falls_back_to_path",
            "scan",
            &[
                ("HOME", dir.to_str().unwrap()),
                ("PATH", bin.to_str().unwrap()),
                (CHILD_DIR, dir.to_str().unwrap()),
            ],
            &["CARGO_HOME"],
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_format_time_ago_all_branches() {
        assert_eq!(format_time_ago(None), "never");
        assert_eq!(
            format_time_ago(Some(
                SystemTime::now() + std::time::Duration::from_secs(3600)
            )),
            "recently"
        );
        assert_eq!(format_time_ago(Some(SystemTime::now())), "just now");
        assert_eq!(
            format_time_ago(Some(
                SystemTime::now() - std::time::Duration::from_secs(180)
            )),
            "3m ago"
        );
        assert_eq!(
            format_time_ago(Some(
                SystemTime::now() - std::time::Duration::from_secs(7200)
            )),
            "2h ago"
        );
        assert_eq!(
            format_time_ago(Some(
                SystemTime::now() - std::time::Duration::from_secs(3 * 86400)
            )),
            "3d ago"
        );
    }

    #[test]
    fn test_dir_size_nested_and_missing() {
        let dir = temp_dir("dirsize");
        let nested = dir.join("a").join("b");
        fs::create_dir_all(&nested).unwrap();
        fs::write(dir.join("top.bin"), vec![0u8; 10]).unwrap();
        fs::write(nested.join("deep.bin"), vec![0u8; 25]).unwrap();
        assert_eq!(dir_size(&dir), 35);
        assert_eq!(dir_size(&dir.join("not-here")), 0);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_slot_mtime_meta_and_directory_fallback() {
        let dir = temp_dir("slotmtime");
        let meta = dir.join(".carpe-meta");
        fs::write(&meta, "timestamp=1000000000\n").unwrap();
        assert_eq!(
            slot_mtime(&dir),
            Some(UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000))
        );

        fs::remove_file(&meta).unwrap();
        let before = SystemTime::now() - std::time::Duration::from_secs(2);
        assert!(slot_mtime(&dir).unwrap() >= before, "dir mtime fallback");

        fs::write(&meta, "timestamp=broken\n").unwrap();
        assert!(
            slot_mtime(&dir).unwrap() >= before,
            "malformed meta -> dir mtime"
        );
        assert!(slot_mtime(&dir.join("missing")).is_none());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_normalize_args_all_supported_flags() {
        assert_eq!(
            normalize_args(vec!["--package my-crate".into()]),
            vec!["--package", "my-crate"]
        );
        assert_eq!(
            normalize_args(vec!["--target wasm32-unknown-unknown".into()]),
            vec!["--target", "wasm32-unknown-unknown"]
        );
        assert_eq!(
            normalize_args(vec!["--profile release".into()]),
            vec!["--profile", "release"]
        );
        assert_eq!(
            normalize_args(vec!["--manifest-path /x/Cargo.toml".into()]),
            vec!["--manifest-path", "/x/Cargo.toml"]
        );
        assert_eq!(
            normalize_args(vec!["--features a,b".into()]),
            vec!["--features", "a,b"]
        );
        assert_eq!(
            normalize_args(vec!["build".into(), "--release".into()]),
            vec!["build", "--release"]
        );
    }

    #[test]
    fn test_extract_manifest_path_edge_cases() {
        assert_eq!(
            extract_manifest_path(&["build".into(), "--manifest-path".into()]),
            None
        );
        assert_eq!(
            extract_manifest_path(&["build".into(), "--manifest-path=/Cargo.toml".into()]),
            Some(PathBuf::from("/"))
        );
        assert_eq!(
            extract_manifest_path(&["build".into(), "--manifest-path=rel/Cargo.toml".into()]),
            Some(PathBuf::from("rel"))
        );
    }

    #[test]
    fn test_target_dir_flag_edge_cases() {
        assert!(!has_explicit_target_dir_flag(&[]));
        assert!(has_explicit_target_dir_flag(&["--target-dir=/x".into()]));
        assert!(!has_explicit_target_dir_flag(&[
            "--target".into(),
            "x86_64".into()
        ]));
    }

    #[test]
    fn test_non_building_subcommand_more_cases() {
        assert!(is_non_building_subcommand(&[
            "+nightly".into(),
            "fmt".into(),
            "--check".into()
        ]));
        for cmd in [
            "add", "remove", "rm", "metadata", "tree", "new", "init", "publish", "search", "login",
            "logout", "owner", "vendor", "yank",
        ] {
            assert!(is_non_building_subcommand(&[cmd.into()]), "{cmd}");
        }
        assert!(!is_non_building_subcommand(&[
            "+nightly".into(),
            "build".into()
        ]));
        let empty: Vec<String> = Vec::new();
        assert!(!is_non_building_subcommand(&empty));
    }

    #[test]
    fn test_which_bin_found_and_missing() {
        assert!(
            which_bin("cargo").is_some(),
            "cargo must be on PATH in tests"
        );
        assert!(which_bin("carpe-definitely-not-a-real-binary-xyz").is_none());
    }

    #[test]
    fn test_color_helpers_and_headers() {
        for rendered in [
            cyan("value"),
            green("value"),
            yellow("value"),
            bold("value"),
            dim("value"),
        ] {
            assert!(rendered.contains("value"));
        }
        assert!(status_header("Carpe").contains("Carpe"));
        assert!(warning_header("Warning").contains("Warning"));
    }

    #[test]
    fn test_canonicalize_best_effort_missing_path() {
        let missing = env::temp_dir().join(format!("carpe-no-such-path-{}", test_rand_nonce()));
        assert_eq!(canonicalize_best_effort(&missing), missing);
    }
}
