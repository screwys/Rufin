use super::ResourceHandle;
use crate::Error;
use crate::msg_handler::{MessageHandler, ReceiveOptions};
use crate::sync_helpers::*;
use maybe_async::*;
use smb_fscc::*;
use smb_msg::*;
use std::ops::{Deref, DerefMut};
use std::time::Duration;

/// A directory resource on the server.
/// This is used to query the directory for its contents,
/// and may not be created directly -- but via [Resource][super::Resource], opened
/// from a [Tree][crate::tree::Tree]
pub struct Directory {
    pub handle: ResourceHandle,
    access: DirAccessMask,
    /// This lock prevents iterating the directory twice at the same time.
    /// This is required since query directory state is tied to the handle of
    /// the directory (hence, to this structure's instance).
    query_lock: Mutex<()>,
}

#[maybe_async(AFIT)]
impl Directory {
    pub fn new(handle: ResourceHandle) -> Self {
        let access: DirAccessMask = handle.access.into();
        Directory {
            handle,
            access,
            query_lock: Default::default(),
        }
    }

    /// An internal method that performs a query on the directory.
    /// # Arguments
    /// * `pattern` - The pattern to match against the file names in the directory. Use wildcards like `*` and `?` to match multiple files.
    /// * `restart` - Whether to restart the scan or not. This is used to indicate whether this is the first query or not.
    /// # Returns
    /// * A vector of [`QueryDirectoryInfoValue`] objects, containing the results of the query.
    /// * If the query returned [`Status::NoMoreFiles`], an empty vector is returned.
    async fn send_query<T>(
        &self,
        pattern: &str,
        restart: bool,
        buffer_size: u32,
    ) -> crate::Result<Vec<T>>
    where
        T: QueryDirectoryInfoValue + for<'a> binrw::prelude::BinWrite<Args<'a> = ()>,
    {
        if !self.access.list_directory() {
            return Err(Error::MissingPermissions("file_list_directory".to_string()));
        }

        debug_assert!(buffer_size <= self.conn_info.negotiation.max_transact_size);
        if buffer_size > self.conn_info.negotiation.max_transact_size {
            return Err(Error::InvalidArgument(format!(
                "Buffer size {} exceeds maximum transact size {}",
                buffer_size, self.conn_info.negotiation.max_transact_size
            )));
        }

        log::debug!("Querying directory {}", self.handle.name());

        let response = self
            .handle
            .send_receive(
                QueryDirectoryRequest {
                    file_information_class: T::CLASS_ID,
                    flags: QueryDirectoryFlags::new().with_restart_scans(restart),
                    file_index: 0,
                    file_id: self.handle.file_id()?,
                    output_buffer_length: buffer_size,
                    file_name: pattern.into(),
                }
                .into(),
            )
            .await;

        let response = match response {
            Ok(res) => res,
            Err(Error::UnexpectedMessageStatus(Status::U32_NO_MORE_FILES)) => {
                log::debug!("No more files in directory");
                return Ok(vec![]);
            }
            Err(Error::UnexpectedMessageStatus(Status::U32_INFO_LENGTH_MISMATCH)) => {
                return Err(Error::InvalidArgument(format!(
                    "Provided query buffer size {buffer_size} is too small to contain directory information"
                )));
            }
            Err(e) => {
                log::error!("Error querying directory: {e}");
                return Err(e);
            }
        };

        Ok(response
            .message
            .content
            .to_querydirectory()?
            .read_output()?)
    }

    const QUERY_DIRECTORY_DEFAULT_BUFFER_SIZE: u32 = 0x10000;

