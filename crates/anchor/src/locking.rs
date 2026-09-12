use std::fmt;
use std::fs::File;
use std::sync::{Mutex, MutexGuard, TryLockError};
use std::thread;
use std::time::{Duration, Instant};

use fs2::FileExt;

use crate::error::{AppError, AppResult};

pub(crate) const CROSS_PROCESS_LOCK_TIMEOUT: Duration = Duration::from_secs(1);
const CROSS_PROCESS_LOCK_POLL: Duration = Duration::from_millis(25);

#[derive(Debug)]
pub(crate) enum BoundedLockError {
    Busy { waited_ms: u64 },
    Io(std::io::Error),
}

impl fmt::Display for BoundedLockError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy { waited_ms } => {
                write!(formatter, "lock remained busy for {waited_ms} ms")
            }
            Self::Io(error) => write!(formatter, "lock operation failed: {error}"),
        }
    }
}

impl std::error::Error for BoundedLockError {}

fn lock_is_contended(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::WouldBlock || matches!(error.raw_os_error(), Some(32 | 33))
}

pub(crate) fn lock_file_exclusive(file: &File) -> Result<(), BoundedLockError> {
    lock_file_exclusive_for(file, CROSS_PROCESS_LOCK_TIMEOUT)
}

pub(crate) fn lock_file_exclusive_for(
    file: &File,
    timeout: Duration,
) -> Result<(), BoundedLockError> {
    let started = Instant::now();
    loop {
        match FileExt::try_lock_exclusive(file) {
            Ok(()) => return Ok(()),
            Err(error) if lock_is_contended(&error) => {
                if started.elapsed() >= timeout {
                    return Err(BoundedLockError::Busy {
                        waited_ms: started.elapsed().as_millis().min(u64::MAX as u128) as u64,
                    });
                }
                thread::sleep(CROSS_PROCESS_LOCK_POLL.min(timeout));
            }
            Err(error) => return Err(BoundedLockError::Io(error)),
        }
    }
}

pub(crate) fn lock_file_app(file: &File, code: &str, context: &str) -> AppResult<()> {
    match lock_file_exclusive(file) {
        Ok(()) => Ok(()),
        Err(BoundedLockError::Busy { waited_ms }) => Err(AppError::Message(format!(
            "{code}_BUSY: {context} lock remained busy for {waited_ms} ms; retryable=true; retry later"
        ))),
        Err(BoundedLockError::Io(error)) => Err(AppError::Message(format!(
            "{code}_FAILED: {context} lock failed: {error}"
        ))),
    }
}

pub(crate) fn lock_file_string(file: &File, code: &str, context: &str) -> Result<(), String> {
    match lock_file_exclusive(file) {
        Ok(()) => Ok(()),
        Err(BoundedLockError::Busy { waited_ms }) => Err(format!(
            "{code}_BUSY: {context} lock remained busy for {waited_ms} ms; retryable=true; retry later"
        )),
        Err(BoundedLockError::Io(error)) => {
            Err(format!("{code}_FAILED: {context} lock failed: {error}"))
        }
    }
}

pub(crate) fn lock_mutex_app<'a>(
    mutex: &'a Mutex<()>,
    code: &str,
    context: &str,
) -> AppResult<MutexGuard<'a, ()>> {
    let started = Instant::now();
    loop {
        match mutex.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(TryLockError::WouldBlock) => {
                if started.elapsed() >= CROSS_PROCESS_LOCK_TIMEOUT {
                    return Err(AppError::Message(format!(
                        "{code}_BUSY: {context} in-process lock remained busy for {} ms; retryable=true; retry later",
                        started.elapsed().as_millis()
                    )));
                }
                thread::sleep(CROSS_PROCESS_LOCK_POLL);
            }
            Err(TryLockError::Poisoned(_)) => {
                return Err(AppError::Message(format!(
                    "{code}_FAILED: {context} in-process lock is poisoned"
                )))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;

    #[test]
    fn contended_file_lock_times_out_instead_of_blocking_forever() {
        let temp = tempfile::tempdir().expect("temp");
        let path = temp.path().join("bounded.lock");
        let first = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .expect("first lock file");
        let second = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .expect("second lock file");
        FileExt::try_lock_exclusive(&first).expect("first lock");

        let started = Instant::now();
        let error = lock_file_exclusive_for(&second, Duration::from_millis(80))
            .expect_err("second lock must time out");
        assert!(matches!(error, BoundedLockError::Busy { .. }));
        assert!(started.elapsed() < Duration::from_secs(1));

        FileExt::unlock(&first).expect("unlock first");
        lock_file_exclusive_for(&second, Duration::from_millis(80)).expect("lock after release");
        FileExt::unlock(&second).expect("unlock second");
    }

    #[test]
    fn contended_process_mutex_is_bounded_and_retryable() {
        let mutex = Mutex::new(());
        let guard = mutex.lock().expect("guard");
        let started = Instant::now();
        let error =
            lock_mutex_app(&mutex, "TEST_LOCK", "test").expect_err("contended mutex must time out");
        assert!(error.to_string().contains("TEST_LOCK_BUSY"));
        assert!(error.to_string().contains("retryable=true"));
        assert!(started.elapsed() < Duration::from_secs(2));
        drop(guard);
    }
}
