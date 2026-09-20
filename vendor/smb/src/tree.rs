use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use maybe_async::*;
use smb_msg::{FileId, FsctlRequest, IoctlRequest, IoctlRequestFlags};

use crate::FileCreateArgs;
use crate::connection::connection_info::ConnectionInfo;
use smb_fscc::{FileAccessMask, FileAttributes};
use smb_msg::{
    CreateOptions, RequestContent, ShareFlags, ShareType,
    create::CreateDisposition,
    tree_connect::{TreeConnectRequest, TreeDisconnectRequest},
};

use crate::{
    Error, Resource,
    msg_handler::{HandlerReference, MessageHandler},
    session::SessionMessageHandler,
};
mod dfs_tree;
mod ipc_tree;
use crate::msg_handler::OutgoingMessage;
pub use dfs_tree::*;
pub use ipc_tree::*;

type Upstream = HandlerReference<SessionMessageHandler>;

#[derive(Debug, Clone)]
pub struct TreeConnectInfo {
    share_type: ShareType,
    share_flags: ShareFlags,
}

/// Represents an SMB share.
///
/// A Tree is the SMB protocol's representation of a connected share on the server.
pub struct Tree {
    handler: HandlerReference<TreeMessageHandler>,
    conn_info: Arc<ConnectionInfo>,
}

#[maybe_async(AFIT)]
impl Tree {
    #[maybe_async]
    pub(crate) async fn connect(
        name: &str,
        upstream: &Upstream,
        conn_info: &Arc<ConnectionInfo>,
    ) -> crate::Result<Tree> {
        // send and receive tree request & response.
        let response = upstream
            .send_recv(TreeConnectRequest::new(name).into())
            .await?;

        let content = response.message.content.to_treeconnect()?;

        // Make sure the share flags from the server are valid to the dialect.
        if ((!u32::from_le_bytes(conn_info.dialect.get_tree_connect_caps_mask().into_bytes()))
            & u32::from_le_bytes(content.capabilities.into_bytes()))
            != 0
        {
            return Err(Error::InvalidMessage(format!(
                "Invalid share flags received from server for tree '{}': {:?}",
                name, content.share_flags
            )));
        }

        // Same for share flags
        if ((!u32::from_le_bytes(conn_info.dialect.get_share_flags_mask().into_bytes()))
            & u32::from_le_bytes(content.share_flags.into_bytes()))
            != 0
        {
            return Err(Error::InvalidMessage(format!(
                "Invalid capabilities received from server for tree '{}': {:?}",
                name, content.capabilities
            )));
        }

        // If encryption is required, make sure it is available.
        if content.share_flags.encrypt_data() && conn_info.config.encryption_mode.is_disabled() {
            return Err(Error::InvalidMessage(
                "Server requires encryption, but client does not support it".to_string(),
            ));
        }

        let tree_id = response
            .message
            .header
            .tree_id
            .ok_or(Error::InvalidMessage(
                "Tree ID is not set in the response".to_string(),
            ))?;

        log::info!("Connected to tree {name} (#{tree_id})");

        let tree_connect_info = TreeConnectInfo {
            share_type: content.share_type,
            share_flags: content.share_flags,
        };

        let t = Tree {
            handler: TreeMessageHandler::new(
                upstream,
                tree_id,
                name.to_string(),
                tree_connect_info,
            ),
            conn_info: conn_info.clone(),
        };

        Ok(t)
    }

    /// Creates a resource (file, directory, pipe, or printer) on the remote server by it's name.
    /// See [Tree::create_file] and [Tree::create_directory] for an easier API.
    /// # Arguments
    /// * `file_name` - The name of the resource to create. This should NOT contain the share name, or begin with a backslash.
    /// * `args` - The arguments for the create operation. This includes the desired access, file attributes, and create options.
    ///     See [`FileCreateArgs`] for more information.
    /// # Returns
    /// * A [Resource] object representing the created resource. This can be a file, directory, pipe, or printer.
    /// # Notes
    /// This function automatically handles the following:
    /// * *DFS operations*: If the share has been opened as a DFS referral share, the create operation will modify the file name to include the DFS path.
    ///     That is, assuming it is NOT prefixed with "\\". This is rquired for a proper DFS referral file open. ("DFS normalization", MS-SMB2 2.2.13 + 3.3.5.9)
    pub async fn create(&self, file_name: &str, args: &FileCreateArgs) -> crate::Result<Resource> {
        let info = self.handler.info()?;
        Resource::create(
            file_name,
            &self.handler,
            args,
            &self.conn_info,
            info.share_type,
            info.share_flags.dfs(),
        )
        .await
    }