    /// Asynchronously iterates over the directory contents, using the provided pattern and information type.
    /// # Arguments
    /// * `pattern` - The pattern to match against the file names in the directory. Use wildcards like `*` and `?` to match multiple files.
    /// * `info` - The information type to query. This is a trait object that implements the [`QueryDirectoryInfoValue`] trait.
    /// # Returns
    /// * An iterator over the directory contents, yielding [`QueryDirectoryInfoValue`] objects.
    /// # Returns
    /// [`iter_stream::QueryDirectoryStream`] - Which implements [futures_core::Stream] and can be used to iterate over the directory contents.
    /// # Notes
    /// * **IMPORTANT** Calling this method BLOCKS ANY ADDITIONAL CALLS to this method on THIS structure instance.
    ///   Hence, you should not call this method on the same instance from multiple threads. This is for thread safety,
    ///   since SMB2 does not allow multiple queries on the same handle at the same time. Re-open the directory and
    ///   create a new instance of this structure to query the directory again.
    /// * You must use [`futures_util::StreamExt`] to consume the stream.
    ///   See (<https://tokio.rs/tokio/tutorial/streams>) for more information on how to use streams.
    #[cfg(feature = "async")]
    pub fn query<'a, T>(
        this: &'a Arc<Self>,
        pattern: &str,
    ) -> impl Future<Output = crate::Result<iter_stream::QueryDirectoryStream<'a, T>>>
    where
        T: QueryDirectoryInfoValue + for<'b> binrw::prelude::BinWrite<Args<'b> = ()> + Send,
    {
        Self::query_with_options(this, pattern, Self::QUERY_DIRECTORY_DEFAULT_BUFFER_SIZE)
    }

    /// Asynchronously iterates over the directory contents, using the provided pattern and information type.
    /// # Arguments
    /// * `pattern` - The pattern to match against the file names in the directory. Use wildcards like `*` and `?` to match multiple files.
    /// * `info` - The information type to query. This is a trait object that implements the [`QueryDirectoryInfoValue`] trait.
    /// * `buffer_size` - The size of the query buffer, in bytes.
    /// # Returns
    /// * An iterator over the directory contents, yielding [`QueryDirectoryInfoValue`] objects.
    /// # Returns
    /// [`iter_stream::QueryDirectoryStream`] - Which implements [futures_core::Stream] and can be used to iterate over the directory contents.
    /// # Notes
    /// * **IMPORTANT** Calling this method BLOCKS ANY ADDITIONAL CALLS to this method on THIS structure instance.
    ///   Hence, you should not call this method on the same instance from multiple threads. This is for thread safety,
    ///   since SMB2 does not allow multiple queries on the same handle at the same time. Re-open the directory and
    ///   create a new instance of this structure to query the directory again.
    /// * You must use [`futures_util::StreamExt`] to consume the stream.
    ///   See [<https://tokio.rs/tokio/tutorial/streams>] for more information on how to use streams.
    /// * The actual buffer size that may be used depends on the negotiated transact size given by the server.
    ///   In case of `buffer_size` > `max_transact_size`, the function would use the minimum, and log a warning.
    #[cfg(feature = "async")]
    pub async fn query_with_options<'a, T>(
        this: &'a Arc<Self>,
        pattern: &str,
        buffer_size: u32,
    ) -> crate::Result<iter_stream::QueryDirectoryStream<'a, T>>
    where
        T: QueryDirectoryInfoValue + for<'b> binrw::prelude::BinWrite<Args<'b> = ()> + Send,
    {
        let max_allowed_buffer_size = this.conn_info.negotiation.max_transact_size;
        if buffer_size > max_allowed_buffer_size {
            log::warn!(
                "Buffer size {} is larger than max transact size {}. Using minimum.",
                buffer_size,
                max_allowed_buffer_size
            );
        }
        let buffer_size = buffer_size.min(max_allowed_buffer_size);

        iter_stream::QueryDirectoryStream::new(this, pattern.to_string(), buffer_size).await
    }

    /// Synchronously iterates over the directory contents, using the provided pattern and information type.
    /// # Arguments
    /// * `pattern` - The pattern to match against the file names in the directory. Use wildcards like `*` and `?` to match multiple files.
    /// # Returns
    /// * An iterator over the directory contents, yielding [`QueryDirectoryInfoValue`] objects.
    /// # Notes
    /// * **IMPORTANT**: Calling this method BLOCKS ANY ADDITIONAL CALLS to this method on THIS structure instance.
    ///   Hence, you should not call this method on the same instance from multiple threads. This is for safety,
    ///   since SMB2 does not allow multiple queries on the same handle at the same time.
    #[cfg(not(feature = "async"))]
    pub fn query<'a, T>(
        &'a self,
        pattern: &str,
    ) -> crate::Result<iter_sync::QueryDirectoryIterator<'a, T>>
    where
        T: QueryDirectoryInfoValue,
    {
        Self::query_with_options(self, pattern, Self::QUERY_DIRECTORY_DEFAULT_BUFFER_SIZE)
    }

    /// Synchronously iterates over the directory contents, using the provided pattern and information type.
    /// # Arguments
    /// * `pattern` - The pattern to match against the file names in the directory. Use wildcards like `*` and `?` to match multiple files.
    /// # Returns
    /// * An iterator over the directory contents, yielding [`QueryDirectoryInfoValue`] objects.
    /// # Notes
    /// * **IMPORTANT**: Calling this method BLOCKS ANY ADDITIONAL CALLS to this method on THIS structure instance.
    ///   Hence, you should not call this method on the same instance from multiple threads. This is for safety,
    ///   since SMB2 does not allow multiple queries on the same handle at the same time.
    #[cfg(not(feature = "async"))]
    pub fn query_with_options<'a, T>(
        &'a self,
        pattern: &str,
        buffer_size: u32,
    ) -> crate::Result<iter_sync::QueryDirectoryIterator<'a, T>>
    where
        T: QueryDirectoryInfoValue,
    {
        iter_sync::QueryDirectoryIterator::new(self, pattern.to_string(), buffer_size)
    }

    /// Watches the directory for changes.
    /// # Arguments
    /// * `filter` - The filter to use for the changes. This is a bitmask of the changes to watch for.
    /// * `recursive` - Whether to watch the directory recursively or not.
    /// # Returns
    /// * A vector of [`FileNotifyInformation`] objects, containing the changes that occurred.
    /// # Notes
    /// * This is a long-running operation, and will block until a result is received. See [`watch_timeout`][Self::watch_timeout] for a version that supports a timeout.
    pub async fn watch(
        &self,
        filter: NotifyFilter,
        recursive: bool,
    ) -> crate::Result<Vec<FileNotifyInformation>> {
        self.watch_timeout(filter, recursive, Duration::MAX).await
    }

    /// Watches the directory for changes, with a specified timeout.
    /// # Arguments
    /// * `filter` - The filter to use for the changes. This is a bitmask of the changes to watch for.
    /// * `recursive` - Whether to watch the directory recursively or not.
    /// # Returns
    /// * A vector of [`FileNotifyInformation`] objects, containing the changes that occurred.
    /// # Notes
    /// * This is a long-running operation, and will block until a result is received or the provided timeout elapses.
    ///  If the timeout elapses, an error of type [`Error::OperationTimeout`] is returned.
    /// * A similar method without timeout is available as [`watch`][Self::watch].
    pub async fn watch_timeout(
        &self,
        filter: NotifyFilter,
        recursive: bool,
        timeout: std::time::Duration,
    ) -> crate::Result<Vec<FileNotifyInformation>> {
        self._watch_options(
            filter,
            recursive,
            ReceiveOptions::new().with_timeout(timeout),
        )
        .await
        .into()
    }

    #[cfg(feature = "async")]
    /// Watches the directory for changes, returning a [`Stream`][`futures_core::Stream`] of notifications.
    ///
    /// * See [`watch_stream_cancellable`][Self::watch_stream_cancellable] for a version that supports cancellation,
    ///  via a [`CancellationToken`].
    ///
    /// # Arguments
    /// * `filter` - The filter to use for the changes. This is a bitmask of the changes to watch for.
    /// * `recursive` - Whether to watch the directory recursively or not.
    /// # Returns
    /// * A stream of [`FileNotifyInformation`] objects, containing the changes that occurred.
    ///
    /// # Notes
    /// Error handling in this stream is done by returning `Result<FileNotifyInformation>`.
    pub fn watch_stream(
        this: &Arc<Self>,
        filter: NotifyFilter,
        recursive: bool,
    ) -> crate::Result<impl futures_core::Stream<Item = crate::Result<FileNotifyInformation>>> {
        Self::watch_stream_cancellable(this, filter, recursive, Default::default())
    }

    #[cfg(feature = "async")]
    pub fn watch_stream_cancellable(
        this: &Arc<Self>,
        filter: NotifyFilter,
        recursive: bool,
        cancel: tokio_util::sync::CancellationToken,
    ) -> crate::Result<impl futures_core::Stream<Item = crate::Result<FileNotifyInformation>>> {
        // Since watching for notifications is more passive, this does not require the same level
        // of synchronization as querying the directory - since we won't DoS the server by sending
        // too many requests.

        use tokio::select;
        use tokio_stream::wrappers::ReceiverStream;

        let (sender, receiver) = tokio::sync::mpsc::channel(1024);
        let (watch_tx, mut watch_rx) = tokio::sync::mpsc::channel(1024);

        let receive_options = ReceiveOptions::default()
            .with_timeout(Duration::MAX)
            .with_async_msg_ids(Default::default());

        // Receive task is required to avoid race conditions.
        // if the receive task is aborted, we might miss a cancellation message.
        // so cancelling a running watch should only be by cleanup/cancel ack messages,
        // or stream drop.
        tokio::spawn({
            let receive_options = receive_options.clone();

            let directory = this.clone();
            async move {
                loop {
                    select! {
                        _ = watch_tx.closed() => {
                            // Receiver dropped, exit the loop.
                            break;
                        }
                        result = directory
                            ._watch_options(filter, recursive, receive_options.clone())
                            =>  {
                            let should_stop = matches!(result, DirectoryWatchResult::Cancelled | DirectoryWatchResult::Cleanup);
                            if watch_tx.send(result).await.is_err() {
                                break; // Receiver dropped
                            }
                            if should_stop {
                                break;
                            }
                        }
                    }
                }
            }
        });

        tokio::spawn({
            let directory = this.clone();
            async move {
                let mut cancel_called = false;
                loop {
                    select! {
                        biased;
                        _ = sender.closed(), if sender.is_closed() && !cancel.is_cancelled() => {
                            // Sender close. request a cancellation. That triggers the branch above.
                            log::debug!("Watch receiver closed, stopping watch by raising cancellation.");
                            if !cancel_called {
                                cancel.cancel();
                            }
                        }
                        _ = cancel.cancelled(), if !cancel_called => {
                            // Cancellation step 1: send cancel request to server.
                            log::debug!("Watch cancelled by user");
                            directory.send_cancel(receive_options.async_msg_ids.as_ref().unwrap()).await.ok();
                            cancel_called = true;
                            // Now, wait for the server to confirm cancellation.
                        }
                        result = watch_rx.recv() => {
                            match result {
                                Some(DirectoryWatchResult::Notifications(v)) => {
                                    for item in v {
                                        if sender.send(Ok(item)).await.is_err() {
                                            log::debug!("Watch notifications receiver closed, stop sending, begin cancellation.");
                                            break;
                                        }
                                    }
                                }
                                Some(DirectoryWatchResult::Cancelled) => {
                                    if sender.is_closed() {
                                        // Already closed, ignore - cancellation should be complete anyway.
                                        log::debug!("Watch cancelled after sender closed, ignoring.");
                                        break;
                                    }

                                    if !cancel.is_cancelled() {
                                        sender.send(Err(Error::Cancelled("watch cancelled unexpectedly"))).await.ok();
                                    }

                                    // Cancellation step 2: exit the loop.
                                    log::debug!("Watch cancellation complete.");
                                    break;
                                }
                                Some(DirectoryWatchResult::Cleanup) => {
                                    // Server cleaned up the watch, exit the loop.
                                    log::debug!("Watch cleaned up by server. Stopping stream.");
                                    break;
                                }
                                Some(x) => {
                                    let x: crate::Result<_> = x.into();
                                    let x = x.unwrap_err();
                                    log::debug!("Error watching directory: {x}. Stopping stream.");
                                    sender.send(Err(x)).await.map_err(|e| {
                                        log::debug!("Error watching directory after sender closed: {e}. Ignoring.");
                                        e
                                    }).ok();
                                    break; // Exit on error
                                },
                                None => {
                                    log::debug!("Watch internal task ended, stopping stream.");
                                    break; // Internal task ended
                                }
                            }
                        }
                    }
                }
            }
        });

        Ok(ReceiverStream::new(receiver))
    }

    // TODO: Doc

    #[cfg(feature = "multi_threaded")]
    pub fn watch_stream(
        this: &Arc<Self>,
        filter: NotifyFilter,
        recursive: bool,
    ) -> crate::Result<
        impl Iterator<Item = crate::Result<FileNotifyInformation>> + NotifyDirectoryIteratorCancellable,
    > {
        let cancel_handle = NotifyDirectoryIteratorCanceller::new(this);
        iter_mtd::NotifyDirectoryIterator::new(cancel_handle, filter, recursive)
    }

    /// Returns an iterator that watches the directory for changes.
    #[cfg(feature = "single_threaded")]
    pub fn watch_stream(
        this: &Arc<Self>,
        filter: NotifyFilter,
        recursive: bool,
    ) -> crate::Result<impl Iterator<Item = crate::Result<FileNotifyInformation>> + '_> {
        // Simply watch in loop and chain the results.
        let veci = std::iter::from_fn(move || {
            match this
                ._watch_options(filter, recursive, ReceiveOptions::default())
                .into()
            {
                Ok(result) => Some(Ok(result)),
                Err(e) => Some(Err(e)),
            }
        })
        // Flatten the results into a single item iterator
        .flat_map(|result| match result {
            Ok(vec) => vec.into_iter().map(Ok).collect::<Vec<_>>().into_iter(),
            Err(e) => vec![Err(e)].into_iter(),
        });
        Ok(veci)
    }

    /// (Internal) Watches the directory for changes, with an optional timeout.
    ///
    /// This method accepts the `ReceiveOptions` struct, allowing more fine-tuned control over the receive operation.
    /// It uses:
    /// * `timeout` - to set the timeout for the receive operation.
    /// * `async_msg_ids` - to allow async notifications.
    /// * `async_cancel` - to allow cancellation of the receive operation, when crate feature `async` is enabled.
    async fn _watch_options(
        &self,
        filter: NotifyFilter,
        recursive: bool,
        options: ReceiveOptions<'_>,
    ) -> DirectoryWatchResult {
        if !self.access.list_directory() {
            return DirectoryWatchResult::Error(Error::MissingPermissions(
                "list_directory".to_string(),
            ));
        }
        let output_buffer_length = self.calc_transact_size(None);

        let file_id = match self.file_id() {
            Ok(id) => id,
            Err(e) => return DirectoryWatchResult::Error(e),
        };

        let response = self
            .handle
            .handler
            .send_recvo(
                ChangeNotifyRequest {
                    file_id,
                    flags: NotifyFlags::new().with_watch_tree(recursive),
                    completion_filter: filter,
                    output_buffer_length,
                }
                .into(),
                ReceiveOptions {
                    allow_async: true,
                    #[cfg(feature = "async")]
                    async_cancel: options.async_cancel,
                    async_msg_ids: options.async_msg_ids,
                    timeout: options.timeout,
                    cmd: Some(Command::ChangeNotify),
                    status: &[
                        Status::Success,
                        Status::Cancelled,
                        Status::NotifyCleanup,
                        Status::NotifyEnumDir,
                    ],
                    ..Default::default()
                },
            )
            .await;

        let response = match response {
            Ok(res) => match res.message.header.status {
                Status::U32_SUCCESS => res,
                // Cancellation from CancelRequest
                Status::U32_CANCELLED => return DirectoryWatchResult::Cancelled,
                Status::U32_NOTIFY_CLEANUP => return DirectoryWatchResult::Cleanup,
                Status::U32_NOTIFY_ENUM_DIR => {
                    return DirectoryWatchResult::NotifyEnumDir {
                        provided_size: output_buffer_length as usize,
                    };
                }
                s => {
                    log::debug!("Unexpected status while watching directory: {s:?}");
                    return DirectoryWatchResult::Error(Error::UnexpectedMessageStatus(s));
                }
            },
            // Other cancellation (token)
            Err(Error::Cancelled(_)) => return DirectoryWatchResult::Cancelled,
            Err(e) => {
                log::debug!("Error watching directory: {e}");
                return DirectoryWatchResult::Error(e);
            }
        };

        let change_notify = match response.message.content.to_changenotify() {
            Ok(cn) => cn,
            Err(e) => return DirectoryWatchResult::Error(e.into()),
        };

        DirectoryWatchResult::Notifications(change_notify.buffer.into())
    }

    /// Queries the quota information for the current file.
    /// # Arguments
    /// * `info` - The information to query - a [`QueryQuotaInfo`].
    pub async fn query_quota_info(
        &self,
        info: QueryQuotaInfo,
    ) -> crate::Result<Vec<FileQuotaInformation>> {
        self.query_quota_info_with_options(info, None).await
    }
    /// Queries the quota information for the current file.
    /// # Arguments
    /// * `info` - The information to query - a [`QueryQuotaInfo`].
    pub async fn query_quota_info_with_options(
        &self,
        info: QueryQuotaInfo,
        output_buffer_length: Option<usize>,
    ) -> crate::Result<Vec<FileQuotaInformation>> {
        if output_buffer_length.is_some_and(|x| x < FileQuotaInformation::MIN_SIZE) {
            return Err(Error::BufferTooSmall {
                data_type: "FileQuotaInformation",
                required: FileQuotaInformation::MIN_SIZE.into(),
                provided: output_buffer_length.unwrap(),
            });
        }

        Ok(self
            .handle
            .query_common(
                QueryInfoRequest {
                    info_type: InfoType::Quota,
                    info_class: Default::default(),
                    output_buffer_length: 0,
                    additional_info: AdditionalInfo::new(),
                    flags: QueryInfoFlags::new()
                        .with_restart_scan(info.restart_scan.into())
                        .with_return_single_entry(info.return_single.into()),
                    file_id: self.handle.file_id()?,
                    data: GetInfoRequestData::Quota(info),
                },
                output_buffer_length,
                std::any::type_name::<FileQuotaInformation>(),
            )
            .await?
            .as_quota()?
            .into())
    }

    /// Sets the quota information for the current file.
    /// # Arguments
    /// * `info` - The information to set - a vector of [`FileQuotaInformation`].
    pub async fn set_quota_info(&self, info: Vec<FileQuotaInformation>) -> crate::Result<()> {
        let info = ChainedItemList::from(info);
        self.handle
            .set_info_common(
                info,
                SetInfoClass::Quota(Default::default()),
                Default::default(),
            )
            .await
    }
}

