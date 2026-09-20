use crate::ConnectionConfig;
use crate::{Connection, Error, FileCreateArgs, Pipe, Resource, Session, Tree, sync_helpers::*};
use maybe_async::maybe_async;
use smb_msg::{NetworkInterfaceInfo, ReferralEntry, ReferralEntryValue, Status};
use smb_rpc::interface::{ShareInfo1, SrvSvc};
use smb_transport::TransportConfig;
use smb_transport::utils::TransportUtils;
use sspi::{AuthIdentity, Secret};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::{collections::HashMap, str::FromStr};

use super::{config::ClientConfig, unc_path::UncPath};

/*
    Note:
    - Most of the operations here are not especially high-performance critical,
        especially the ones tied to creating connection/sessions/trees - those are limited and slow anyway.
        Therefore, the wide use of Mutex/RwLock is acceptable here, for code simplicity.
*/

/// This struct represents a high-level SMB client, and it is highly encouraged to use it
/// for interacting with SMB servers, instead of manually creating connections.
///
/// ## General usage
/// When connecting to a new share, even if it's on the same server,
/// you must always connect to the share using [`Client::share_connect`].
///
/// ## Drop behavior
/// When the client drops, the held connections are not forcibly closed, but rather
/// kept alive until all their references are dropped.
/// For example, if a file is opened from the client, and the client is dropped,
/// the connection, session and tree of the opened file, will still be alive, but you will not be able
/// to use the client to interact with them.
///
/// To force a closure of all connections and their managed resources,
/// use the [`Client::close`] method.
///
/// ## Example
///
/// ```no_run
/// use smb::{Client, ClientConfig, UncPath, FileCreateArgs, FileAccessMask};
/// use std::str::FromStr;
/// # #[cfg(not(feature = "async"))] fn main() {}
/// #[cfg(feature = "async")]
/// #[tokio::main]
/// async fn main() -> Result<(), Box<dyn std::error::Error>> {
///     // instantiate the client
///     let client = Client::new(ClientConfig::default());
///
///     // Connect to a share
///     let target_path = UncPath::from_str(r"\\server\share").unwrap();
///     client.share_connect(&target_path, "username", "password".to_string()).await?;
///
///     // And open a file on the server
///     let file_to_open = target_path.with_path("file.txt");
///     let file_open_args = FileCreateArgs::make_open_existing(FileAccessMask::new().with_generic_read(true));
///     let file = client.create_file(&file_to_open, &file_open_args).await?;
///     // now, you can do a bunch of operations against `file`, and close it at the end.
///     Ok(())
/// }
/// ```
pub struct Client {
    config: ClientConfig,
    /// Server Name + [RDMA|NONE] => [`ClientConnectionInfo`]
    // It's quite common to have one connection for RDMA, and one for TCP,
    connections: RwLock<HashMap<IpAddr, ClientConnectionInfo>>,
    /// shares (trees) that are currently connected.
    share_connects: Mutex<HashMap<UncPath, ClientConectedTree>>,
}

/// (Internal)
///
/// Holds information for a connection, held by the client.
/// This is most useful to avoid creating multiple connections to the same server,
struct ClientConnectionInfo {
    connection: Arc<Connection>,
    /// Sessions owned by the connection
    sessions: HashMap<u64, ClientSessionInfo>,
}

struct ClientSessionInfo {
    session: Arc<Session>,
    /// alternate channels established for this session
    session_alt_channels: Option<HashMap<u32, AltChannelInfo>>,
}

struct ClientConectedTree {
    session: Arc<Session>,
    tree: Arc<Tree>,
    credentials: Option<AuthIdentity>,
}

#[derive(Clone)]
pub struct AltChannelInfo {
    connection: Arc<Connection>,
}

#[maybe_async(AFIT)]
impl Client {
    /// Creates a new `Client` instance with the given configuration.
    pub fn new(config: ClientConfig) -> Self {
        Client {
            config,
            connections: Default::default(),
            share_connects: Default::default(),
        }
    }

    pub fn config(&self) -> &ClientConfig {
        &self.config
    }

