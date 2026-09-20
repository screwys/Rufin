pub mod worker_trait;
pub use worker_trait::*;

#[cfg(feature = "single_threaded")]
pub mod single_worker;
#[cfg(feature = "single_threaded")]
pub use single_worker::SingleWorker as WorkerImpl;

#[cfg(not(feature = "single_threaded"))]
mod parallel;
#[cfg(not(feature = "single_threaded"))]
pub use parallel::ParallelWorker as WorkerImpl;