/// Single result from a directory watch operation.
///
/// Implements `From<DirectoryWatchResult>` to convert into `Result<Vec<FileNotifyInformation>>`.
/// Note that all states except `Notifications` are converted into errors.
pub enum DirectoryWatchResult {
    /// A vector of file change notifications.
    Notifications(Vec<FileNotifyInformation>),

    /// The specified buffer size cannot contain the results.
    NotifyEnumDir { provided_size: usize },

    /// The watch was cleaned up by the server.
    ///
    /// This is usually due to file being closed, while watch is still active.
    Cleanup,

    /// The watch was cancelled by the user.
    Cancelled,

    /// An error occurred while watching the directory.
    Error(crate::Error),
}

impl From<DirectoryWatchResult> for crate::Result<Vec<FileNotifyInformation>> {
    fn from(val: DirectoryWatchResult) -> Self {
        match val {
            DirectoryWatchResult::Notifications(v) => Ok(v),
            DirectoryWatchResult::Cancelled => Err(Error::Cancelled("watch cancelled")),
            DirectoryWatchResult::Cleanup => Err(Error::Cancelled("watch cleaned up by server")),
            DirectoryWatchResult::Error(e) => Err(e),
            DirectoryWatchResult::NotifyEnumDir { provided_size } => Err(Error::BufferTooSmall {
                data_type: "FileNotifyInformation",
                required: None,
                provided: provided_size,
            }),
        }
    }
}

