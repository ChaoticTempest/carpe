use std::env;
use std::fs;
use std::time::SystemTime;

use dialoguer::{console, theme::ColorfulTheme, MultiSelect};

use crate::lockfile;
use crate::repo::RepoIdentity;
use crate::slots::{collect_slots, SlotCandidate};
use crate::utils::{
    cache_root, cyan, format_size, format_time_ago, green, is_ci_environment, yellow,
};

pub fn run_prune(args: &[String]) {
    let mut interactive = false;
    let mut lru: Option<usize> = None;
    let mut auto_prune = false;
    let mut prune_all = false;
    let mut dry_run = false;

    let mut args_iter = args.iter();
    while let Some(arg) = args_iter.next() {
        match arg.as_str() {
            "-i" | "--interactive" => interactive = true,
            "-a" | "--all" => prune_all = true,
            "--auto" => {
                auto_prune = true;
                prune_all = true;
            }
            "-n" | "--dry-run" => dry_run = true,
            "--lru" => {
                let num_str = args_iter.next().unwrap_or_else(|| {
                    eprintln!("carpe: --lru requires a number argument");
                    std::process::exit(1);
                });
                lru = Some(num_str.parse::<usize>().unwrap_or_else(|_| {
                    eprintln!("carpe: invalid number for --lru");
                    std::process::exit(1);
                }));
            }
            other => {
                eprintln!("carpe: unknown prune option '{other}'");
                std::process::exit(1);
            }
        }
    }

    let root = cache_root();
    if !root.exists() {
        println!("No carpe cache directory found.");
        return;
    }

    let (filter_pool, preferred) = if prune_all {
        (None, None)
    } else {
        let cwd = env::current_dir().expect("carpe: cannot read current directory");
        let identity = RepoIdentity::detect(&cwd);
        (Some(identity.pool_name()), identity.read_preferred_slot())
    };

    let mut slots = collect_slots(&root, filter_pool.as_deref(), preferred);

    if slots.is_empty() {
        println!("No target slots found.");
        return;
    }

    if interactive {
        if is_ci_environment() || !console::Term::stdout().is_term() {
            eprintln!("carpe: interactive prompt disabled in non-interactive / CI environment.");
            eprintln!("       Use 'carpe prune --auto' or 'carpe prune -a' for automated pruning.");
            return;
        }
        run_interactive_prune(&mut slots, dry_run);
    } else if auto_prune {
        let max_gb: f64 = env::var("CARPE_MAX_STORAGE_GB")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(128.0);
        run_auto_prune(&mut slots, max_gb, dry_run);
    } else if let Some(keep_count) = lru {
        run_lru_prune(&mut slots, keep_count, dry_run);
    } else {
        run_default_prune(&slots, dry_run);
    }
}

pub fn run_interactive_prune(slots: &mut [SlotCandidate], dry_run: bool) {
    let mut choices: Vec<String> = Vec::new();
    let mut default_states: Vec<bool> = Vec::new();

    for slot in slots.iter() {
        let status = if slot.is_busy {
            "LOCKED / BUSY"
        } else if slot.is_preferred {
            "PREFERRED"
        } else {
            "UNLOCKED"
        };
        let line = format!(
            "{:<35} {:<10} {:>10}   (last modified: {})",
            slot.name,
            format_size(slot.size_bytes),
            status,
            format_time_ago(slot.mtime)
        );
        choices.push(line);
        default_states.push(!slot.is_busy && !slot.is_preferred);
    }

    println!("Select target slots to prune (Space to select, Enter to confirm):\n");

    let selection = match MultiSelect::with_theme(&ColorfulTheme::default())
        .items(&choices)
        .defaults(&default_states)
        .interact_on_opt(&console::Term::stdout())
    {
        Ok(Some(sel)) => sel,
        Ok(None) => {
            println!("Prune cancelled.");
            return;
        }
        Err(e) => {
            eprintln!("carpe: interactive prompt error ({e})");
            return;
        }
    };

    if selection.is_empty() {
        println!("No slots selected for pruning.");
        return;
    }

    let mut pruned_count = 0;
    let mut total_reclaimed = 0u64;

    for &idx in &selection {
        if let Some(size) = try_prune_slot(&slots[idx], dry_run) {
            pruned_count += 1;
            total_reclaimed += size;
        }
    }

    let prefix = if dry_run { "[dry-run] " } else { "" };
    println!(
        "\n{prefix}Done: pruned {pruned_count} slots, reclaimed {}.",
        format_size(total_reclaimed)
    );
}

