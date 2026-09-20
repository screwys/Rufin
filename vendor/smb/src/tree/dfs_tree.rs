use std::ops::Deref;

use crate::msg_handler::{MessageHandler, ReceiveOptions};
use maybe_async::*;
use smb_msg::{FileId, FsctlCodes, IoctlReqData, IoctlRequest, IoctlRequestFlags, dfsc::*};

use super::Tree;

/// A wrapper around the [`Tree`] struct that provides a DFS root functions.
///
/// The struct implements `Deref` to allow access to the underlying [`Tree`] methods.
pub struct DfsRootTreeRef<'a> {
    tree: &'a Tree,
}

impl<'a> DfsRootTreeRef<'a> {
    /// Creates a new [`DfsRootTree`] instance,
    /// wrapping the provided [`Tree`].
    pub(crate) fn new(tree: &'a Tree) -> Self {
        Self { tree }
    }

    /// Performs a DFS referral request to the server.
    /// This is used to get the referral information for a given path.
    ///
    /// See [MS-DFSC](<https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-dfsc/04657125-a7d5-4c62-9bec-85af601fa14c>) for more information.
    #[maybe_async]
    pub async fn dfs_get_referrals(&self, path: &str) -> crate::Result<RespGetDfsReferral> {
        let res = self
            .handler
            .send_recvo(
                IoctlRequest {
                    ctl_code: FsctlCodes::DfsGetReferrals as u32,
                    file_id: FileId::FULL,
                    max_input_response: 1024,
                    max_output_response: 1024,
                    flags: IoctlRequestFlags::new().with_is_fsctl(true),
                    buffer: IoctlReqData::FsctlDfsGetReferrals(ReqGetDfsReferral {
                        max_referral_level: ReferralLevel::V4,
                        request_file_name: path.into(),
                    }),
                }
                .into(),
                ReceiveOptions::new().with_allow_async(true),
            )
            .await?;
        let res = res
            .message
            .content
            .to_ioctl()?
            .parse_fsctl::<RespGetDfsReferral>()?;
        Ok(res)
    }
}

impl<'a> Deref for DfsRootTreeRef<'a> {
    type Target = Tree;

    fn deref(&self) -> &Self::Target {
        self.tree
    }
}