    /// Shuts down the client, and all its managed connections.
    ///
    /// Any resource held by the client will not be accessible after calling this method,
    /// directly or indirectly.
    ///
    /// See [Drop behavior][Client#drop-behavior] for more information.
    pub async fn close(&self) -> crate::Result<()> {
        // Close all opened shares
        let mut trees = self.share_connects.lock().await?;
        for (_unc, connected_tree) in trees.iter() {
            connected_tree.tree.disconnect().await?;
        }
        trees.clear();

        let mut connections = self.connections.write().await?;
        // Close sessions
        for (_unc, conn) in connections.iter_mut() {
            for (_session_id, session) in conn.sessions.iter_mut() {
                // First close alternate channels
                if let Some(alt_channels) = &mut session.session_alt_channels {
                    for (_channel_id, alt_conn) in alt_channels.iter() {
                        alt_conn.connection.close().await.ok();
                    }
                    alt_channels.clear();
                }
                // Close primary session
                session.session.logoff().await.ok();
            }
        }
        // Close connections
        for (_ip, conn) in connections.iter() {
            conn.connection.close().await.ok();
        }
        connections.clear();

        Ok(())
    }

    /// Lists all shares on the specified server.
    pub async fn list_shares(&self, server: &str) -> crate::Result<Vec<ShareInfo1>> {
        let srvsvc_pipe_name: &str = "srvsvc";
        let srvsvc_pipe = self.open_pipe(server, srvsvc_pipe_name).await?;

        let mut srvsvc_pipe: SrvSvc<_> = srvsvc_pipe.bind().await?;
        let shares = srvsvc_pipe.netr_share_enum(server).await?;

        Ok(shares)
    }

    /// Connects to a share on the specified server.
    ///
    /// This method is the equivalent for executing a `net use` command on a local windows machine.
    ///
    /// Once the connection completes, the client will be able to access resource under the specified share,
    /// without needing to re-authenticate.
    ///
    /// If the share is already connected, this method will do nothing, and will log a warning indicating the double-connection attempt.
    ///
    /// ## Arguments
    /// * `target` - The UNC path of the share to connect to. The method refers to the server and share components in this path.
    /// * `user_name` - The username to use for authentication.
    /// * `password` - The password to use for authentication.
    ///
    /// ## Returns
    /// The connected share - a [`Tree`] instance.
    ///
    /// ## Notes
    /// This is the best high-level method that performs share connection, but it might not suit advanced use cases.
    ///
    /// You can replace calls to this method by performing the connection, session and share setup manually, just like it does,
    /// using the [`Client::connect`] method:
    /// ```no_run
    /// # use smb::{Client, ClientConfig, UncPath, FileCreateArgs, FileAccessMask};
    /// # use std::str::FromStr;
    /// # #[cfg(not(feature = "async"))] fn main() {}
    /// # #[cfg(feature = "async")]
    /// # #[tokio::main]
    /// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// // instantiate the client
    /// # let client = Client::new(ClientConfig::default());
    /// // Connect to a share
    /// let target_path = UncPath::from_str(r"\\server\share").unwrap();
    /// let connection = client.share_connect(&target_path, "username", "password".to_string()).await?;
    /// #   Ok(()) }
    pub async fn share_connect(
        &self,
        target: &UncPath,
        user_name: &str,
        password: String,
    ) -> crate::Result<()> {
        let identity = AuthIdentity {
            username: sspi::Username::parse(user_name).map_err(|e| Error::SspiError(e.into()))?,
            password: Secret::from(password),
        };

        self._share_connect(target, &identity).await?;

        // Establish an additional channel if multi-channel is enabled.
        let mchannel_map = self._setup_multi_channel(target, &identity).await;
        if let Ok(mchannel_map) = mchannel_map {
            let session = self.get_session(target).await?;
            log::debug!(
                "Established {} multi-channel connections",
                mchannel_map.as_ref().map(|m| m.len()).unwrap_or(0)
            );

            let address = TransportUtils::parse_socket_address(target.server())?;
            self._with_connection(address.ip(), |f| {
                let session_info = f
                    .sessions
                    .get(&session.session_id())
                    .expect("session info not found, but tree has just been created");
                if session_info.session_alt_channels.is_none() {
                    f.sessions
                        .get_mut(&session.session_id())
                        .unwrap()
                        .session_alt_channels = mchannel_map;
                }
                Ok(())
            })
            .await?;
        } else {
            log::warn!(
                "Failed to establish multi-channel connections: {}",
                mchannel_map.err().unwrap()
            );
        }

        Ok(())
    }

