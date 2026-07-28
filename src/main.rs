//! carpe: multiplex cargo target dirs across git worktrees.
//!
//! `carpe <cargo-args...>` picks a `CARGO_TARGET_DIR` from a small pool of
//! directories under `~/.cache/carpe/`, shared by every worktree of the
//! same repo, then execs `cargo` with that env var set.

mod lockfile;
mod meta;
mod prune;
mod repo;
mod slots;
mod utils;

use std::env;
use std::fs;
use std::process::Command;

use prune::run_prune;
use repo::RepoIdentity;
use slots::{collect_slots, select_slot};
#[cfg(not(unix))]
use utils::exit_with_status;
use utils::{
    cache_root, check_storage_budget, extract_manifest_path, format_size, format_time_ago,
    has_explicit_target_dir_flag, is_non_building_subcommand,
};

fn main() {
    let mut raw_args: Vec<String> = env::args().skip(1).collect();
    if raw_args.first().map(String::as_str) == Some("carpe") {
        raw_args.remove(0);
    }
    let args = raw_args;

    match args.first().map(String::as_str) {
        None | Some("-h") | Some("--help") => {
            print_usage();
            std::process::exit(if args.is_empty() { 2 } else { 0 });
        }
        Some("-V") | Some("--version") => {
            println!("carpe 0.1.0");
        }
        Some("status") => {
            run_status();
        }
        Some("info") => {
            run_info();
        }
        Some("prune") => {
            run_prune(&args[1..]);
        }
        _ => {
            run_cargo(&args);
        }
    }
}

fn print_usage() {
    eprintln!("carpe: multiplex cargo target dirs across git worktrees");
    eprintln!();
    eprintln!("usage:");
    eprintln!("  carpe <cargo-args...>      run cargo with a coordinated CARGO_TARGET_DIR");
    eprintln!("  carpe status               show target-dir slots for the current repo");
    eprintln!(
        "  carpe info                 display detailed workspace, pool, and cache diagnostics"
    );
    eprintln!("  carpe prune [options]      remove target-dir slots not held by any cargo process");
    eprintln!();
    eprintln!("prune options:");
    eprintln!("  -i, --interactive          choose interactively which slots to prune");
    eprintln!("  -n, --dry-run              preview slots to prune without deleting files");
    eprintln!(
        "  --auto                     prune oldest slots until total size drops under budget"
    );
    eprintln!(
        "  --lru <num>                keep the <num> newest unlocked slots and prune older ones"
    );
    eprintln!(
        "  -a, --all                  prune across all repository pools under ~/.cache/carpe"
    );
}

fn run_cargo(args: &[String]) {
    if is_non_building_subcommand(args) {
        let mut cmd = Command::new("cargo");
        cmd.args(args);
        spawn_or_exec_cargo(cmd);
    }

    let cwd = env::current_dir().expect("carpe: cannot read current directory");
    let base_dir = extract_manifest_path(args).unwrap_or_else(|| cwd.clone());
    let identity = RepoIdentity::detect(&base_dir);
    let root = cache_root();
    fs::create_dir_all(&root).expect("carpe: cannot create ~/.cache/carpe");

    let dir_name = identity.pool_name();
    let preferred = identity.read_preferred_slot();

    let (slot_idx, slot_path, _lock) = select_slot(&identity, &root, &base_dir, args);

    let env_target_dir = env::var_os("CARGO_TARGET_DIR")
        .or_else(|| env::var_os("CARGO_BUILD_TARGET_DIR"))
        .filter(|s| !s.is_empty());

    if let Some(existing_env) = env_target_dir {
        eprintln!(
            "carpe: warning: target directory is set in environment ({}), overriding with carpe slot {}",
            existing_env.to_string_lossy(),
            slot_path.display()
        );
    }

    if has_explicit_target_dir_flag(args) {
        eprintln!(
            "carpe: warning: explicit --target-dir flag passed in arguments; Cargo CLI flag will take precedence over carpe slot {}",
            slot_path.display()
        );
    }

    match preferred {
        Some(p) if p != slot_idx => eprintln!(
            "carpe: preferred slot {dir_name}-{p} is busy, using {dir_name}-{slot_idx} instead (parallel build)"
        ),
        _ => eprintln!("carpe: using {}", slot_path.display()),
    }

    let mut cmd = Command::new("cargo");
    cmd.args(args).env("CARGO_TARGET_DIR", &slot_path);
    spawn_or_exec_cargo(cmd);
}

