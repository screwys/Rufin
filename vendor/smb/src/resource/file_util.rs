use crate::sync_helpers::*;
use maybe_async::*;

/// This trait describes an object that can perform read operations at a specific offset,
/// optionally using a specific channel ID.
///
/// It has a single method, [`ReadAtChannel::read_at_channel`], which takes a mutable buffer,
/// an offset, and an optional channel ID, and returns the number of bytes read.
///
/// Every structure that implements this trait, also implements the [`ReadAt`] trait, being
/// a subset of it. The [`ReadAt::read_at`] method is equivalent to calling
/// [`ReadAtChannel::read_at_channel`] with `channel` set to `None`.
pub trait ReadAtChannel {
    #[cfg(feature = "async")]
    fn read_at_channel(
        &self,
        buf: &mut [u8],
        offset: u64,
        channel: Option<u32>,
    ) -> impl std::future::Future<Output = crate::Result<usize>> + std::marker::Send;
    #[cfg(not(feature = "async"))]
    fn read_at_channel(
        &self,
        buf: &mut [u8],
        offset: u64,
        channel: Option<u32>,
    ) -> crate::Result<usize>;
}

/// This trait describes an object that can perform read operations at a specific offset.
///
/// See [`ReadAtChannel`] for an extended version of this trait, that supports
/// specifying a channel ID for the read operation.
pub trait ReadAt {
    #[cfg(feature = "async")]
    fn read_at(
        &self,
        buf: &mut [u8],
        offset: u64,
    ) -> impl std::future::Future<Output = crate::Result<usize>> + std::marker::Send;
    #[cfg(not(feature = "async"))]
    fn read_at(&self, buf: &mut [u8], offset: u64) -> crate::Result<usize>;
}

impl<T: ReadAtChannel + ?Sized> ReadAt for T {
    #[cfg(feature = "async")]
    fn read_at(
        &self,
        buf: &mut [u8],
        offset: u64,
    ) -> impl std::future::Future<Output = crate::Result<usize>> + std::marker::Send {
        self.read_at_channel(buf, offset, None)
    }
    #[cfg(not(feature = "async"))]
    fn read_at(&self, buf: &mut [u8], offset: u64) -> crate::Result<usize> {
        self.read_at_channel(buf, offset, None)
    }
}

/// This trait describes an object that can perform write operations at a specific offset,
/// optionally using a specific channel ID.
///
/// It has a single method, [`WriteAtChannel::write_at_channel`], which takes a buffer,
/// an offset, and an optional channel ID, and returns the number of bytes written.
///
/// Every structure that implements this trait, also implements the [`WriteAt`] trait, being
/// a subset of it. The [`WriteAt::write_at`] method is equivalent to calling
/// [`WriteAtChannel::write_at_channel`] with `channel` set to `None`.
pub trait WriteAtChannel {
    #[cfg(feature = "async")]
    fn write_at_channel(
        &self,
        buf: &[u8],
        offset: u64,
        channel: Option<u32>,
    ) -> impl std::future::Future<Output = crate::Result<usize>> + std::marker::Send;

    #[cfg(not(feature = "async"))]
    fn write_at_channel(
        &self,
        buf: &[u8],
        offset: u64,
        channel: Option<u32>,
    ) -> crate::Result<usize>;
}

/// This trait describes an object that can perform write operations at a specific offset.
///
/// See [`WriteAtChannel`] for an extended version of this trait, that supports
/// specifying a channel ID for the write operation.
pub trait WriteAt {
    #[cfg(feature = "async")]
    fn write_at(
        &self,
        buf: &[u8],
        offset: u64,
    ) -> impl std::future::Future<Output = crate::Result<usize>> + std::marker::Send;

    #[cfg(not(feature = "async"))]
    fn write_at(&self, buf: &[u8], offset: u64) -> crate::Result<usize>;
}

impl<T: WriteAtChannel + ?Sized> WriteAt for T {
    #[cfg(feature = "async")]
    fn write_at(
        &self,
        buf: &[u8],
        offset: u64,
    ) -> impl std::future::Future<Output = crate::Result<usize>> + std::marker::Send {
        self.write_at_channel(buf, offset, None)
    }