    /// (Internal)
    ///
    /// Performs the actual share connection logic,
    /// without setting up multi-channel.
    async fn _share_connect(&self, target: &UncPath, identity: &AuthIdentity) -> crate::Result<()> {
        if target.share().is_none() {
            return Err(crate::Error::InvalidArgument(
                "UNC path does not contain a share name.".to_string(),
            ));
        }

        let target = target.clone().with_no_path();

        let already_connected = self._with_tree(&target, |_| Ok(())).await;
        if already_connected.is_ok() {
            log::debug!(
                "Share {} is already connected. Ignoring duplicate connection attempt.",
                target
            );
            return Ok(());
        }

        let connection = self.connect(target.server()).await?;

        let session = {
            let session = connection.authenticate(identity.clone()).await?;
            log::debug!(
                "Successfully authenticated to {} as {}",
                target.server(),
                identity.username.account_name()
            );
            let session = Arc::new(session);

            let address = TransportUtils::parse_socket_address(target.server())?;
            self._with_connection(address.ip(), |f| {
                f.sessions.insert(
                    session.session_id(),
                    ClientSessionInfo {
                        session: session.clone(),
                        session_alt_channels: None,
                    },
                );
                Ok(())
            })
            .await?;

            session
        };

        let tree = session.tree_connect(&target).await?;

        let credentials = if tree.is_dfs_root()? {
            Some(identity.to_owned())
        } else {
            None
        };

        let connect_share_info = ClientConectedTree {
            session,
            tree: Arc::new(tree),
            credentials,
        };

        self.share_connects
            .lock()
            .await?
            .insert(target.clone(), connect_share_info);

        log::debug!(
            "Successfully connected to share: {}",
            target.share().unwrap()
        );

        Ok(())
    }

    async fn _get_credentials(&self, target: &UncPath) -> crate::Result<AuthIdentity> {
        self._with_tree(target, |tree| {
            tree.credentials.as_ref().cloned().ok_or_else(|| {
                Error::InvalidArgument(format!(
                    "No credentials found for DFS root share: {target}. Cannot resolve DFS path."
                ))
            })
        })
        .await
    }

    async fn _create_file(&self, path: &UncPath, args: &FileCreateArgs) -> crate::Result<Resource> {
        let tree = self.get_tree(path).await?;
        let resource = tree.create(path.path().unwrap_or(""), args).await?;
        Ok(resource)
    }

    /// Makes a connection to the specified server.
    /// If a matching connection already exists, returns it.
    ///
    /// _Note:_ You should usually connect the client through the [`Client::share_connect`] method.
    /// Using this method, for example, will require you to hold a reference to trees, or otherwise
    /// they will disconnect (as opposed to the `share_connect` method, which assures keeping the tree alive!)
    ///
    /// See [`Client::connect_to_address`] to connect to a server using a specific socket address.
    ///
    /// ## Arguments
    /// * `server` - The target server to make the connection for.
    ///
    /// ## Returns
    /// The connected connection, if succeeded. Error if failed to make the connection,
    pub async fn connect(&self, server: &str) -> crate::Result<Arc<Connection>> {
        let server_address = TransportUtils::parse_socket_address(server)?;
        self.connect_to_address(server, server_address).await
    }

    /// Makes a connection to the specified server and address.
    /// If a matching connection already exists, returns it.
    ///
    /// _Note:_ You should usually connect the client through the [`Client::share_connect`] method.
    /// Using this method, for example, will require you to hold a reference to trees, or otherwise
    /// they will disconnect (as opposed to the `share_connect` method, which assures keeping the tree alive!)
    ///
    /// See [`Client::connect`] to connect to a server using DNS resolution.
    ///
    /// ## Arguments
    /// * `server` - The target server to make the connection for.
    /// * `server_address` - An optional socket address to connect to.
    ///     If the port is set to 0, the default port will be used according to the transport that is used.
    ///
    /// ## Returns
    /// The connected connection, if succeeded. Error if failed to make the connection,
    /// or failed to connect the remote.
    pub async fn connect_to_address(
        &self,
        server: &str,
        server_address: SocketAddr,
    ) -> crate::Result<Arc<Connection>> {
        self._connect_transport_to_address(server, server_address, None)
            .await
    }

    /// Just like [`Client::connect_to_address`], but allows specifying a custom transport configuration.
    pub async fn connect_transport_to_address(
        &self,
        server: &str,
        server_address: SocketAddr,
        transport: TransportConfig,
    ) -> crate::Result<Arc<Connection>> {
        self._connect_transport_to_address(server, server_address, Some(transport))
            .await
    }

