//! Process-group control and cross-process file locks.
//!
//! A child spawned with `process_group(0)` leads its own group, and the
//! group - not the leader alone - is what a caller must be able to
//! clean up: cargo's rustc grandchildren, a shell's background jobs.
//! std can signal one process; the group needs `killpg`, which lives
//! here with the rest of the crate's `unsafe`, behind a signature that
//! cannot name a wrong process by accident.
//!
//! The same reasoning holds for advisory locks: `flock` has no safe
//! wrapper in std, and the crates that forbid `unsafe` still have
//! files that more than one process writes.

/// Kill the process group led by `pid`, if it still exists.
///
/// Idempotent: a group that is already gone is not an error, because
/// the caller is cleaning up, not asking a question.
pub fn kill_process_group(pid: u32) {
    let Some(group) = i32::try_from(pid).ok().filter(|pid| *pid > 0) else {
        return;
    };
    // SAFETY: `kill` with a negative pid addresses the process group; the
    // only kernel state touched is the signal bit of processes in it.
    unsafe {
        libc::kill(-group, libc::SIGKILL);
    }
}

/// An exclusive advisory lock on a file, held until the guard drops.
///
/// The caller supplies the path of the *protected* artefact; the lock
/// lives in a sibling dot-file so locking never truncates or creates
/// the artefact itself. `flock` is whole-file and advisory - every
/// writer that cooperates is serialised, and a non-cooperating reader
/// is unaffected.
pub struct FileLock {
    file: std::fs::File,
    lock_path: std::path::PathBuf,
}

impl FileLock {
    /// Take the exclusive lock for `target_path`, blocking until free.
    ///
    /// # Errors
    ///
    /// Whatever opening the lock file reports; the lock itself only
    /// fails with the descriptor's own errors.
    pub fn acquire(target_path: &std::path::Path) -> std::io::Result<Self> {
        use std::os::fd::AsRawFd as _;

        let file_name = target_path
            .file_name()
            .map_or_else(|| "target".to_owned(), |name| name.to_string_lossy().into_owned());
        let lock_path = target_path.with_file_name(format!(".{file_name}.lock"));
        if let Some(parent) = lock_path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let file = std::fs::OpenOptions::new().create(true).append(true).open(&lock_path)?;
        // SAFETY: `flock` on a just-opened descriptor; the only kernel
        // state touched is the advisory lock bit on this file.
        let outcome = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        if outcome != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self { file, lock_path })
    }

    /// Where the lock lives, for tests and diagnostics.
    #[must_use]
    pub fn lock_path(&self) -> &std::path::Path {
        &self.lock_path
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd as _;

        // SAFETY: releasing the lock this guard holds; a failure here
        // only delays release to the descriptor's close, which Drop
        // performs immediately after.
        let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Barrier};

    #[test]
    fn two_threads_take_the_lock_in_turns() {
        let dir = std::env::temp_dir().join(format!(
            "supra-ffi-lock-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.subsec_nanos())
        ));
        std::fs::create_dir_all(&dir).expect("dir");
        let target = dir.join("log.txt");

        let inside = Arc::new(AtomicBool::new(false));
        let violated = Arc::new(AtomicBool::new(false));
        let barrier = Arc::new(Barrier::new(2));

        let mut joins = Vec::new();
        for _ in 0..2 {
            let (inside, violated, barrier, target) =
                (Arc::clone(&inside), Arc::clone(&violated), Arc::clone(&barrier), target.clone());
            joins.push(std::thread::spawn(move || {
                barrier.wait();
                let lock = FileLock::acquire(&target).expect("lock");
                if inside.swap(true, Ordering::AcqRel) {
                    violated.store(true, Ordering::Release);
                }
                assert!(lock.lock_path().ends_with(".log.txt.lock"));
                std::thread::sleep(std::time::Duration::from_millis(20));
                inside.store(false, Ordering::Release);
            }));
        }
        for join in joins {
            join.join().expect("thread");
        }
        assert!(!violated.load(Ordering::Acquire), "the lock was held twice at once");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