impl Deref for Directory {
    type Target = ResourceHandle;

    fn deref(&self) -> &Self::Target {
        &self.handle
    }
}

impl DerefMut for Directory {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.handle
    }
}

#[cfg(feature = "async")]
pub mod iter_stream {
    use super::*;
    use futures_core::Stream;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    /// A stream that allows you to iterate over the contents of a directory.
    /// See [Directory::query] for more information on how to use it.
    pub struct QueryDirectoryStream<'a, T> {
        /// A channel to receive the results from the query.
        /// This is used to send the results from the query loop to the stream.
        receiver: tokio::sync::mpsc::Receiver<crate::Result<T>>,
        /// This is used to wake up the query (against the server) loop when more data is required,
        /// since the iterator is lazy and will not fetch data until it is needed.
        notify_fetch_next: Arc<tokio::sync::Notify>,
        /// Holds the lock while iterating the directory,
        /// to prevent multiple queries at the same time.
        /// See [Directory::query] for more information.
        _lock_guard: MutexGuard<'a, ()>,
    }

    impl<'a, T> QueryDirectoryStream<'a, T>
    where
        T: QueryDirectoryInfoValue + for<'b> binrw::prelude::BinWrite<Args<'b> = ()> + Send,
    {
        pub async fn new(
            directory: &'a Arc<Directory>,
            pattern: String,
            buffer_size: u32,
        ) -> crate::Result<Self> {
            let (sender, receiver) = tokio::sync::mpsc::channel(1024);
            let notify_fetch_next = Arc::new(tokio::sync::Notify::new());
            {
                let notify_fetch_next = notify_fetch_next.clone();
                let directory = directory.clone();
                tokio::spawn(async move {
                    Self::fetch_loop(
                        directory,
                        pattern,
                        buffer_size,
                        sender,
                        notify_fetch_next.clone(),
                    )
                    .await;
                });
            }
            let guard = directory.query_lock.lock().await?;
            Ok(Self {
                receiver,
                notify_fetch_next,
                _lock_guard: guard,
            })
        }

        async fn fetch_loop(
            directory: Arc<Directory>,
            pattern: String,
            buffer_size: u32,
            sender: mpsc::Sender<crate::Result<T>>,
            notify_fetch_next: Arc<tokio::sync::Notify>,
        ) {
            let mut is_first = true;
            loop {
                let result = directory
                    .send_query::<T>(&pattern, is_first, buffer_size)
                    .await;
                is_first = false;

                match result {
                    Ok(items) => {
                        if items.is_empty() {
                            // No more files, exit the loop
                            break;
                        }
                        for item in items {
                            if sender.send(Ok(item)).await.is_err() {
                                return; // Receiver dropped
                            }
                        }
                    }
                    Err(e) => {
                        if sender.send(Err(e)).await.is_err() {
                            return; // Receiver dropped
                        }
                    }
                }

                // Notify the stream that a new batch is available
                notify_fetch_next.notify_waiters();
                notify_fetch_next.notified().await;
            }
        }
    }

    impl<'a, T> Stream for QueryDirectoryStream<'a, T>
    where
        T: QueryDirectoryInfoValue + Unpin + Send,
    {
        type Item = crate::Result<T>;

        fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            let this = self.get_mut();
            match this.receiver.poll_recv(cx) {
                Poll::Ready(Some(value)) => {
                    if this.receiver.is_empty() {
                        this.notify_fetch_next.notify_waiters() // Notify that batch is done
                    }
                    Poll::Ready(Some(value))
                }
                Poll::Ready(None) => Poll::Ready(None), // Stream is closed!
                Poll::Pending => Poll::Pending,
            }
        }
    }
}