    async fn _connect_transport_to_address(
        &self,
        server: &str,
        server_address: SocketAddr,
        transport: Option<TransportConfig>,
    ) -> crate::Result<Arc<Connection>> {
        log::debug!("Creating new connection to {server}",);

        let config = if let Some(transport) = transport {
            ConnectionConfig {
                transport,
                ..self.config.connection.clone()
            }
        } else {
            self.config.connection.clone()
        };

        let conn = Connection::build(server, server_address, self.config.client_guid, config)?;

        let conn = Arc::new(conn);

        // TODO: This is a bit racy
        if let Ok(c) = self.get_connection_ip_channel(server_address.ip()).await {
            log::debug!("Reusing existing connection to {server}",);
            return Ok(c);
        }
        self._add_connection(conn.clone(), &server_address.ip())
            .await?;

        let connect_ok = conn.connect().await;

        if connect_ok.is_err() {
            let mut connections = self.connections.write().await?;
            connections.remove(&server_address.ip());
            connect_ok?;
        }

        log::debug!("Successfully connected to {server}",);

        Ok(conn)
    }

    #[maybe_async]
    async fn _add_connection(&self, to_add: Arc<Connection>, ip: &IpAddr) -> crate::Result<()> {
        let mut connections = self.connections.write().await?;
        if connections.contains_key(ip) {
            return Err(Error::InvalidArgument(format!(
                "Connection to {ip:?} already exists",
            )));
        }
        connections.insert(
            *ip,
            ClientConnectionInfo {
                connection: to_add,
                sessions: Default::default(),
            },
        );
        Ok(())
    }

    /// Returns the underlying [`Connection`] for the specified server,
    /// after a successful call to [`Client::connect`] or [`Client::share_connect`].
    pub async fn get_connection(&self, server: &str) -> crate::Result<Arc<Connection>> {
        let addr = TransportUtils::parse_socket_address(server)?;
        self.get_connection_ip(addr.ip()).await
    }

    pub async fn get_connection_ip(&self, ip: IpAddr) -> crate::Result<Arc<Connection>> {
        self.get_connection_ip_channel(ip).await
    }

    #[maybe_async]
    async fn get_connection_ip_channel(&self, ip: IpAddr) -> crate::Result<Arc<Connection>> {
        self._with_connection(ip, |c| Ok(c.connection.clone()))
            .await
    }

    pub async fn get_session(&self, path: &UncPath) -> crate::Result<Arc<Session>> {
        self._with_tree(path, |tree| Ok(tree.session.clone())).await
    }

    /// Returns a map of channel IDs to their corresponding connections for the specified session,
    pub async fn get_channels(
        &self,
        path: &UncPath,
    ) -> crate::Result<HashMap<u32, AltChannelInfo>> {
        let session = self.get_session(path).await?;
        let address = TransportUtils::parse_socket_address(path.server())?;
        let channels = self
            ._with_connection(address.ip(), |c| {
                let session_info = c.sessions.get(&session.session_id());
                session_info.ok_or_else(|| {
                    Error::NotFound(format!(
                        "No session found for session ID: {}",
                        session.session_id()
                    ))
                })?;

                let session_info = session_info.unwrap();

                let mut alt_channels = session_info
                    .session_alt_channels
                    .as_ref()
                    .map(|m| {
                        m.iter()
                            .map(|(&k, v)| (k, v.clone()))
                            .collect::<HashMap<u32, AltChannelInfo>>()
                    })
                    .unwrap_or_default();

                alt_channels.insert(
                    session.channel_id(),
                    AltChannelInfo {
                        // TODO: that's a bit shady, re-think the entire HashMap key of connections.
                        connection: c.connection.clone(),
                    },
                );
                Ok(alt_channels)
            })
            .await?;

        Ok(channels)
    }

    /// Returns the underlying [`Tree`] for the specified UNC path,
    /// after a successful call to [`Client::share_connect`].
    pub async fn get_tree(&self, path: &UncPath) -> crate::Result<Arc<Tree>> {
        self._with_tree(path, |tree| Ok(tree.tree.clone())).await
    }

    #[maybe_async]
    async fn _with_connection<F, R>(&self, ip: IpAddr, f: F) -> crate::Result<R>
    where
        F: FnOnce(&mut ClientConnectionInfo) -> crate::Result<R>,
    {
        let mut connections = self.connections.write().await?;
        let conn = connections
            .get_mut(&ip)
            .ok_or_else(|| Error::NotFound(format!("No connection found for server: {ip:?}")))?;
        f(conn)
    }