pub fn run_auto_prune(slots: &mut [SlotCandidate], max_gb: f64, dry_run: bool) {
    let max_bytes = (max_gb * 1024.0 * 1024.0 * 1024.0) as u64;
    let mut total_size: u64 = slots.iter().map(|s| s.size_bytes).sum();

    if total_size <= max_bytes {
        println!(
            "Auto Prune: Total cache size ({}) is within storage budget ({:.1} GB), nothing to prune.",
            format_size(total_size),
            max_gb
        );
        return;
    }

    let mut free_slots: Vec<&SlotCandidate> = slots.iter().filter(|s| !s.is_busy).collect();
    free_slots.sort_by(|a, b| {
        let t_a = a.mtime.unwrap_or(SystemTime::UNIX_EPOCH);
        let t_b = b.mtime.unwrap_or(SystemTime::UNIX_EPOCH);
        t_a.cmp(&t_b)
    });

    let mut pruned_count = 0;
    let mut total_reclaimed = 0u64;

    for slot in free_slots {
        if total_size <= max_bytes {
            break;
        }

        if let Some(size) = try_prune_slot(slot, dry_run) {
            pruned_count += 1;
            total_reclaimed += size;
            total_size = total_size.saturating_sub(size);
        }
    }

    let prefix = if dry_run { "[dry-run] " } else { "" };
    println!(
        "{prefix}Auto Prune: pruned {pruned_count} older slots, reclaimed {} (new cache size: {}).",
        format_size(total_reclaimed),
        format_size(total_size)
    );
}

pub fn run_lru_prune(slots: &mut [SlotCandidate], keep_num: usize, dry_run: bool) {
    let mut free_slots: Vec<&SlotCandidate> = slots.iter().filter(|s| !s.is_busy).collect();

    free_slots.sort_by(|a, b| {
        let t_a = a.mtime.unwrap_or(SystemTime::UNIX_EPOCH);
        let t_b = b.mtime.unwrap_or(SystemTime::UNIX_EPOCH);
        t_b.cmp(&t_a)
    });

    if free_slots.len() <= keep_num {
        println!(
            "Free slots ({}) <= keep count ({keep_num}), nothing to prune.",
            free_slots.len()
        );
        return;
    }

    let to_prune = &free_slots[keep_num..];
    let mut pruned_count = 0;
    let mut total_reclaimed = 0u64;

    for slot in to_prune {
        if let Some(size) = try_prune_slot(slot, dry_run) {
            pruned_count += 1;
            total_reclaimed += size;
        }
    }

    let prefix = if dry_run { "[dry-run] " } else { "" };
    println!(
        "{prefix}LRU Prune: kept {keep_num} newest free slots, pruned {pruned_count} older slots, reclaimed {}.",
        format_size(total_reclaimed)
    );
}

pub fn run_default_prune(slots: &[SlotCandidate], dry_run: bool) {
    let mut pruned_count = 0;
    let mut total_reclaimed = 0u64;

    for slot in slots.iter() {
        if let Some(size) = try_prune_slot(slot, dry_run) {
            pruned_count += 1;
            total_reclaimed += size;
        }
    }

    let prefix = if dry_run { "[dry-run] " } else { "" };
    println!(
        "{prefix}Prune complete: removed {pruned_count} slots, reclaimed {}.",
        format_size(total_reclaimed)
    );
}

