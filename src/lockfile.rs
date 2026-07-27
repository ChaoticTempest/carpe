//! A tiny non-blocking, advisory, whole-file exclusive lock wrapper around `fd-lock`.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

/// An acquired exclusive lock on a file.
pub struct Lock {
    // We store the RwLock and its guard in a box/owning structure so the lock
    // remains acquired until `Lock` is dropped.
    _guard: fd_lock::RwLockWriteGuard<'static, File>,
    _lock: Box<fd_lock::RwLock<File>>,
}

/// Try to take a non-blocking exclusive lock on `path`, creating the file
/// if it doesn't exist. Returns:
/// - `Ok(Some(lock))` if the lock was acquired,
/// - `Ok(None)` if someone else already holds it,
/// - `Err(_)` on a genuine I/O error (e.g. can't create the file).
pub fn try_lock(path: &Path) -> io::Result<Option<Lock>> {
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)?;

    let mut lock = Box::new(fd_lock::RwLock::new(file));

    // Erase the borrow lifetime of `lock` so `Lock` can own both the `RwLock` and its `RwLockWriteGuard`.
    // SAFETY: `lock` is heap-allocated on the heap inside `Box`, so its address does not move when `Lock` is moved.
    let lock_ptr: *mut fd_lock::RwLock<File> = &mut *lock;
    let guard = match unsafe { (*lock_ptr).try_write() } {
        Ok(guard) => guard,
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(None),
        Err(e) => return Err(e),
    };

    // Re-bind lifetime to 'static safely because `_lock` keeps the underlying `RwLock` alive at a fixed memory address.
    let static_guard = unsafe {
        std::mem::transmute::<
            fd_lock::RwLockWriteGuard<'_, File>,
            fd_lock::RwLockWriteGuard<'static, File>,
        >(guard)
    };

    Ok(Some(Lock {
        _guard: static_guard,
        _lock: lock,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lockfile_acquisition_and_contention() {
        let temp_dir = std::env::temp_dir();
        let lock_path = temp_dir.join(format!("carpe_test_lock_{}.lock", std::process::id()));

        // Clean up any stale file from prior runs
        let _ = std::fs::remove_file(&lock_path);

        // 1. First lock attempt succeeds
        let lock1 = try_lock(&lock_path).expect("lock1 should succeed");
        assert!(lock1.is_some(), "first lock should be acquired");

        // 2. Second lock attempt on same file fails (contended)
        let lock2 = try_lock(&lock_path).expect("lock2 check should not error");
        assert!(lock2.is_none(), "second lock should be contended");

        // 3. Dropping the first lock frees the file lock
        drop(lock1);

        // 4. Subsequent lock attempt succeeds
        let lock3 = try_lock(&lock_path).expect("lock3 should succeed after drop");
        assert!(
            lock3.is_some(),
            "lock should be acquired after first lock is dropped"
        );

        // Cleanup
        drop(lock3);
        let _ = std::fs::remove_file(&lock_path);
    }
}
