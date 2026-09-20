use super::file_util::*;
use super::*;
#[cfg(not(feature = "async"))]
use std::io::prelude::*;
use std::ops::{Deref, DerefMut};

/// An opened file on the server.
///
/// # [std::io] Support
/// The [File] struct also supports the [Read][std::io::Read] and [Write][std::io::Write] traits.
/// Note that both of these traits are blocking, and will block the current thread until the operation is complete.
/// Use [File::read_block] and [File::write_block] for non-blocking operations.
/// The [File] struct also implements the [Seek][std::io::Seek] trait.
/// This allows you to seek to a specific position in the file, combined with the [Read][std::io::Read] and [Write][std::io::Write] traits.
/// Using any of the implemented [std::io] traits mentioned above should have no effect on calling the other, non-blocking methods.
/// Since we would NOT like to call a tokio task from a blocking context, these traits are **NOT** implemented in the async context!
///
/// You may not directly create this struct. Instead, use the [Tree::create][crate::tree::Tree::create] method to gain
/// a proper handle against the server in the shape of a [Resource], that can be then converted to a [File].
pub struct File {
    handle: ResourceHandle,

    #[cfg(not(feature = "async"))]
    pos: u64,
    #[cfg(not(feature = "async"))]
    dirty: bool,

    end_of_file: u64,
}

#[maybe_async(AFIT)]
impl File {
    pub fn new(handle: ResourceHandle, end_of_file: u64) -> Self {
        File {
            handle,
            end_of_file,
            #[cfg(not(feature = "async"))]
            pos: 0,
            #[cfg(not(feature = "async"))]
            dirty: false,
        }
    }

    /// Returns the access mask of the file,
    /// when the file was opened.
    pub fn access(&self) -> FileAccessMask {
        self.access
    }

    /// Read a block of data from an opened file.
    /// # Arguments
    /// * `buf` - The buffer to read the data into. A maximum of `buf.len()` bytes will be read.
    /// * `pos` - The offset in the file to read from.
    /// * `unbuffered` - Whether to try using unbuffered I/O (if supported by the server).
    /// # Returns
    /// The number of bytes read, up to `buf.len()`.
    pub async fn read_block(
        &self,
        buf: &mut [u8],
        pos: u64,
        channel: Option<u32>,
        unbuffered: bool,
    ) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        if !self.access.file_read_data() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "No read permission",
            ));
        }

        // EOF
        if pos >= self.end_of_file {
            return Ok(0);
        }

        log::debug!(
            "Reading up to {} bytes at offset {} from {}",
            buf.len(),
            pos,
            self.handle.name()
        );

        let mut flags = ReadFlags::new();
        if self.handle.conn_info.config.compression_enabled
            && self.handle.conn_info.dialect.supports_compression()
        {
            flags.set_read_compressed(true);
        }

        if unbuffered && self.handle.conn_info.negotiation.dialect_rev >= Dialect::Smb0302 {
            flags.set_read_unbuffered(true);
        }

        let request = OutgoingMessage::new(
            ReadRequest {
                flags,
                length: buf.len() as u32,
                offset: pos,
                file_id: self.handle.file_id().map_err(std::io::Error::other)?,
                minimum_count: 1,
            }
            .into(),
        )
        .with_channel_id(channel);

        let response = self
            .handle
            .sendo_recvo(request, ReceiveOptions::new().with_allow_async(true))
            .await
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        let content = response
            .message
            .content
            .to_read()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let actual_read_length = content.buffer.len();
        log::debug!(
            "Read {} bytes from {}.",
            actual_read_length,
            self.handle.name()
        );

        buf[..actual_read_length].copy_from_slice(&content.buffer);

        Ok(actual_read_length)
    }

    /// Write a block of data to an opened file.
    /// # Arguments
    /// * `buf` - The data to write.
    /// * `pos` - The offset in the file to write to.
    /// # Returns
    /// The number of bytes written.
    /// # Note
    /// this method copies the data from `buf` into an internal buffer,
    /// which is then sent to the server.
    /// If you want to avoid this copy, use [`File::write_block_zc`] instead.
    #[maybe_async]
    #[inline]
    pub async fn write_block(
        &self,
        buf: &[u8],
        pos: u64,
        channel: Option<u32>,
    ) -> std::io::Result<usize> {
        self.write_block_zc(buf.into(), pos, channel).await
    }

    /// Write a block of data to an opened file, without copying the data.
    /// # Arguments
    /// * `buf` - The data to write.
    /// * `pos` - The offset in the file to write to.
    /// # Returns
    /// The number of bytes written.
    pub async fn write_block_zc(
        &self,
        buf: Arc<[u8]>,
        pos: u64,
        channel: Option<u32>,
    ) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        if !self.access.file_write_data() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "No write permission",
            ));
        }

        log::debug!(
            "Writing {} bytes at offset {} to {}",
            buf.len(),
            pos,
            self.handle.name()
        );

        // Arc is accepted to provide safety regarding the buffer's lifetime,
        // without forcing an actual copy of the data.
        let outgoing = OutgoingMessage::new(
            WriteRequest::new(
                pos,
                self.handle.file_id().map_err(std::io::Error::other)?,
                WriteFlags::new(),
                buf.len() as u32,
            )
            .into(),
        )
        .with_additional_data(Arc::clone(&buf))
        .with_channel_id(channel);

        let response = self
            .handle
            .sendo_recvo(outgoing, ReceiveOptions::new().with_allow_async(true))
            .await
            .map_err(|e| std::io::Error::other(e.to_string()))?;

        let content = response
            .message
            .content
            .to_write()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let actual_written_length = content.count as usize;
        log::debug!(
            "Wrote {} bytes to {}.",
            actual_written_length,
            self.handle.name()
        );
        Ok(actual_written_length)
    }

    /// Sends a flush request to the server to flush the file.
    pub async fn flush(&self) -> std::io::Result<()> {
        let _response = self
            .handle
            .send_recvo(
                FlushRequest {
                    file_id: self.handle.file_id().map_err(std::io::Error::other)?,
                }
                .into(),
                ReceiveOptions::new().with_allow_async(true),
            )
            .await
            .map_err(|e| std::io::Error::other(e.to_string()))?;

        log::debug!("Flushed {}.", self.handle.name());
        Ok(())
    }

    /// Performs a server-side copy from another file on the same server.
    /// # Arguments
    /// * `from` - The file to copy from.
    /// # Notes
    /// * This copy must be performed against a file from the same share (tree) as this file.
    pub async fn srv_copy(&self, from: &File) -> crate::Result<()> {
        if !self.access.file_write_data() {
            return Err(Error::InvalidState(
                "No write permission on destination file".to_string(),
            ));
        }
        if !from.access.file_read_data() {
            return Err(Error::InvalidState(
                "No read permission on source file".to_string(),
            ));
        }

        // Even if we weren't testing it properly, the remote would have returned
        // [Status::ObjectNameNotFound] error for unmatching trees.
        if !self.same_tree(from) {
            return Err(Error::InvalidArgument(
                "Source and destination files must be opened from the same share (tree)"
                    .to_string(),
            ));
        }

        let other_end_of_file = from.get_len().await?;
        self.set_len(other_end_of_file).await?;

        let resume_key_response = from.fsctl(SrvRequestResumeKeyRequest(())).await?;
        let resume_key = resume_key_response.resume_key;

        let chunks = (0..other_end_of_file)
            .step_by(CHUNK_SIZE)
            .map(|start| {
                let len_left = other_end_of_file - start;
                SrvCopychunkItem {
                    source_offset: start,
                    target_offset: start,
                    length: std::cmp::min(CHUNK_SIZE as u32, len_left as u32),
                }
            })
            .collect::<Vec<_>>();

        const CHUNK_SIZE: usize = 1024 * 1024; // 1 MB
        let req = SrvCopychunkCopy {
            source_key: resume_key,
            chunks,
        };
        let copy_response = self.fsctl(req).await?;
        if copy_response.total_bytes_written as u64 != other_end_of_file {
            return Err(Error::InvalidArgument(format!(
                "Expected to write {} bytes, but wrote {} bytes",
                other_end_of_file, copy_response.total_bytes_written
            )));
        }
        Ok(())
    }
}