fn spawn_or_exec_cargo(mut cmd: Command) -> ! {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = cmd.exec();
        eprintln!("carpe: failed to exec cargo: {err}");
        std::process::exit(1);
    }

    #[cfg(not(unix))]
    {
        let status = cmd
            .status()
            .expect("carpe: failed to spawn cargo (is it in PATH?)");
        exit_with_status(status);
    }
}

fn run_status() {
    let cwd = env::current_dir().expect("carpe: cannot read current directory");
    let identity = RepoIdentity::detect(&cwd);
    let pool_name = identity.pool_name();

    println!("Pool:      {pool_name}");
    println!("Marker:    {}", identity.marker_path().display());
    println!(
        "Preferred: {}",
        identity
            .read_preferred_slot()
            .map(|i| i.to_string())
            .unwrap_or_else(|| "none".to_string())
    );

    let root = cache_root();
    if !root.exists() {
        println!("Slots:     (cache root ~/.cache/carpe does not exist yet)");
        return;
    }

    let pref = identity.read_preferred_slot();
    let slots = collect_slots(&root, Some(&pool_name), pref);

    if slots.is_empty() {
        println!("Slots:     (none exist for this pool yet)");
        return;
    }

    println!("\nSlots:");
    let mut total_size = 0u64;
    for s in &slots {
        total_size += s.size_bytes;
        let mut status_flags = Vec::new();
        if s.is_busy {
            status_flags.push("LOCKED/BUSY");
        } else {
            status_flags.push("free");
        }
        if s.is_preferred {
            status_flags.push("preferred");
        }

        println!(
            "  {:<25} {:<10} [{}] (modified: {})",
            s.name,
            format_size(s.size_bytes),
            status_flags.join(", "),
            format_time_ago(s.mtime)
        );
    }
    println!("\nTotal pool size: {}", format_size(total_size));
    check_storage_budget(&root);
}

fn run_info() {
    let cwd = env::current_dir().expect("carpe: cannot read current directory");
    let identity = RepoIdentity::detect(&cwd);
    let pool_name = identity.pool_name();
    let root = cache_root();
    let git_common = repo::git_common_dir(&cwd);
    let git_dir = repo::git_dir(&cwd);

    println!("Carpe System & Workspace Info:");
    println!("------------------------------");
    println!("CWD:            {}", cwd.display());
    println!("Workspace Root: {}", identity.name_source.display());
    println!("Pool Name:      {}", pool_name);
    println!("Cache Root:     {}", root.display());
    println!("Marker Path:    {}", identity.marker_path().display());
    println!(
        "Preferred Slot: {}",
        identity
            .read_preferred_slot()
            .map(|i| i.to_string())
            .unwrap_or_else(|| "none".to_string())
    );

    if let Some(common) = git_common {
        println!("Git Common Dir: {}", common.display());
    } else {
        println!("Git Common Dir: (none - non-git cargo workspace)");
    }
    if let Some(gdir) = git_dir {
        println!("Git Worktree:   {}", gdir.display());
    }

    if root.exists() {
        let pref = identity.read_preferred_slot();
        let slots = collect_slots(&root, Some(&pool_name), pref);
        let free_count = slots.iter().filter(|s| !s.is_busy).count();
        let busy_count = slots.iter().filter(|s| s.is_busy).count();
        let total_size: u64 = slots.iter().map(|s| s.size_bytes).sum();
        println!(
            "\nSlot Pool Stats: {} total slots ({} free, {} busy), total size {}",
            slots.len(),
            free_count,
            busy_count,
            format_size(total_size)
        );
        check_storage_budget(&root);
    } else {
        println!("\nSlot Pool Stats: Cache directory does not exist yet.");
    }
}