    #[cfg(not(feature = "async"))]
    fn write_at(&self, buf: &[u8], offset: u64) -> crate::Result<usize> {
        self.write_at_channel(buf, offset, None)
    }
}

#[maybe_async(AFIT)]
#[allow(async_fn_in_trait)]
pub trait GetLen {
    async fn get_len(&self) -> crate::Result<u64>;
}

#[maybe_async(AFIT)]
#[allow(async_fn_in_trait)]
pub trait SetLen {
    async fn set_len(&self, len: u64) -> crate::Result<()>;
}

#[cfg(feature = "std-fs-impls")]
mod impls {
    use super::*;

    #[cfg(not(feature = "async"))]
    use std::{
        fs::File,
        io::{Read, Seek, Write},
    };
    #[cfg(feature = "async")]
    use tokio::{
        fs::File,
        io::{AsyncRead, AsyncReadExt, AsyncSeek, AsyncSeekExt, AsyncWrite, AsyncWriteExt},
    };

    #[cfg(feature = "async")]
    pub trait ReadSeek: AsyncRead + AsyncSeek + Unpin {}
    #[cfg(not(feature = "async"))]
    pub trait ReadSeek: Read + Seek {}
    impl ReadSeek for File {}
    impl<F: ReadSeek + Send> ReadAtChannel for Mutex<F> {
        #[maybe_async]
        async fn read_at_channel(
            &self,
            buf: &mut [u8],
            offset: u64,
            _channel: Option<u32>,
        ) -> crate::Result<usize> {
            let mut reader = self
                .lock()
                .await
                .map_err(|e| std::io::Error::other(e.to_string()))?;
            reader.seek(std::io::SeekFrom::Start(offset)).await?;
            Ok(reader.read(buf).await?)
        }
    }

    #[cfg(feature = "async")]
    pub trait WriteSeek: AsyncWrite + AsyncSeek + Unpin {}
    #[cfg(not(feature = "async"))]
    pub trait WriteSeek: Write + Seek {}
    impl WriteSeek for File {}
    impl<F: WriteSeek + Send> WriteAtChannel for Mutex<F> {
        #[maybe_async]
        async fn write_at_channel(
            &self,
            buf: &[u8],
            offset: u64,
            _channel: Option<u32>,
        ) -> crate::Result<usize> {
            let mut writer = self
                .lock()
                .await
                .map_err(|e| std::io::Error::other(e.to_string()))?;
            writer.seek(std::io::SeekFrom::Start(offset)).await?;
            Ok(writer.write(buf).await?)
        }
    }

    impl GetLen for Mutex<File> {
        #[maybe_async]
        async fn get_len(&self) -> crate::Result<u64> {
            let file = self
                .lock()
                .await
                .map_err(|e| std::io::Error::other(e.to_string()))?;
            Ok(file.metadata().await?.len())
        }
    }

    impl SetLen for Mutex<File> {
        #[maybe_async]
        async fn set_len(&self, len: u64) -> crate::Result<()> {
            let file = self
                .lock()
                .await
                .map_err(|e| std::io::Error::other(e.to_string()))?;
            Ok(File::set_len(&file, len).await?)
        }
    }
}

#[cfg(feature = "std-fs-impls")]
pub use impls::*;

#[cfg(not(feature = "single_threaded"))]
mod copy {
    use super::*;

    use std::{
        collections::HashMap,
        sync::{Arc, atomic::AtomicU64},
    };

    #[derive(Debug)]
    pub struct CopyState {
        current_block: AtomicU64,

        last_block: u64,
        total_size: u64,

        max_chunk_size: u64,
        channel_jobs: HashMap<Option<u32>, usize>,
    }

    impl CopyState {
        /// Returns the total size of the file being copied (in bytes).
        pub fn total_size(&self) -> u64 {
            self.total_size
        }

        /// Returns the number of bytes copied so far.
        pub fn bytes_copied(&self) -> u64 {
            let current_block = self.current_block.load(std::sync::atomic::Ordering::SeqCst);
            if current_block > self.last_block {
                self.total_size
            } else {
                current_block * self.max_chunk_size
            }
        }