    /// A wrapper around [Tree::create] that creates a file on the remote server.
    /// See [Tree::create] for more information.
    pub async fn create_file(
        &self,
        file_name: &str,
        disposition: CreateDisposition,
        desired_access: FileAccessMask,
    ) -> crate::Result<Resource> {
        self.create(
            file_name,
            &FileCreateArgs {
                disposition,
                options: CreateOptions::new(),
                desired_access,
                attributes: FileAttributes::new(),
            },
        )
        .await
    }

    /// A wrapper around [Tree::create] that creates a directory on the remote server.
    /// See [Tree::create] for more information.
    pub async fn create_directory(
        &self,
        dir_name: &str,
        disposition: CreateDisposition,
        desired_access: FileAccessMask,
    ) -> crate::Result<Resource> {
        self.create(
            dir_name,
            &FileCreateArgs {
                disposition,
                options: CreateOptions::new().with_directory_file(true),
                desired_access,
                attributes: FileAttributes::new().with_directory(true),
            },
        )
        .await
    }

    /// A wrapper around [create][crate::tree::Tree::create] that opens an existing file or directory on the remote server.
    /// See [create][crate::tree::Tree::create] for more information.
    pub async fn open_existing(
        &self,
        file_name: &str,
        access: FileAccessMask,
    ) -> crate::Result<Resource> {
        self.create(file_name, &FileCreateArgs::make_open_existing(access))
            .await
    }

    pub fn is_dfs_root(&self) -> crate::Result<bool> {
        let info = self.handler.info()?;
        Ok(info.share_flags.dfs_root() && info.share_flags.dfs())
    }

    pub fn as_dfs_tree(&self) -> crate::Result<DfsRootTreeRef<'_>> {
        if !self.is_dfs_root()? {
            return Err(Error::InvalidState("Tree is not a DFS tree".to_string()));
        }
        Ok(DfsRootTreeRef::new(self))
    }

    /// Returns a DFS tree reference without checking if the tree is a DFS root.
    ///
    /// This is useful for querying DFS referrals on trees that may not be DFS roots.
    /// An example of this is when querying IPC$ shares on DFS namespaces, as they are not DFS roots but may still support DFS referrals.
    ///
    /// Use with caution, as calling DFS-specific methods on non-DFS trees may lead to errors.
    pub fn as_dfs_tree_unchecked(&self) -> DfsRootTreeRef<'_> {
        DfsRootTreeRef::new(self)
    }

    pub fn as_ipc_tree(&self) -> crate::Result<IpcTreeRef<'_>> {
        let info = self.handler.info()?;
        if info.share_type != ShareType::Pipe {
            return Err(Error::InvalidState(format!(
                "Tree is not IPC tree ({:?})",
                info.share_type
            )));
        }

        IpcTreeRef::new(self)
    }

    /// Disconnects from the tree (share) on the server.
    ///
    /// After calling this method, none of the resources held open by the tree are accessible.
    pub async fn disconnect(&self) -> crate::Result<()> {
        self.handler.disconnect().await?;
        Ok(())
    }

    // TODO: Make it common with ResourceHandle::fsctl_with_options
    #[maybe_async]
    pub(crate) async fn fsctl_with_options<T: FsctlRequest>(
        &self,
        request: T,
        max_output_response: u32,
    ) -> crate::Result<T::Response> {
        const NO_INPUT_IN_RESPONSE: u32 = 0;
        let response = self
            .handler
            .send_recv(RequestContent::Ioctl(IoctlRequest {
                ctl_code: T::FSCTL_CODE as u32,
                file_id: FileId::FULL,
                max_input_response: NO_INPUT_IN_RESPONSE,
                max_output_response,
                flags: IoctlRequestFlags::new().with_is_fsctl(true),
                buffer: request.into(),
            }))
            .await?
            .message
            .content
            .to_ioctl()?
            .parse_fsctl::<T::Response>()?;
        Ok(response)
    }
}