#[cfg(not(feature = "async"))]
pub mod iter_sync {

    use super::*;
    pub struct QueryDirectoryIterator<'a, T>
    where
        T: QueryDirectoryInfoValue,
    {
        /// Results from last call to [`Directory::send_query`], that were not yet consumed.
        backlog: Vec<T>,
        /// The directory to query.
        directory: &'a Directory,
        /// The pattern to match against the file names in the directory.
        pattern: String,
        /// Whether this is the first query or not.
        is_first: bool,
        /// The buffer size to use for each query.
        buffer_size: u32,

        /// The lock being held while iterating the directory.
        _iter_lock_guard: MutexGuard<'a, ()>,
    }

    impl<'a, T> QueryDirectoryIterator<'a, T>
    where
        T: QueryDirectoryInfoValue,
    {
        pub fn new(
            directory: &'a Directory,
            pattern: String,
            buffer_size: u32,
        ) -> crate::Result<Self> {
            Ok(Self {
                backlog: Vec::new(),
                directory,
                pattern,
                is_first: true,
                buffer_size,
                _iter_lock_guard: directory.query_lock.lock()?,
            })
        }
    }

    impl<'a, T> Iterator for QueryDirectoryIterator<'a, T>
    where
        T: QueryDirectoryInfoValue + for<'b> binrw::prelude::BinWrite<Args<'b> = ()>,
    {
        type Item = crate::Result<T>;

        fn next(&mut self) -> Option<Self::Item> {
            // Pop from backlog if we have any results left.
            if !self.backlog.is_empty() {
                return Some(Ok(self.backlog.remove(0)));
            }

            // If we have no backlog, we need to query the directory again.
            let query_result =
                self.directory
                    .send_query::<T>(&self.pattern, self.is_first, self.buffer_size);
            self.is_first = false;
            match query_result {
                Ok(next_backlog) => {
                    if next_backlog.is_empty() {
                        // No more items
                        None
                    } else {
                        // Store the items in the backlog and return the first one.
                        self.backlog = next_backlog;
                        self.next()
                    }
                }
                Err(e) => {
                    // Another error occurred, return it.
                    Some(Err(e))
                }
            }
        }
    }
}