        /// Returns the progress of the copy operation as a value between 0.0 and 1.0.
        pub fn progress(&self) -> f64 {
            if self.total_size == 0 {
                1.0
            } else {
                self.bytes_copied() as f64 / self.total_size as f64
            }
        }

        /// Returns the number of parallel jobs being used for the copy operation.
        pub fn num_total_jobs(&self) -> usize {
            self.channel_jobs.values().sum()
        }
    }

    /// Generic block copy function.
    ///
    /// # Parameters
    /// - `from`: The source to read from. Must implement `ReadAtChannel` and `GetLen`.
    /// - `to`: The destination to write to. Must implement `WriteAtChannel` and `SetLen`.
    /// - `jobs`: The number of parallel jobs to use. If 0, a default value will be used.
    ///
    /// # Returns
    /// - `Ok(())` if the copy was successful.
    /// - `Err(crate::Error)` if an error occurred.
    ///
    /// # Notes
    /// - To report progress, use the [`prepare_parallel_copy`] function to get a `CopyState`, and then
    ///   use that to report progress while the copy is running.
    /// - This function performs operations against the default chanel of the connection.
    ///   To specify the number of jobs per channel, use the [`block_copy_channel`] function instead.
    #[maybe_async]
    pub async fn block_copy<
        F: ReadAtChannel + GetLen + Send + Sync + 'static,
        T: WriteAtChannel + SetLen + Send + Sync + 'static,
    >(
        from: F,
        to: T,
        jobs: usize,
    ) -> crate::Result<()> {
        let copy_state = prepare_parallel_copy(&from, &to, HashMap::from([(None, jobs)])).await?;

        log::debug!("Starting parallel copy: {copy_state:?}",);
        start_parallel_copy(from, to, Arc::new(copy_state)).await?;

        Ok(())
    }

    /// Generic block copy function with channel support.
    ///
    /// # Parameters
    /// - `from`: The source to read from. Must implement `ReadAtChannel` and `GetLen`.
    /// - `to`: The destination to write to. Must implement `WriteAtChannel` and `SetLen`.
    /// - `channel_jobs`: A map of channel IDs to the number of jobs to use for each channel.
    ///
    /// # Returns
    /// - `Ok(())` if the copy was successful.
    /// - `Err(crate::Error)` if an error occurred.
    ///
    /// # Notes
    /// - To report progress, use the [`prepare_parallel_copy`] function to get a `CopyState`, and then
    ///   use that to report progress while the copy is running.
    #[maybe_async]
    pub async fn block_copy_channel<
        F: ReadAtChannel + GetLen + Send + Sync + 'static,
        T: WriteAtChannel + SetLen + Send + Sync + 'static,
    >(
        from: F,
        to: T,
        channel_jobs: &HashMap<u32, usize>,
    ) -> crate::Result<()> {
        let channel_jobs = channel_jobs
            .iter()
            .map(|(&k, &v)| (Some(k), v))
            .collect::<HashMap<_, _>>();

        let copy_state = prepare_parallel_copy(&from, &to, channel_jobs).await?;

        log::debug!("Starting parallel copy: {copy_state:?}",);
        start_parallel_copy(from, to, Arc::new(copy_state)).await?;

        Ok(())
    }

