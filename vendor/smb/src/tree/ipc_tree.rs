use crate::Error;

use super::Tree;
use maybe_async::maybe_async;
use smb_msg::{NetworkInterfaceInfo, QueryNetworkInterfaceInfoRequest};

pub struct IpcTreeRef<'a> {
    tree: &'a Tree,
}

impl<'a> IpcTreeRef<'a> {
    /// Creates a new [`IpcTreeRef`] instance,
    /// wrapping the provided [`Tree`].
    pub(crate) fn new(tree: &'a Tree) -> crate::Result<Self> {
        Ok(Self { tree })
    }

    #[maybe_async]
    pub async fn query_network_interfaces(&self) -> crate::Result<Vec<NetworkInterfaceInfo>> {
        if !self.tree.conn_info.config.multichannel.is_enabled() {
            // Server might decline + this is irrelevant!
            return Err(Error::InvalidState(
                "Network interfaces can only be queried when multi-channel is enabled".to_string(),
            ));
        }

        const QUERY_NETWORK_INTERFACE_MAX_OUTPUT: u32 = 2u32.pow(16);
        let interface_info = self
            .tree
            .fsctl_with_options(
                QueryNetworkInterfaceInfoRequest(()),
                QUERY_NETWORK_INTERFACE_MAX_OUTPUT,
            )
            .await?;

        Ok(interface_info.into())
    }
}