    /// Locks `share_connects`, locates the tree for the specified path,
    /// and calls the specified closure with the tree.
    #[maybe_async]
    async fn _with_tree<F, R>(&self, path: &UncPath, f: F) -> crate::Result<R>
    where
        F: FnOnce(&mut ClientConectedTree) -> crate::Result<R>,
    {
        let tree_path = path.clone().with_no_path();
        let mut sc = self.share_connects.lock().await?;
        let sc = sc.get_mut(&tree_path).ok_or_else(|| {
            Error::NotFound(format!("No connected share found for path: {path}",))
        })?;
        f(sc)
    }

    /// Creates (or opens) a file on the specified path, using the specified args.
    ///
    /// See [`FileCreateArgs`] for detailed information regarding the file open options.
    ///
    /// The function also handles DFS resolution if it is enabled in the client configuration.
    ///
    /// ## Arguments
    /// * `path` - The UNC path of the file to create or open.
    /// * `args` - The arguments to use when creating or opening the file.
    ///
    /// ## Returns
    /// A result containing the created or opened file resource, or an error.
    pub async fn create_file(
        &self,
        path: &UncPath,
        args: &FileCreateArgs,
    ) -> crate::Result<Resource> {
        let file_result = self._create_file(path, args).await;

        let resource = match file_result {
            Ok(file) => Ok(file),
            Err(Error::ReceivedErrorMessage(Status::U32_PATH_NOT_COVERED, _)) => {
                if self.config.dfs {
                    DfsResolver::new(self).resolve_to_dfs_file(path, args).await
                } else {
                    Err(Error::UnsupportedOperation(
                        "DFS is not enabled, but the server returned path not covered (dfs must be enabled in config to resolve the path!).".to_string(),
                    ))
                }
            }
            x => x,
        }?;

        Ok(resource)
    }

    /// Similar [`Client::share_connect`], but connects to the SMB pipes share (IPC$).
    ///
    /// After calling this method, the [`Client::open_pipe`] method can be used to open named pipes.
    pub async fn ipc_connect(
        &self,
        server: &str,
        username: &str,
        password: String,
    ) -> crate::Result<()> {
        let ipc_share = UncPath::ipc_share(server)?;
        let identity = AuthIdentity {
            username: sspi::Username::parse(username).map_err(|e| Error::SspiError(e.into()))?,
            password: Secret::from(password),
        };
        self._share_connect(&ipc_share, &identity).await
    }

    pub async fn _ipc_connect(&self, server: &str, identity: &AuthIdentity) -> crate::Result<()> {
        let ipc_share = UncPath::ipc_share(server)?;
        self._share_connect(&ipc_share, identity).await
    }

    /// Opens a named pipe on the specified server.
    /// Use this when intending to communicate with a service using a named pipe, for convenience.
    ///
    /// ## Arguments
    /// * `server` - The name of the server hosting the pipe.
    /// * `pipe_name` - The name of the pipe to open.
    ///
    /// ## Returns
    /// A result containing the opened [`Pipe`] resource, or an error.
    ///
    /// ## Notes
    /// before calling this method, you MUST call the [`Client::ipc_connect`] method,
    /// that connects to the IPC$ share on the server, which then allows for communication with the named pipe.
    pub async fn open_pipe(&self, server: &str, pipe_name: &str) -> crate::Result<Pipe> {
        let path = UncPath::ipc_share(server)?.with_path(pipe_name);
        let pipe = self
            ._create_file(&path, &FileCreateArgs::make_pipe())
            .await?;
        match pipe {
            Resource::Pipe(file) => {
                log::info!("Successfully opened pipe: {pipe_name}",);
                Ok(file)
            }
            _ => crate::Result::Err(Error::InvalidMessage(
                "Expected a pipe resource, but got something else.".to_string(),
            )),
        }
    }