fn try_prune_slot(slot: &SlotCandidate, dry_run: bool) -> Option<u64> {
    if slot.is_busy {
        println!("Skipping {} (currently locked/busy)", cyan(&slot.name));
        return None;
    }

    let lock_path = slot.path.join(".carpe-lock");
    match lockfile::try_lock(&lock_path) {
        Ok(Some(_lock)) => {
            let size = slot.size_bytes;
            if dry_run {
                println!(
                    "{}",
                    yellow(format!(
                        "[dry-run] Would prune {} ({})",
                        cyan(&slot.name),
                        format_size(size)
                    ))
                );
                Some(size)
            } else if fs::remove_dir_all(&slot.path).is_ok() {
                println!("Pruned {} ({})", green(&slot.name), format_size(size));
                Some(size)
            } else {
                eprintln!("Failed to remove {}", slot.path.display());
                None
            }
        }
        _ => {
            println!("Skipping {} (busy)", cyan(&slot.name));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_rand_nonce;

    #[test]
    fn test_lru_pruning_selection() {
        let sys_temp = env::temp_dir();
        let test_dir = sys_temp.join(format!("carpe_lru_test_{}", test_rand_nonce()));
        fs::create_dir_all(&test_dir).unwrap();

        let pool_name = "testrepo-12345678";
        let path0 = test_dir.join(format!("{pool_name}-0"));
        let path1 = test_dir.join(format!("{pool_name}-1"));
        let path2 = test_dir.join(format!("{pool_name}-2"));

        fs::create_dir_all(&path0).unwrap();
        fs::create_dir_all(&path1).unwrap();
        fs::create_dir_all(&path2).unwrap();

        let now = SystemTime::now();
        let oldest_time = now - std::time::Duration::from_secs(3600);
        let middle_time = now - std::time::Duration::from_secs(1800);
        let newest_time = now;

        let mut slots = vec![
            SlotCandidate {
                name: format!("{pool_name}-0"),
                path: path0.clone(),
                size_bytes: 1,
                mtime: Some(oldest_time),
                is_busy: false,
                is_preferred: false,
            },
            SlotCandidate {
                name: format!("{pool_name}-1"),
                path: path1.clone(),
                size_bytes: 3,
                mtime: Some(middle_time),
                is_busy: false,
                is_preferred: false,
            },
            SlotCandidate {
                name: format!("{pool_name}-2"),
                path: path2.clone(),
                size_bytes: 6,
                mtime: Some(newest_time),
                is_busy: false,
                is_preferred: false,
            },
        ];

        run_lru_prune(&mut slots, 1, false);

        assert!(!path0.exists(), "Oldest slot 0 should be pruned");
        assert!(!path1.exists(), "Middle slot 1 should be pruned");
        assert!(path2.exists(), "Newest slot 2 should be kept");

        let _ = fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn test_auto_pruning_selection() {
        let sys_temp = env::temp_dir();
        let test_dir = sys_temp.join(format!("carpe_auto_test_{}", test_rand_nonce()));
        fs::create_dir_all(&test_dir).unwrap();

        let pool_name = "testrepo-auto";
        let path0 = test_dir.join(format!("{pool_name}-0"));
        let path1 = test_dir.join(format!("{pool_name}-1"));

        fs::create_dir_all(&path0).unwrap();
        fs::create_dir_all(&path1).unwrap();

        let now = SystemTime::now();
        let oldest = now - std::time::Duration::from_secs(3600);
        let newest = now;

        let mut slots = vec![
            SlotCandidate {
                name: format!("{pool_name}-0"),
                path: path0.clone(),
                size_bytes: 1000,
                mtime: Some(oldest),
                is_busy: false,
                is_preferred: false,
            },
            SlotCandidate {
                name: format!("{pool_name}-1"),
                path: path1.clone(),
                size_bytes: 1000,
                mtime: Some(newest),
                is_busy: false,
                is_preferred: false,
            },
        ];

        run_auto_prune(&mut slots, 0.000001396, false);

        assert!(!path0.exists(), "Oldest slot 0 should be auto-pruned");
        assert!(path1.exists(), "Newest slot 1 should be retained");

        let _ = fs::remove_dir_all(&test_dir);
    }
}