#[cfg(feature = "multi_threaded")]
mod iter_mtd {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    use crate::msg_handler::AsyncMessageIds;
    /// A helper structure that allows cancelling a [`NotifyDirectoryIterator`].
    #[derive(Clone)]
    pub struct NotifyDirectoryIteratorCanceller {
        /// Dual-purpose flag:
        /// 1. Indicates to the worker thread that it should stop (by setting to `true`).
        /// 2. Used by the worker thread to indicate that a cancellation was requested
        ///    (by setting to `true` when it sends a cancel request) - in the async processing itself (via `ReceiveOptions`).
        pub(crate) cancel_flag: Arc<AtomicBool>,
        pub(crate) directory: Arc<Directory>,
        pub(crate) async_msg_ids: Arc<AsyncMessageIds>,

        cancel_event: Arc<std::sync::Condvar>,
        cancel_done: Arc<Mutex<bool>>,
    }

    impl NotifyDirectoryIteratorCanceller {
        pub(crate) fn new(directory: &Arc<Directory>) -> Self {
            Self {
                cancel_flag: Default::default(),
                directory: directory.clone(),
                async_msg_ids: Default::default(),
                cancel_event: Default::default(),
                cancel_done: Default::default(),
            }
        }

        /// Cancels the ongoing watch operation.
        ///
        /// This method is non-blocking: it launches a thread to send the cancel request,
        /// and returns immediately. The actual cancellation may take some time to complete,
        /// depending on network conditions and server responsiveness.
        /// Use [`wait_cancelled`] to block until the cancellation is confirmed.
        pub fn cancel(&self) {
            self.cancel_flag.store(true, Ordering::SeqCst);

            // Dropping the iterator requests cancellation of the async operation.
            // the cancel response should arrive to the worker as well.
            // * if during the wait, we already have the common async_msg_ids set - use it to send cancel request.
            // * if before sending next request, the flag is checked.
            let directory = self.directory.clone();
            let async_msg_ids = self.async_msg_ids.clone();
            std::thread::spawn(move || {
                directory
                    .send_cancel(&async_msg_ids)
                    .map_err(|e| {
                        log::error!("Error sending cancel request: {e}");
                        e
                    })
                    .ok()
            });
        }