    /// Returns a CopyState that can be used to start a parallel copy.
    ///
    /// pass the return value to start_parallel_copy to start the copy.
    ///
    /// this is mostly useful for cases that require an additional interaction with the
    /// state, beyond the copy workers themselves - for example, reporting progress.
    /// if you don't need that, just use the [`block_copy`] or [`block_copy_channel`] functions.
    ///
    /// # Parameters
    /// - `from`: The source to read from. Must implement `ReadAtChannel` and `GetLen`.
    /// - `to`: The destination to write to. Must implement `WriteAtChannel` and `SetLen`.
    /// - `channel_jobs`: A map of channel IDs to the number of jobs to use for each channel.
    ///   Use `None` as the key for the default channel. The total number of jobs will be the sum of all values in the map.
    ///   If the map is empty, a default value will be used. Setting both None and Some values is allowed, and the default channel
    ///   will use the total number of jobs specified for it. If any channel is specified with 0 jobs, it will use the default number of jobs.
    #[maybe_async]
    pub async fn prepare_parallel_copy<
        F: ReadAtChannel + GetLen + Send + Sync + 'static,
        T: WriteAtChannel + SetLen + Send + Sync + 'static,
    >(
        from: &F,
        to: &T,
        mut channel_jobs: HashMap<Option<u32>, usize>,
    ) -> crate::Result<CopyState> {
        const AUTO_JOB_INDICATOR: usize = 0;
        if channel_jobs.is_empty() {
            channel_jobs.insert(None, AUTO_JOB_INDICATOR); // default
        }

        const MAX_JOBS_PER_CHANNEL: usize = 128;
        const AUTO_JOBS: usize = 16;
        for (&channel, jobs) in channel_jobs.iter_mut() {
            if *jobs > MAX_JOBS_PER_CHANNEL {
                return Err(crate::Error::InvalidArgument(format!(
                    "Number of jobs for channel {channel:?} exceeds maximum allowed (128)"
                )));
            }

            if *jobs == AUTO_JOB_INDICATOR {
                log::debug!("No jobs specified for channel {channel:?}, using default: 16",);
                *jobs = AUTO_JOBS;
            }
        }

        const MAX_TOTAL_JOBS: usize = 512;
        if channel_jobs.values().sum::<usize>() > MAX_TOTAL_JOBS {
            return Err(crate::Error::InvalidArgument(format!(
                "Total number of jobs exceeds maximum allowed ({MAX_TOTAL_JOBS})"
            )));
        }

        const CHUNK_SIZE: u64 = 2u64.pow(16);

        let file_length = from.get_len().await?;
        to.set_len(file_length).await?;

        if file_length == 0 {
            log::debug!("Source file is empty, nothing to copy.");
            return Ok(CopyState {
                current_block: AtomicU64::new(0),
                last_block: 0,
                total_size: 0,
                max_chunk_size: CHUNK_SIZE,
                channel_jobs,
            });
        }

        Ok(CopyState {
            current_block: AtomicU64::new(0),
            last_block: file_length / CHUNK_SIZE,
            total_size: file_length,
            max_chunk_size: CHUNK_SIZE,
            channel_jobs,
        })
    }

    /// Starts a parallel copy using the provided [`CopyState`].
    ///
    /// See [`prepare_parallel_copy`] for more details.
    #[cfg(feature = "async")]
    pub async fn start_parallel_copy<
        F: ReadAtChannel + GetLen + Send + Sync + 'static,
        T: WriteAtChannel + SetLen + Send + Sync + 'static,
    >(
        from: F,
        to: T,
        state: Arc<CopyState>,
    ) -> crate::Result<()> {
        use tokio::task::JoinSet;

        to.set_len(from.get_len().await?).await?;

        let to = Arc::new(to);
        let from = Arc::new(from);

        let mut handles = JoinSet::new();
        for (&channel_id, &jobs) in state.channel_jobs.iter() {
            for task_id in 0..jobs {
                let from = from.clone();
                let to = to.clone();
                let state = state.clone();
                handles.spawn(async move {
                    block_copy_task(from, to, state, task_id, channel_id).await
                });
            }
        }

        handles.join_all().await;
        Ok(())
    }

    /// Starts a parallel copy using the provided [`CopyState`].
    ///
    /// See [`prepare_parallel_copy`] for more details.
    #[cfg(feature = "multi_threaded")]
    pub fn start_parallel_copy<
        F: ReadAtChannel + GetLen + Send + Sync + 'static,
        T: WriteAtChannel + SetLen + Send + Sync + 'static,
    >(
        from: F,
        to: T,
        state: Arc<CopyState>,
    ) -> crate::Result<()> {
        let from = Arc::new(from);
        let to = Arc::new(to);

        let mut handles = Vec::new();
        for (&channel_id, &jobs) in state.channel_jobs.iter() {
            for task_id in 0..jobs {
                let from = from.clone();
                let to = to.clone();
                let state = state.clone();
                let handle = std::thread::spawn(move || {
                    block_copy_task(from.clone(), to, state, task_id, channel_id)
                });
                handles.push(handle);
            }
        }

        for handle in handles {
            handle.join().unwrap()?;
        }

        Ok(())
    }

