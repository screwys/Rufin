//! This module contains the implementation for the async worker(s).
//!
//! Depending on the crate configuration, one of the two backends will be used:
//! - async_backend for async workers
//! - threading_backend for sync workers
//!
//! The effective backend is exported as [ParallelWorker] from this module.

pub mod backend_trait;
pub mod base;

#[cfg(feature = "multi_threaded")]
pub mod threading_backend;
#[cfg(feature = "multi_threaded")]
use threading_backend::ThreadingBackend as Backend;

#[cfg(feature = "async")]
pub mod async_backend;
#[cfg(feature = "async")]
use async_backend::AsyncBackend as Backend;

pub type ParallelWorker = base::ParallelWorker<Backend>;
