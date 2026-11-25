//! End-to-end CLI tests. Every test spawns the real `carpe` binary with an
//! isolated environment, so no test ever touches the user's real cargo shim
//! or cache directory.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static NONCE: AtomicU64 = AtomicU64::new(0);

fn unique_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let n = NONCE.fetch_add(1, Ordering::Relaxed);
    let dir = env::temp_dir().join(format!(
        "carpe_cli_{label}_{}_{n}_{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("create test temp dir");
    dir
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        Self(unique_dir(label))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn carpe() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_carpe"));
    cmd.env_remove("CARPE_CARGO_PATH")
        .env_remove("CARPE_MAX_STORAGE_GB")
        .env_remove("CARPE_CI")
        .env_remove("CONTINUOUS_INTEGRATION")
        // A CI/coverage runner may export these; carpe treats them as user input.
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("CARGO_BUILD_TARGET_DIR");
    cmd
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn assert_code(output: &Output, code: i32) {
    assert_eq!(
        output.status.code(),
        Some(code),
        "status={:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        stdout(output),
        stderr(output)
    );
}

#[test]
fn no_args_prints_usage_and_exits_two() {
    let output = carpe().output().unwrap();
    assert_code(&output, 2);
    assert!(stderr(&output).contains("usage:"));
    assert!(stderr(&output).contains("carpe status"));
}

#[test]
fn help_flags_exit_zero() {
    for flag in ["-h", "--help"] {
        let output = carpe().arg(flag).output().unwrap();
        assert_code(&output, 0);
        assert!(stderr(&output).contains("usage:"));
    }
}

#[test]
fn version_flags_exit_zero() {
    for flag in ["-V", "--version"] {
        let output = carpe().arg(flag).output().unwrap();
        assert_code(&output, 0);
        assert!(stdout(&output).contains("carpe 0.1.0"));
    }
}

#[test]
fn version_with_leading_binary_name() {
    let output = carpe().args(["carpe", "--version"]).output().unwrap();
    assert_code(&output, 0);
    assert!(stdout(&output).contains("carpe 0.1.0"));
}

#[test]
fn toggle_override_backs_up_restores_and_detects_shim() {
    let temp = TempDir::new("override");
    let cargo_home = temp.path().join("cargo_home");
    let bin = cargo_home.join("bin");
    fs::create_dir_all(&bin).unwrap();

    let shim = bin.join(if cfg!(windows) { "cargo.exe" } else { "cargo" });
    let original = "REAL CARGO CONTENT\n";
    fs::write(&shim, original).unwrap();

    let enable = carpe()
        .env("CARGO_HOME", &cargo_home)
        .arg("toggle-override")
        .output()
        .unwrap();
    assert_code(&enable, 0);
    assert!(stdout(&enable).contains("enabling cargo=carpe"));

    let backup = bin.join(if cfg!(windows) {
        "cargo.real.exe"
    } else {
        "cargo.real"
    });
    assert!(backup.exists(), "real cargo must be backed up");
    assert_eq!(fs::read_to_string(&backup).unwrap(), original);
    let shim_content = fs::read_to_string(&shim).unwrap();
    if cfg!(unix) {
        assert!(shim_content.contains("# carpe-shim"));
    } else {
        assert!(shim_content.contains("carpe-shim"));
    }

    let disable = carpe()
        .env("CARGO_HOME", &cargo_home)
        .arg("toggle-override")
        .output()
        .unwrap();
    assert_code(&disable, 0);
    assert!(stdout(&disable).contains("disabling cargo=carpe"));
    assert!(!backup.exists(), "backup must be restored");
    assert_eq!(fs::read_to_string(&shim).unwrap(), original);
}

#[test]
fn prune_rejects_bad_arguments() {
    let cases: &[(&[&str], &str)] = &[
        (&["prune", "--unknown"], "unknown prune option"),
        (&["prune", "--lru"], "--lru requires a number"),
        (
            &["prune", "--lru", "notanumber"],
            "invalid number for --lru",
        ),
    ];
    for (args, message) in cases {
        let output = carpe().args(*args).output().unwrap();
        assert_code(&output, 1);
        assert!(
            stderr(&output).contains(message),
            "stderr: {}",
            stderr(&output)
        );
    }
}

#[cfg(unix)]
mod unix {
    use super::*;

    use std::os::unix::fs::PermissionsExt;

    struct Isolated {
        dir: PathBuf,
    }

    impl Isolated {
        fn new(label: &str) -> Self {
            Self {
                dir: unique_dir(label),
            }
        }

        fn path(&self) -> &Path {
            &self.dir
        }

        fn apply(&self, cmd: &mut Command) {
            cmd.env("HOME", &self.dir)
                .env("XDG_CACHE_HOME", self.dir.join("cache"))
                .current_dir(&self.dir)
                // Never inherit CI mode: some tests explicitly opt in.
                .env_remove("CI")
                .env_remove("CARPE_CI")
                .env_remove("CONTINUOUS_INTEGRATION");
        }

        fn field(&self, key: &str) -> String {
            let mut cmd = carpe();
            self.apply(&mut cmd);
            let output = cmd.arg("info").output().unwrap();
            assert_code(&output, 0);
            stdout(&output)
                .lines()
                .find_map(|line| line.strip_prefix(key).map(str::trim))
                .unwrap_or_else(|| panic!("missing {key} in {}", stdout(&output)))
                .to_string()
        }

        fn cache_root(&self) -> PathBuf {
            PathBuf::from(self.field("Cache Root:"))
        }

        fn pool(&self) -> String {
            self.field("Pool Name:")
        }
    }

    impl Drop for Isolated {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    fn write_fake_cargo(path: &Path, script: &str) {
        fs::write(path, script).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn make_script(path: &Path, body: &str) -> PathBuf {
        write_fake_cargo(path, &format!("#!/bin/sh\n{body}\n"));
        path.to_path_buf()
    }

    #[test]
    fn status_without_cache_directory() {
        let iso = Isolated::new("status_missing");
        let output = carpe().arg("status").output_unwrapped(&iso);
        assert_code(&output, 0);
        assert!(stdout(&output).contains("does not exist yet"));
        assert!(stdout(&output).contains("Preferred:"));
    }

    // Small helper trait so every test applies the same isolation.
    trait ApplyIsolation {
        fn output_unwrapped(&mut self, iso: &Isolated) -> Output;
    }

    impl ApplyIsolation for Command {
        fn output_unwrapped(&mut self, iso: &Isolated) -> Output {
            iso.apply(self);
            self.output().unwrap()
        }
    }

    #[test]
    fn status_with_empty_cache_directory() {
        let iso = Isolated::new("status_empty");
        fs::create_dir_all(iso.cache_root()).unwrap();

        let output = carpe().arg("status").output_unwrapped(&iso);
        assert_code(&output, 0);
        assert!(stdout(&output).contains("none exist for this pool yet"));
    }

    #[test]
    fn status_with_populated_pool_and_budget_warning() {
        let iso = Isolated::new("status_slots");
        let root = iso.cache_root();
        let slot = root.join(format!("{}-0", iso.pool()));
        fs::create_dir_all(&slot).unwrap();
        fs::write(slot.join(".carpe-meta"), "timestamp=1000000000\n").unwrap();
        fs::write(slot.join("data.bin"), vec![7u8; 512]).unwrap();

        let output = carpe().arg("status").output_unwrapped(&iso);
        assert_code(&output, 0);
        assert!(stdout(&output).contains("free"));
        assert!(stdout(&output).contains("Total pool size:"));
        assert!(stdout(&output).contains("modified:"));

        let warned = carpe()
            .env("CARPE_MAX_STORAGE_GB", "0.0000001")
            .arg("status")
            .output_unwrapped(&iso);
        assert_code(&warned, 0);
        assert!(
            stderr(&warned).contains("exceeds"),
            "stderr: {}",
            stderr(&warned)
        );
    }

    #[test]
    fn status_and_info_report_locked_slots() {
        use fd_lock::RwLock;

        let iso = Isolated::new("busy_slot");
        let root = iso.cache_root();
        let slot = root.join(format!("{}-0", iso.pool()));
        fs::create_dir_all(&slot).unwrap();
        fs::write(slot.join("data.bin"), vec![3u8; 64]).unwrap();

        let lock_path = slot.join(".carpe-lock");
        let file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .unwrap();
        let mut lock = RwLock::new(file);
        let guard = lock.try_write().unwrap();

        let status = carpe().arg("status").output_unwrapped(&iso);
        assert_code(&status, 0);
        assert!(stdout(&status).contains("LOCKED/BUSY"));

        let info = carpe().arg("info").output_unwrapped(&iso);
        assert_code(&info, 0);
        assert!(stdout(&info).contains("1 busy"));
        assert!(stdout(&info).contains("0 free"));

        drop(guard);
    }

    #[test]
    fn build_warns_on_cargo_build_target_dir_env() {
        let iso = Isolated::new("build_target_env");
        let script = make_script(&iso.path().join("fake-cargo"), "echo FAKE_CARGO_RAN");
        let output = carpe()
            .env("CARPE_CARGO_PATH", &script)
            .env("CARGO_BUILD_TARGET_DIR", "/ignored/build-target")
            .arg("build")
            .output_unwrapped(&iso);
        assert_code(&output, 0);
        assert!(stderr(&output).contains("target directory is set in environment"));
        assert!(stderr(&output).contains("/ignored/build-target"));
    }

    #[test]
    fn info_in_git_repo_and_non_git_directory() {
        let iso = Isolated::new("info");

        let outside = carpe().arg("info").output_unwrapped(&iso);
        assert_code(&outside, 0);
        assert!(stdout(&outside).contains("Git Common Dir: (none"));
        assert!(stdout(&outside).contains("Cache directory does not exist yet."));

        let repo = iso.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        let init = Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repo)
            .status()
            .unwrap();
        if !init.success() {
            eprintln!("skipping git-dependent assertions: git init failed");
            return;
        }

        let mut cmd = carpe();
        iso.apply(&mut cmd);
        cmd.current_dir(&repo);
        let inside = cmd.arg("info").output().unwrap();
        assert_code(&inside, 0);
        assert!(stdout(&inside).contains("Git Common Dir:"));
        assert!(!stdout(&inside).contains("Git Common Dir: (none"));
        assert!(stdout(&inside).contains("Git Worktree:"));

        let root = iso.cache_root();
        let pool = iso.field_with_cwd("Pool Name:", &repo);
        let slot = root.join(format!("{pool}-0"));
        fs::create_dir_all(&slot).unwrap();
        fs::write(slot.join("data.bin"), vec![1u8; 300]).unwrap();

        let mut cmd = carpe();
        iso.apply(&mut cmd);
        cmd.current_dir(&repo);
        let with_slots = cmd.arg("info").output().unwrap();
        assert_code(&with_slots, 0);
        assert!(stdout(&with_slots).contains("Slot Pool Stats: 1 total slots"));
        assert!(stdout(&with_slots).contains("1 free"));
        assert!(stdout(&with_slots).contains("0 busy"));

        let mut cmd = carpe();
        iso.apply(&mut cmd);
        cmd.current_dir(&repo)
            .env("CARPE_MAX_STORAGE_GB", "0.0000001");
        let warned = cmd.arg("info").output().unwrap();
        assert_code(&warned, 0);
        assert!(stderr(&warned).contains("exceeds"));
    }

    impl Isolated {
        fn field_with_cwd(&self, key: &str, cwd: &Path) -> String {
            let mut cmd = carpe();
            self.apply(&mut cmd);
            cmd.current_dir(cwd);
            let output = cmd.arg("info").output().unwrap();
            assert_code(&output, 0);
            stdout(&output)
                .lines()
                .find_map(|line| line.strip_prefix(key).map(str::trim))
                .unwrap_or_else(|| panic!("missing {key} in {}", stdout(&output)))
                .to_string()
        }
    }

    #[test]
    fn build_selects_slot_zero_and_records_meta() {
        let iso = Isolated::new("build");
        let root = iso.cache_root();
        let pool = iso.pool();
        let marker = iso.path().join("cargo-target-dir");
        let script = make_script(
            &iso.path().join("fake-cargo"),
            &format!(
                "printf '%s' \"$CARGO_TARGET_DIR\" > \"{}\"\necho FAKE_CARGO_RAN",
                marker.display()
            ),
        );

        let output = carpe()
            .env("CARPE_CARGO_PATH", &script)
            .args(["build", "--release"])
            .output_unwrapped(&iso);
        assert_code(&output, 0);
        assert!(stdout(&output).contains("FAKE_CARGO_RAN"));
        assert!(stderr(&output).contains("using "));

        let target = fs::read_to_string(&marker).unwrap();
        assert_eq!(target, format!("{}/{pool}-0", root.display()));
        assert!(root.join(format!("{pool}-0")).join(".carpe-meta").exists());
        assert!(root.join(format!("{pool}-0")).join(".carpe-lock").exists());
    }

    #[test]
    fn build_warns_on_env_target_dir_and_explicit_flag() {
        let iso = Isolated::new("build_warnings");
        let script = make_script(&iso.path().join("fake-cargo"), "echo FAKE_CARGO_RAN");
        let output = carpe()
            .env("CARPE_CARGO_PATH", &script)
            .env("CARGO_TARGET_DIR", "/ignored/env-target")
            .args(["build", "--target-dir", "/ignored/flag-target"])
            .output_unwrapped(&iso);
        assert_code(&output, 0);
        assert!(stdout(&output).contains("FAKE_CARGO_RAN"));
        assert!(stderr(&output).contains("target directory is set in environment"));
        assert!(stderr(&output).contains("explicit --target-dir flag passed"));
    }

    #[test]
    fn build_falls_back_when_preferred_slot_is_busy() {
        use fd_lock::RwLock;

        let iso = Isolated::new("build_busy");
        let root = iso.cache_root();
        let pool = iso.pool();
        let script = make_script(&iso.path().join("fake-cargo"), "echo FAKE_CARGO_RAN");

        let first = carpe()
            .env("CARPE_CARGO_PATH", &script)
            .arg("build")
            .output_unwrapped(&iso);
        assert_code(&first, 0);
        assert!(root.join(format!("{pool}-0")).exists());

        // Hold slot 0's lock: the second build must choose slot 1.
        let lock_path = root.join(format!("{pool}-0")).join(".carpe-lock");
        let file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .unwrap();
        let mut lock = RwLock::new(file);
        let guard = lock.try_write().unwrap();

        let second = carpe()
            .env("CARPE_CARGO_PATH", &script)
            .arg("build")
            .output_unwrapped(&iso);
        assert_code(&second, 0);
        assert!(
            stderr(&second).contains("is busy, using"),
            "stderr: {}",
            stderr(&second)
        );
        assert!(root.join(format!("{pool}-1")).exists());

        drop(guard);
    }

    #[test]
    fn fmt_uses_fake_cargo_without_creating_a_pool() {
        let iso = Isolated::new("fmt");
        let script = make_script(
            &iso.path().join("fake-cargo"),
            "echo FAKE_FMT_ARGS:$*; echo CARPE_TARGET:${CARGO_TARGET_DIR:-unset}",
        );
        let output = carpe()
            .env("CARPE_CARGO_PATH", &script)
            .args(["fmt", "--check"])
            .output_unwrapped(&iso);
        assert_code(&output, 0);
        assert!(stdout(&output).contains("FAKE_FMT_ARGS:fmt --check"));
        assert!(stdout(&output).contains("CARPE_TARGET:unset"));
        assert!(!iso.cache_root().exists(), "fmt must not create a cache");
    }

    #[test]
    fn prune_reports_missing_and_empty_cache() {
        let iso = Isolated::new("prune_missing");
        let output = carpe().arg("prune").output_unwrapped(&iso);
        assert_code(&output, 0);
        assert!(stdout(&output).contains("No carpe cache directory found."));

        fs::create_dir_all(iso.cache_root()).unwrap();
        let empty = carpe().arg("prune").output_unwrapped(&iso);
        assert_code(&empty, 0);
        assert!(stdout(&empty).contains("No target slots found."));
    }

    fn populated_pool(iso: &Isolated) -> PathBuf {
        let root = iso.cache_root();
        let pool = iso.pool();
        for idx in 0..3 {
            let slot = root.join(format!("{pool}-{idx}"));
            fs::create_dir_all(&slot).unwrap();
            fs::write(
                slot.join(".carpe-meta"),
                format!("timestamp={}\n", 1_000_000_000 + idx),
            )
            .unwrap();
            fs::write(slot.join("data.bin"), vec![idx as u8 + 1; 100 + idx]).unwrap();
        }
        root
    }

    #[test]
    fn prune_dry_run_then_remove() {
        let iso = Isolated::new("prune_default");
        let root = populated_pool(&iso);
        let pool = iso.pool();

        let dry = carpe().args(["prune", "-n"]).output_unwrapped(&iso);
        assert_code(&dry, 0);
        assert!(stdout(&dry).contains("[dry-run] Would prune"));
        assert!(stdout(&dry).contains("[dry-run] Prune complete"));
        assert!(root.join(format!("{pool}-0")).exists());

        let real = carpe().arg("prune").output_unwrapped(&iso);
        assert_code(&real, 0);
        assert!(stdout(&real).contains("Pruned "));
        assert!(stdout(&real).contains("Prune complete"));
        assert!(!root.join(format!("{pool}-0")).exists());
        assert!(!root.join(format!("{pool}-1")).exists());
        assert!(!root.join(format!("{pool}-2")).exists());
    }

    #[test]
    fn prune_lru_keeps_newest_slots() {
        let iso = Isolated::new("prune_lru");
        let root = populated_pool(&iso);
        let pool = iso.pool();

        let keep_all = carpe()
            .args(["prune", "--lru", "10"])
            .output_unwrapped(&iso);
        assert_code(&keep_all, 0);
        assert!(stdout(&keep_all).contains("nothing to prune"));
        assert!(root.join(format!("{pool}-0")).exists());

        let output = carpe().args(["prune", "--lru", "1"]).output_unwrapped(&iso);
        assert_code(&output, 0);
        assert!(stdout(&output).contains("LRU Prune:"));
        assert!(!root.join(format!("{pool}-0")).exists());
        assert!(!root.join(format!("{pool}-1")).exists());
        assert!(root.join(format!("{pool}-2")).exists());
    }

    #[test]
    fn prune_auto_respects_budget_and_dry_run() {
        let iso = Isolated::new("prune_auto");
        let root = populated_pool(&iso);

        let within = carpe().args(["prune", "--auto"]).output_unwrapped(&iso);
        assert_code(&within, 0);
        assert!(stdout(&within).contains("within storage budget"));

        let dry = carpe()
            .env("CARPE_MAX_STORAGE_GB", "0.0000001")
            .args(["prune", "--auto", "-n"])
            .output_unwrapped(&iso);
        assert_code(&dry, 0);
        assert!(stdout(&dry).contains("[dry-run] Auto Prune:"));

        let real = carpe()
            .env("CARPE_MAX_STORAGE_GB", "0.0000001")
            .args(["prune", "--auto"])
            .output_unwrapped(&iso);
        assert_code(&real, 0);
        assert!(stdout(&real).contains("Auto Prune: pruned"));
        assert_eq!(
            fs::read_dir(&root).unwrap().filter(|e| e.is_ok()).count(),
            0,
            "all slots should be pruned under a tiny budget"
        );
    }

    #[test]
    fn prune_all_ignores_pool_filter() {
        let iso = Isolated::new("prune_all");
        let root = populated_pool(&iso);
        let other = root.join("another-pool-0");
        fs::create_dir_all(&other).unwrap();
        fs::write(other.join("data.bin"), vec![9u8; 50]).unwrap();

        let output = carpe().args(["prune", "-a"]).output_unwrapped(&iso);
        assert_code(&output, 0);
        assert!(stdout(&output).contains("Prune complete"));
        assert!(!other.exists());
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
    }

    #[test]
    fn prune_interactive_is_disabled_in_ci() {
        let iso = Isolated::new("prune_ci");
        populated_pool(&iso);

        let output = carpe()
            .env("CI", "true")
            .args(["prune", "-i"])
            .output_unwrapped(&iso);
        assert_code(&output, 0);
        assert!(
            stderr(&output).contains("interactive prompt disabled"),
            "stderr: {}",
            stderr(&output)
        );
        assert!(stderr(&output).contains("carpe prune --auto"));
    }
}