    #[maybe_async]
    async fn block_copy_task<
        F: ReadAtChannel + GetLen + Send + Sync,
        T: WriteAtChannel + SetLen + Send + Sync,
    >(
        from: Arc<F>,
        to: Arc<T>,
        state: Arc<CopyState>,
        task_id: usize,
        channel_id: Option<u32>,
    ) -> crate::Result<()> {
        log::debug!("Starting copy task {task_id} of channel {channel_id:?}",);

        let mut curr_chunk = vec![0u8; state.max_chunk_size as usize];

        loop {
            let current_block = state
                .current_block
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if current_block > state.last_block {
                break;
            }
            let chunk_size = if current_block == state.last_block {
                let last_block_leftover = state.total_size % state.max_chunk_size;
                if last_block_leftover == 0 {
                    break;
                }
                last_block_leftover
            } else {
                state.max_chunk_size
            } as usize;

            let offset = current_block * state.max_chunk_size;
            let bytes_read = from
                .read_at_channel(&mut curr_chunk[..chunk_size], offset, channel_id)
                .await?;
            if bytes_read < chunk_size {
                log::warn!(
                    "Task {task_id}@{channel_id:?}: Read less bytes than expected. File might be corrupt. Expected: {chunk_size}: {bytes_read}"
                );
            }
            let valid_chunk_end = bytes_read;
            to.write_at_channel(&curr_chunk[..valid_chunk_end], offset, channel_id)
                .await?;
        }
        log::debug!("Copy task {task_id}@{channel_id:?} completed",);
        Ok(())
    }
}

#[cfg(feature = "single_threaded")]
mod copy {
    use super::*;

    /// Generic block copy function.
    pub fn block_copy<F: ReadAtChannel + GetLen, T: WriteAtChannel + SetLen>(
        from: F,
        to: T,
    ) -> crate::Result<()> {
        block_copy_progress(from, to, None)
    }

    /// Generic block copy function with progress callback.
    ///
    /// * `progress_callback` - A callback function that will be called with the number of bytes copied so far.
    ///
    /// # Note
    /// * A simpler method named [`block_copy`] is also available, which does not take a progress callback.
    pub fn block_copy_progress<F: ReadAtChannel + GetLen, T: WriteAtChannel + SetLen>(
        from: F,
        to: T,
        progress_callback: Option<&dyn Fn(u64)>,
    ) -> crate::Result<()> {
        block_copy_channel_progress(from, to, progress_callback, None)
    }

    /// Generic block copy function with progress callback and channel specification.
    ///
    /// * `channel` - The channel ID to use for the copy operation. If `None`, the default channel will be used.
    ///
    /// # Note
    /// * A simpler method named [`block_copy`] and [`block_copy_progress`] are also available,
    pub fn block_copy_channel_progress<F: ReadAtChannel + GetLen, T: WriteAtChannel + SetLen>(
        from: F,
        to: T,
        progress_callback: Option<&dyn Fn(u64)>,
        channel: Option<u32>,
    ) -> crate::Result<()> {
        let file_length = from.get_len()?;
        to.set_len(file_length)?;

        if file_length == 0 {
            log::debug!("Source file is empty, nothing to copy.");
            return Ok(());
        }

        let mut curr_chunk = vec![0u8; 2u64.pow(16) as usize];
        let mut offset = 0;

        while offset < file_length {
            let chunk_size = if offset + curr_chunk.len() as u64 > file_length {
                (file_length - offset) as usize
            } else {
                curr_chunk.len()
            };
            let bytes_read =
                from.read_at_channel(&mut curr_chunk[..chunk_size], offset, channel)?;
            if bytes_read < chunk_size {
                log::warn!(
                    "Read less bytes than expected. File might be corrupt. Expected: {chunk_size}: {bytes_read}"
                );
            }
            to.write_at(&curr_chunk[..bytes_read], offset)?;
            offset += bytes_read as u64;
            if let Some(callback) = progress_callback {
                callback(offset);
            }
        }
        Ok(())
    }
}

pub use copy::*;