// Despite being available, seeking means nothing here,
// since it may only be used when calling read/write from the std::io traits.
#[cfg(not(feature = "async"))]
impl Seek for File {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        let next_pos = match pos {
            std::io::SeekFrom::Start(pos) => pos,
            std::io::SeekFrom::End(pos) => {
                let pos = self.end_of_file as i64 + pos;
                if pos < 0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "Invalid seek position",
                    ));
                }
                pos.try_into().map_err(|_| {
                    std::io::Error::new(std::io::ErrorKind::InvalidInput, "Invalid seek position")
                })?
            }
            std::io::SeekFrom::Current(pos) => {
                let pos = self.pos as i64 + pos;
                if pos < 0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "Invalid seek position",
                    ));
                }
                pos.try_into().map_err(|_| {
                    std::io::Error::new(std::io::ErrorKind::InvalidInput, "Invalid seek position")
                })?
            }
        };
        if next_pos > self.end_of_file {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Invalid seek position",
            ));
        }
        Ok(self.pos)
    }
}

#[cfg(not(feature = "async"))]
impl Read for File {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let read_length = File::read_block(self, buf, self.pos, None, false)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        self.pos += read_length as u64;
        Ok(read_length)
    }
}

#[cfg(not(feature = "async"))]
impl Write for File {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let written_length = File::write_block(self, buf, self.pos, None)?;
        self.pos += written_length as u64;
        self.dirty = true;
        Ok(written_length)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if !self.dirty {
            return Ok(());
        }
        File::flush(self)
    }
}

impl ReadAtChannel for File {
    #[maybe_async]
    async fn read_at_channel(
        &self,
        buf: &mut [u8],
        offset: u64,
        channel: Option<u32>,
    ) -> crate::Result<usize> {
        self.read_block(buf, offset, channel, false)
            .await
            .map_err(crate::Error::IoError)
    }
}

impl WriteAtChannel for File {
    #[maybe_async]
    async fn write_at_channel(
        &self,
        buf: &[u8],
        offset: u64,
        channel: Option<u32>,
    ) -> crate::Result<usize> {
        self.write_block(buf, offset, channel)
            .await
            .map_err(crate::Error::IoError)
    }
}

impl GetLen for File {
    #[maybe_async]
    async fn get_len(&self) -> crate::Result<u64> {
        Ok(self.end_of_file)
    }
}

impl SetLen for File {
    #[maybe_async]
    async fn set_len(&self, len: u64) -> crate::Result<()> {
        self.set_info(FileEndOfFileInformation { end_of_file: len })
            .await
    }
}

impl Deref for File {
    type Target = ResourceHandle;

    fn deref(&self) -> &Self::Target {
        &self.handle
    }
}

impl DerefMut for File {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.handle
    }
}