    /// If multi-channel is enabled in the client configuration, and the server supports it,
    /// this method will attempt to establish an additional channel to the server,
    /// using a different network interface, if available.
    ///
    /// This method returns a map of channel IDs to their corresponding connections.
    #[maybe_async]
    async fn _setup_multi_channel(
        &self,
        unc: &UncPath,
        identity: &AuthIdentity,
    ) -> crate::Result<Option<HashMap<u32, AltChannelInfo>>> {
        if unc.is_ipc_share() {
            return Err(Error::InvalidArgument(
                "Cannot setup multi-channel for IPC$ share.".to_string(),
            ));
        }

        if !self.config.connection.multichannel.is_enabled() {
            log::debug!("Multi-channel is not enabled in client configuration. Skipping setup.");
            return Ok(None);
        }

        let primary_conn_info = {
            let opened_conn_info = self.get_connection(unc.server()).await?;
            opened_conn_info
                .conn_info()
                .expect("Primary connection must be negotiated.")
                .clone()
        };

        if !primary_conn_info.negotiation.caps.multi_channel() {
            log::debug!("Multi-channel is not enabled for connection to {unc}. Skipping setup.");
            return Ok(None);
        }

        log::debug!(
            "Multi-channel is enabled for connection to {unc}. Scanning for alternate channels."
        );

        // Connect IPC and query network interfaces.
        let ipc_share = UncPath::ipc_share(unc.server())?;
        self._ipc_connect(ipc_share.server(), identity).await?;
        let ipc_tree = self.get_tree(&ipc_share).await?;
        let network_interfaces = ipc_tree
            .as_ipc_tree()
            .unwrap()
            .query_network_interfaces()
            .await?;

        let mut result = HashMap::new();

        // Bind to other, non-rdma network interfaces.
        let other_interfaces = MultiChannelUtils::get_alt_interface_addresses(
            &network_interfaces,
            primary_conn_info.server_address.ip(),
            self.config.connection.multichannel.is_rdma_only(),
        )?;

        if other_interfaces.is_empty() {
            log::warn!(
                "Multi-channel setup failed: unable to determine the current primary network interface.
                This usually means the SMB server is not on the same local network as the client, and multi-channel cannot be used.
                Available interfaces: {network_interfaces:?}",
            );
            return Ok(None);
        }

        let session = self.get_session(unc).await?;
        for (if_index, &interface) in other_interfaces.iter() {
            let address = interface.sockaddr.socket_addr();
            log::debug!("Found alternate interface for multi-channel: {if_index} => {address}");

            let (connection, channel) = {
                let connection = if interface.capability.rdma() && cfg!(feature = "rdma") {
                    self._connect_transport_to_address(
                        unc.server(),
                        address,
                        #[cfg(feature = "rdma")]
                        Some(TransportConfig::Rdma(crate::transport::RdmaConfig {
                            rdma_type: self.config.rdma_type.ok_or_else(|| {
                                Error::InvalidConfiguration(
                                    "RDMA transport type is not specified in client configuration."
                                        .to_string(),
                                )
                            })?,
                        })),
                        #[cfg(not(feature = "rdma"))]
                        None,
                    )
                    .await?
                } else {
                    self.connect_to_address(unc.server(), address).await?
                };

                let channel = connection.bind_session(&session, identity.clone()).await?;

                (connection, channel)
            };
            result.insert(channel, AltChannelInfo { connection });
        }

        Ok(Some(result))
    }
}

impl Default for Client {
    /// Starts the client with default configuration.
    fn default() -> Self {
        Client::new(ClientConfig::default())
    }
}

/// Internal helper struct for implementing DFS referral resolution simply and easily.
struct DfsResolver<'a> {
    client: &'a Client,
}

impl<'a> DfsResolver<'a> {
    fn new(client: &'a Client) -> Self {
        DfsResolver { client }
    }

    /// Resolves the DFS referral for the given UNC path and re-creates a file on the resolved path.
    #[maybe_async]
    async fn resolve_to_dfs_file(
        &self,
        dfs_path: &UncPath,
        args: &FileCreateArgs,
    ) -> crate::Result<Resource> {
        let dfs_ref_paths = self.get_dfs_refs(dfs_path).await?;

        // Re-use the same credentials for the DFS referral.
        let dfs_creds = self.client._get_credentials(dfs_path).await?;

        // Open the next DFS referral. Try each referral path, since some may be down.
        for ref_unc_path in dfs_ref_paths.iter() {
            // Try opening the share. Log failure, and try next ref.
            if let Err(e) = self.client._share_connect(ref_unc_path, &dfs_creds).await {
                log::error!("Failed to open DFS referral: {e}",);
                continue;
            };

            let resource = self
                .client
                ._create_file(ref_unc_path, args)
                .await
                .map_err(|e| {
                    log::error!("Failed to create file on DFS referral: {e}",);
                    e
                })?;
            log::info!("Successfully created file on DFS referral: {ref_unc_path}",);
            return Ok(resource);
        }
        Err(Error::DfsError(dfs_path.clone()))
    }

