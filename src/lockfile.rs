//! A tiny non-blocking, advisory, whole-file exclusive lock wrapper around `fd-lock`.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

#[cfg(unix)]
use std::os::fd::{AsRawFd, RawFd};

/// An acquired exclusive lock on a file.
pub struct Lock {
    // We store the RwLock and its guard in a box/owning structure so the lock
    // remains acquired until `Lock` is dropped.
    _guard: fd_lock::RwLockWriteGuard<'static, File>,
    _lock: Box<fd_lock::RwLock<File>>,
    // Captured before the `File` is moved into the `RwLock`; needed to keep the
    // lock alive across `exec()` (see `clear_cloexec`).
    #[cfg(unix)]
    raw_fd: RawFd,
}

impl Lock {
    /// The raw FD backing this lock, for use with [`clear_cloexec`] before `exec()`.
    #[cfg(unix)]
    pub fn raw_fd(&self) -> RawFd {
        self.raw_fd
    }
}

/// Clear `FD_CLOEXEC` on `raw_fd` so an `exec()`ed process inherits it.
///
/// Rust opens files with `O_CLOEXEC` on Unix. carpe holds its slot lock via
/// `flock(2)` on that FD and replaces itself with cargo using `Command::exec()`;
/// if CLOEXEC is left set, the FD closes during exec and the flock is released,
/// letting another carpe process select/prune the same slot while cargo builds.
/// Called from a `pre_exec` hook so the FD survives into cargo and its
/// descendants; a long-lived descendant can therefore keep the slot busy
/// after cargo exits.
///
/// Only the CLOEXEC bit is cleared; all other FD flags are preserved.
#[cfg(unix)]
pub fn clear_cloexec(raw_fd: RawFd) -> io::Result<()> {
    use libc::{fcntl, FD_CLOEXEC, F_GETFD, F_SETFD};

    // SAFETY: `fcntl` with F_GETFD/F_SETFD only touches the given FD and has no
    // pointer arguments.
    let current = unsafe { fcntl(raw_fd, F_GETFD) };
    if current == -1 {
        return Err(io::Error::last_os_error());
    }
    let result = unsafe { fcntl(raw_fd, F_SETFD, current & !FD_CLOEXEC) };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
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

    // Capture the FD std just opened with `O_CLOEXEC` before moving the `File`
    // into the lock, so it can be inheritable across `exec()` if needed.
    #[cfg(unix)]
    let raw_fd = file.as_raw_fd();

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
        #[cfg(unix)]
        raw_fd,
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

    #[cfg(unix)]
    #[test]
    fn test_clear_cloexec() {
        use std::os::fd::AsRawFd;

        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join(format!("carpe_test_cloexec_{}.lock", std::process::id()));
        let _ = std::fs::remove_file(&path);

        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .expect("open should succeed");
        let fd = file.as_raw_fd();

        // std opens files with O_CLOEXEC on Unix.
        let before = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        assert!(before >= 0, "fcntl(F_GETFD) should succeed");
        assert_ne!(
            before & libc::FD_CLOEXEC,
            0,
            "FD_CLOEXEC should be set initially"
        );

        clear_cloexec(fd).expect("clear_cloexec should succeed");

        let after = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        assert!(after >= 0, "fcntl(F_GETFD) should succeed after clear");
        assert_eq!(
            after & libc::FD_CLOEXEC,
            0,
            "FD_CLOEXEC should be clear after clear_cloexec"
        );
        assert_eq!(
            after,
            before & !libc::FD_CLOEXEC,
            "unrelated FD flags should be preserved"
        );

        drop(file);
        let _ = std::fs::remove_file(&path);
    }
}
