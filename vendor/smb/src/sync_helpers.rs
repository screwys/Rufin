//! This is a helper module that allows easy access and usage of
//! Async/Multi-threaded features in the library, according to the
//! features enabled.
#[cfg(not(feature = "async"))]
pub use std::sync::{Mutex, MutexGuard, OnceLock as OnceCell, RwLock};

#[cfg(feature = "multi_threaded")]
pub use std::{sync::mpsc, thread::JoinHandle};
#[cfg(not(feature = "async"))]
use thiserror::Error;
#[cfg(feature = "async")]
pub use tokio::{
    select,
    sync::{AcquireError, MutexGuard, OnceCell, Semaphore, mpsc},
    task::JoinHandle,
};
#[cfg(feature = "async")]
pub use tokio_util::sync::CancellationToken;

#[cfg(feature = "async")]
use std::sync::LockResult;

// for convenience
pub use maybe_async::maybe_async;
pub use std::sync::{Arc, Weak};

/// A wrapper for [tokio::sync::RwLock] that mocks the behavior of [std::sync::RwLock].
#[cfg(feature = "async")]
#[derive(Debug, Default)]
pub struct RwLock<T: ?Sized> {
    inner: tokio::sync::RwLock<T>,
}

#[cfg(feature = "async")]
impl<T> RwLock<T> {
    #[inline]
    pub fn new(value: T) -> Self {
        Self {
            inner: tokio::sync::RwLock::new(value),
        }
    }

    #[inline]
    pub async fn read(&self) -> LockResult<tokio::sync::RwLockReadGuard<'_, T>> {
        Ok(self.inner.read().await)
    }

    #[inline]
    pub async fn write(&self) -> LockResult<tokio::sync::RwLockWriteGuard<'_, T>> {
        Ok(self.inner.write().await)
    }
}

/// A wrapper for [tokio::sync::Mutex] that mocks the behavior of [std::sync::Mutex].
#[cfg(feature = "async")]
#[derive(Debug, Default)]
pub struct Mutex<T: ?Sized> {
    inner: tokio::sync::Mutex<T>,
}

#[cfg(feature = "async")]
impl<T> Mutex<T> {
    #[inline]
    pub fn new(value: T) -> Self {
        Self {
            inner: tokio::sync::Mutex::new(value),
        }
    }

    #[inline]
    pub async fn lock(&self) -> LockResult<tokio::sync::MutexGuard<'_, T>> {
        Ok(self.inner.lock().await)
    }
}

#[cfg(not(feature = "async"))]
pub struct Semaphore {
    inner: std::sync::Mutex<u32>,
    condvar: std::sync::Condvar,
}

#[cfg(not(feature = "async"))]
#[derive(Debug, Error)]
pub struct AcquireError;

#[cfg(not(feature = "async"))]
impl std::fmt::Display for AcquireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Failed to acquire semaphore.")
    }
}

/// An implementation of a limited tokio::sync::Semaphore using std::sync primitives -
/// for multi-threaded environments, where tokio::sync::Semaphore is not available.
#[cfg(not(feature = "async"))]
impl Semaphore {
    pub fn new(value: u32) -> Self {
        Self {
            inner: std::sync::Mutex::new(value),
            condvar: std::sync::Condvar::new(),
        }
    }

    pub fn acquire(&self) -> Result<SemaphorePermit<'_>, AcquireError> {
        let mut guard = self.inner.lock().unwrap();
        while *guard == 0 {
            guard = self.condvar.wait(guard).unwrap();
        }
        *guard -= 1;
        Ok(SemaphorePermit {
            sem: self,
            count: 1,
        })
    }

    pub fn acquire_many(&self, count: u32) -> Result<SemaphorePermit<'_>, AcquireError> {
        let guard = self.inner.lock().unwrap();
        let mut guard = self.condvar.wait_while(guard, |c| *c < count).unwrap();
        *guard -= count;
        Ok(SemaphorePermit { sem: self, count })
    }

    pub fn add_permits(&self, count: usize) {
        let mut guard = self.inner.lock().unwrap();
        *guard += count as u32;
        self.condvar.notify_all();
    }

    pub fn available_permits(&self) -> usize {
        *self.inner.lock().unwrap() as usize
    }
}

#[cfg(not(feature = "async"))]
pub struct SemaphorePermit<'a> {
    sem: &'a Semaphore,
    count: u32,
}

#[cfg(not(feature = "async"))]
impl SemaphorePermit<'_> {
    pub fn forget(&mut self) {
        self.count = 0;
    }
}

#[cfg(not(feature = "async"))]
impl<'a> Drop for SemaphorePermit<'a> {
    fn drop(&mut self) {
        self.sem.add_permits(self.count as usize);
    }
}