    /// Returns a list of DFS referral paths for the given input UNC path.
    #[maybe_async]
    async fn get_dfs_refs(&self, unc: &UncPath) -> crate::Result<Vec<UncPath>> {
        log::debug!("Resolving DFS referral for {unc}");
        let dfs_path_string = unc.to_string();

        let dfs_refs = {
            let dfs_root = &self.client.get_tree(unc).await?;
            dfs_root
                .as_dfs_tree()?
                .dfs_get_referrals(&dfs_path_string)
                .await?
        };
        if !dfs_refs.referral_header_flags.storage_servers() {
            return Err(Error::InvalidMessage(
                "DFS referral does not contain storage servers".to_string(),
            ));
        }

        let mut paths = vec![];
        // Resolve the DFS referral entries.
        for (indx, curr_referral) in dfs_refs.referral_entries.iter().enumerate() {
            let is_first = indx == 0;
            paths.push(self.ref_entry_to_dfs_target(
                curr_referral,
                dfs_refs.path_consumed as usize,
                &dfs_path_string,
                is_first,
            )?);
        }
        Ok(paths)
    }

    /// Given a [`ReferralEntry`] result from a DFS referral query, returns a ready UNC path for the DFS target.
    fn ref_entry_to_dfs_target(
        &self,
        entry: &ReferralEntry,
        path_consumed: usize,
        dfs_path_string: &str,
        is_first: bool,
    ) -> crate::Result<UncPath> {
        match &entry.value {
            ReferralEntryValue::V4(v4) => {
                // First? verify flags.
                if v4.referral_entry_flags == 0 && is_first {
                    return Err(Error::InvalidMessage(
                        "First DFS Referral is not primary one, invalid message!".to_string(),
                    ));
                }
                // The path consumed is a wstring index.
                let index_end_of_match = path_consumed / std::mem::size_of::<u16>();

                if index_end_of_match > dfs_path_string.len() {
                    return Err(Error::InvalidMessage(
                        "DFS path consumed is out of bounds".to_string(),
                    ));
                }

                let suffix = if index_end_of_match < dfs_path_string.len() {
                    dfs_path_string
                        .char_indices()
                        .nth(index_end_of_match)
                        .ok_or_else(|| {
                            Error::InvalidMessage("DFS path consumed is out of bounds".to_string())
                        })?
                        .0
                } else {
                    // Empty -- exact cover.
                    dfs_path_string.len()
                };

                let unc_str_dest = "\\".to_string()
                    + &v4.refs.network_address.to_string()
                    + &dfs_path_string[suffix..];
                let unc_path = UncPath::from_str(&unc_str_dest)?;
                log::debug!("Resolved DFS referral to {unc_path}",);
                Ok(unc_path)
            }
            _ => Err(Error::UnsupportedOperation(
                "Unsupported DFS referral entry type".to_string(),
            )),
        }
    }
}

struct MultiChannelUtils;
impl MultiChannelUtils {
    /// Given the list of network interfaces on the client machine,
    /// this returns a map of relevant interface indexes to their IP addresses,
    /// which are relevant for multi-channel connections.
    fn get_alt_interface_addresses(
        network_interfaces: &[NetworkInterfaceInfo],
        current_server_address: IpAddr,
        rdma_only: bool,
    ) -> crate::Result<HashMap<u32, &NetworkInterfaceInfo>> {
        let current_primary_interface = network_interfaces
            .iter()
            .find(|iface| iface.sockaddr.socket_addr().ip() == current_server_address);

        if current_primary_interface.is_none() {
            return Ok(HashMap::new());
        }

        let current_primary_interface = current_primary_interface.unwrap();

        let index_to_address = network_interfaces
            .iter()
            .filter(|iface| {
                iface.sockaddr.socket_addr().is_ipv4()
                    && iface.if_index != current_primary_interface.if_index
            }) // TODO: IPv6; RDMA
            .filter(|iface| {
                if rdma_only {
                    iface.capability.rdma()
                } else {
                    true
                }
            })
            .map(|iface| (iface.if_index, iface))
            .collect();

        Ok(index_to_address)
    }
}
