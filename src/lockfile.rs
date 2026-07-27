//! A tiny non-blocking, advisory, whole-file exclusive lock.
//!
//! Carpe does *not* try to inspect cargo's own internal `.cargo-lock` file.
//! That file lives inside the profile subdirectory (e.g.
//! `target/debug/.cargo-lock`), which cargo's own docs describe as an
//! "internal implementation detail... we can change this if needed", and it
//! won't exist at all until the first build has happened. Instead, each
//! carpe "slot" directory gets its own lock file (`.carpe-lock`) at its
//! root, which carpe locks itself for the entire lifetime of the `cargo`
//! child process it spawns. This is the same underlying mechanism cargo
//! uses (flock/LockFileEx), just under carpe's own control so we don't have
//! to guess which profile/target-triple subdir cargo is about to use.
//!
//! Because these are OS-level advisory locks tied to an open file
//! descriptor/handle, they are released automatically by the kernel when
//! the process exits or crashes -- there is no stale-lock file to clean up.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

/// An acquired lock. Holds the underlying file open; the lock is released
/// when this value is dropped or the process exits, whichever comes first.
pub struct Lock {
    #[allow(dead_code)]
    file: File,
}

/// Try to take a non-blocking exclusive lock on `path`, creating the file
/// if it doesn't exist. Returns:
/// - `Ok(Some(lock))` if the lock was acquired,
/// - `Ok(None)` if someone else already holds it,
/// - `Err(_)` on a genuine I/O error (e.g. can't create the file).
pub fn try_lock(path: &Path) -> io::Result<Option<Lock>> {
    let file = OpenOptions::new().create(true).write(true).open(path)?;
    if sys::try_lock_exclusive(&file)? {
        Ok(Some(Lock { file }))
    } else {
        Ok(None)
    }
}

#[cfg(unix)]
mod sys {
    use std::fs::File;
    use std::io;
    use std::os::raw::c_int;
    use std::os::unix::io::AsRawFd;

    extern "C" {
        fn flock(fd: c_int, operation: c_int) -> c_int;
    }

    const LOCK_EX: c_int = 2;
    const LOCK_NB: c_int = 4;

    /// Returns Ok(true) if acquired, Ok(false) if contended, Err on real I/O error.
    pub fn try_lock_exclusive(file: &File) -> io::Result<bool> {
        let ret = unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) };
        if ret == 0 {
            Ok(true)
        } else {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::WouldBlock {
                Ok(false)
            } else {
                Err(err)
            }
        }
    }
}

#[cfg(windows)]
mod sys {
    use std::fs::File;
    use std::io;
    use std::os::windows::io::AsRawHandle;

    #[allow(non_camel_case_types)]
    type HANDLE = *mut std::ffi::c_void;
    #[allow(non_camel_case_types)]
    type BOOL = i32;
    #[allow(non_camel_case_types)]
    type DWORD = u32;

    #[repr(C)]
    struct Overlapped {
        internal: usize,
        internal_high: usize,
        offset: DWORD,
        offset_high: DWORD,
        h_event: HANDLE,
    }

    extern "system" {
        fn LockFileEx(
            hfile: HANDLE,
            flags: DWORD,
            reserved: DWORD,
            bytes_low: DWORD,
            bytes_high: DWORD,
            overlapped: *mut Overlapped,
        ) -> BOOL;
    }

    const LOCKFILE_FAIL_IMMEDIATELY: DWORD = 0x0000_0001;
    const LOCKFILE_EXCLUSIVE_LOCK: DWORD = 0x0000_0002;

    /// Best-effort Windows support via LockFileEx. Less battle-tested than
    /// the Unix flock path; contended and success are distinguished, but a
    /// stray API error is conservatively treated as "contended" rather than
    /// surfaced, so carpe just tries the next slot.
    pub fn try_lock_exclusive(file: &File) -> io::Result<bool> {
        let mut overlapped: Overlapped = unsafe { std::mem::zeroed() };
        let ok = unsafe {
            LockFileEx(
                file.as_raw_handle() as HANDLE,
                LOCKFILE_FAIL_IMMEDIATELY | LOCKFILE_EXCLUSIVE_LOCK,
                0,
                u32::MAX,
                u32::MAX,
                &mut overlapped,
            )
        };
        Ok(ok != 0)
    }
}