pub(crate) struct TreeMessageHandler {
    tree_id: AtomicU32,

    upstream: Upstream,

    tree_name: String,
    info: TreeConnectInfo,
}

impl TreeMessageHandler {
    const INVALID_TREE_ID: u32 = u32::MAX;

    pub fn new(
        upstream: &Upstream,
        tree_id: u32,
        tree_name: String,
        info: TreeConnectInfo,
    ) -> HandlerReference<TreeMessageHandler> {
        HandlerReference::new(TreeMessageHandler {
            tree_id: AtomicU32::new(tree_id),
            upstream: upstream.clone(),
            info,
            tree_name,
        })
    }

    #[maybe_async]
    async fn _disconnect(upstream: Upstream, tree_id: u32, encrypt: bool) -> crate::Result<()> {
        // send and receive tree disconnect request & response.
        let request_content: RequestContent = TreeDisconnectRequest::default().into();
        let mut message = OutgoingMessage::new(request_content).with_encrypt(encrypt);
        message.message.header.tree_id = Some(tree_id);

        let _response = upstream.sendo_recv(message).await?;

        Ok(())
    }

    #[maybe_async]
    async fn disconnect(&self) -> crate::Result<()> {
        let tree_id = self.tree_id.swap(Self::INVALID_TREE_ID, Ordering::SeqCst);
        if tree_id == Self::INVALID_TREE_ID {
            // Already disconnected
            return Ok(());
        }
        let encrypt = self.info.share_flags.encrypt_data();
        Self::_disconnect(self.upstream.clone(), tree_id, encrypt).await
    }

    pub fn info(&self) -> crate::Result<&TreeConnectInfo> {
        if self.tree_id.load(Ordering::Relaxed) == Self::INVALID_TREE_ID {
            return Err(Error::InvalidState("Tree is closed".to_string()));
        }

        Ok(&self.info)
    }
}

impl MessageHandler for TreeMessageHandler {
    #[maybe_async]
    async fn sendo(
        &self,
        mut msg: crate::msg_handler::OutgoingMessage,
    ) -> crate::Result<crate::msg_handler::SendMessageResult> {
        if !msg.message.header.flags.async_command() {
            msg.message.header.tree_id = self.tree_id.load(Ordering::SeqCst).into();
            if self.info.share_flags.encrypt_data() {
                msg.encrypt = true;
            }
        }

        self.upstream.sendo(msg).await
    }

    #[maybe_async]
    async fn recvo(
        &self,
        options: crate::msg_handler::ReceiveOptions<'_>,
    ) -> crate::Result<crate::msg_handler::IncomingMessage> {
        let msg = self.upstream.recvo(options).await?;

        if !msg.message.header.flags.async_command()
            && msg.message.header.tree_id.unwrap() != self.tree_id.load(Ordering::SeqCst)
        {
            return Err(Error::InvalidMessage(
                "Received message for different tree, or tree disconnecting.".to_string(),
            ));
        }

        // Make sure encryption is enforced if the share requires it.
        if !msg.form.encrypted && self.info()?.share_flags.encrypt_data() {
            return Err(Error::InvalidMessage(
                "Received unencrypted message on encrypted share".to_string(),
            ));
        }

        Ok(msg)
    }
}

#[cfg(not(feature = "async"))]
impl Drop for TreeMessageHandler {
    fn drop(&mut self) {
        self.disconnect()
            .map_err(|e| {
                log::error!("Failed to disconnect from tree {}: {e}", self.tree_name);
                e
            })
            .ok();
    }
}

#[cfg(feature = "async")]
impl Drop for TreeMessageHandler {
    fn drop(&mut self) {
        let tree_id = self.tree_id.load(Ordering::SeqCst);
        if tree_id == Self::INVALID_TREE_ID {
            // Already dropped
            return;
        }

        let upstream = self.upstream.clone();
        let tree_name = self.tree_name.clone();
        let encrypt = self.info.share_flags.encrypt_data();
        tokio::task::spawn(async move {
            Self::_disconnect(upstream, tree_id, encrypt)
                .await
                .map_err(|e| {
                    log::error!("Failed to disconnect from tree {}: {e}", tree_name);
                })
                .ok();
        });
    }
}