        /// Blocks the current thread until the cancellation is confirmed.
        pub fn wait_cancelled(
            &self,
        ) -> std::result::Result<(), std::sync::PoisonError<std::sync::MutexGuard<'_, bool>>>
        {
            let _lockguard = self
                .cancel_event
                .wait_while(self.cancel_done.lock()?, |done| !*done)?;
            Ok(())
        }

        pub(crate) fn notify_cancelled(
            &self,
        ) -> std::result::Result<(), std::sync::PoisonError<std::sync::MutexGuard<'_, bool>>>
        {
            {
                let mut done = self.cancel_done.lock()?;
                if *done {
                    return Ok(()); // Already notified
                }
                *done = true;
            }
            self.cancel_event.notify_all();
            Ok(())
        }
    }

    pub trait NotifyDirectoryIteratorCancellable {
        fn get_canceller(&self) -> &NotifyDirectoryIteratorCanceller;
    }

    /// Iterator over directory change notifications.
    ///
    /// This is needed since cancellation of the async operation is complex on multi-threaded
    /// environments, and requires a dedicated worker thread to handle the async operation.
    pub(crate) struct NotifyDirectoryIterator {
        iterator: <std::sync::mpsc::Receiver<crate::Result<FileNotifyInformation>> as IntoIterator>::IntoIter,

        canceller: NotifyDirectoryIteratorCanceller,
    }

    impl NotifyDirectoryIterator {
        pub fn new(
            canceller: NotifyDirectoryIteratorCanceller,
            notify_filter: NotifyFilter,
            recursive: bool,
        ) -> crate::Result<Self> {
            let async_msg_ids = canceller.async_msg_ids.clone();
            let cancel_flag = canceller.cancel_flag.clone();

            let (tx, rx) = std::sync::mpsc::channel();

            // Launch the worker thread that will handle the async notifications.
            std::thread::spawn({
                let canceller = canceller.clone();
                let cancel_flag = cancel_flag.clone();
                let receive_options = ReceiveOptions::new()
                    .with_async_msg_ids(async_msg_ids.clone())
                    .with_cancellation_flag(cancel_flag.clone())
                    .with_timeout(Duration::MAX);
                move || {
                    while !cancel_flag.load(Ordering::SeqCst) {
                        let watch_result = canceller.directory._watch_options(
                            notify_filter,
                            recursive,
                            receive_options.clone(),
                        );
                        receive_options.async_msg_ids.as_ref().unwrap().reset();

                        use DirectoryWatchResult::*;
                        match watch_result {
                            Notifications(notifications) => {
                                for notification in notifications {
                                    if tx.send(Ok(notification)).is_err() {
                                        break; // Receiver dropped
                                    }
                                }
                            }
                            Cancelled => {
                                // Cancelled by user, exit the loop.
                                canceller
                                    .notify_cancelled()
                                    .map_err(|e| {
                                        log::error!("Error notifying cancellation: {e}");
                                        e
                                    })
                                    .ok();
                                log::debug!("Watch cancelled by user");
                                break;
                            }
                            Cleanup => {
                                // Server cleaned up the watch, exit the loop.
                                log::debug!("Watch cleaned up by server");
                                tx.send(Err(crate::Error::Cancelled("watch cleaned up by server")))
                                    .ok();
                                break;
                            }
                            x => {
                                let x: crate::Result<_> = x.into();
                                let x = x.unwrap_err();
                                log::debug!("Error watching directory: {x}");
                                tx.send(Err(x)).ok();
                                break; // Exit on error
                            }
                        }
                    }
                }
            });

            let iterator = rx.into_iter();

            Ok(Self {
                iterator,
                canceller,
            })
        }
    }

    impl Iterator for NotifyDirectoryIterator {
        type Item = crate::Result<FileNotifyInformation>;

        fn next(&mut self) -> Option<Self::Item> {
            self.iterator.next()
        }
    }

    impl NotifyDirectoryIteratorCancellable for NotifyDirectoryIterator {
        fn get_canceller(&self) -> &NotifyDirectoryIteratorCanceller {
            &self.canceller
        }
    }

    impl Drop for NotifyDirectoryIterator {
        fn drop(&mut self) {
            self.canceller.cancel();
        }
    }
}

#[cfg(feature = "multi_threaded")]
pub use iter_mtd::{NotifyDirectoryIteratorCancellable, NotifyDirectoryIteratorCanceller};
