//! Connection configuration settings.

use std::time::Duration;

use smb_msg::Dialect;
use smb_transport::config::*;

/// Specifies the encryption mode for the connection.
/// Use this as part of the [ConnectionConfig] to specify the encryption mode for the connection.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EncryptionMode {
    /// Encryption is allowed but not required, it's up to the server to decide.
    #[default]
    Allowed,
    /// Encryption is required, and connection will fail if the server does not support it.
    Required,
    /// Encryption is disabled, server might fail the connection if it requires encryption.
    Disabled,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum MultiChannelConfig {
    /// Multi-channel is disabled.
    #[default]
    Disabled,
    /// Multi-channel is always enabled, if supported by the server and client.
    Always,
    /// Multi-channel is enabled only if using RDMA transport, and if supported by the server and client.
    #[cfg(feature = "rdma")]
    RdmaOnly,
}

impl MultiChannelConfig {
    /// Returns whether multichannel of any form is enabled.
    pub fn is_enabled(&self) -> bool {
        match self {
            MultiChannelConfig::Always => true,
            #[cfg(feature = "rdma")]
            MultiChannelConfig::RdmaOnly => true,
            MultiChannelConfig::Disabled => false,
        }
    }

    /// Returns whether multichannel is enabled only for RDMA transport.
    pub fn is_rdma_only(&self) -> bool {
        #[cfg(feature = "rdma")]
        return matches!(self, MultiChannelConfig::RdmaOnly);
        #[cfg(not(feature = "rdma"))]
        return false;
    }
}

impl EncryptionMode {
    /// Returns true if encryption is required.
    pub fn is_required(&self) -> bool {
        matches!(self, Self::Required)
    }

    /// Returns true if encryption is disabled.
    pub fn is_disabled(&self) -> bool {
        matches!(self, Self::Disabled)
    }
}

/// Specifies the authentication methods (SSPs) to be used for the connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthMethodsConfig {
    /// Whether to try using NTLM authentication.
    /// This is enabled by default.
    pub ntlm: bool,

    /// Whether to try using Kerberos authentication.
    /// This is supported only if the `kerberos` feature is enabled,
    /// and if so, enabled by default.
    pub kerberos: bool,
}

impl Default for AuthMethodsConfig {
    fn default() -> Self {
        Self {
            ntlm: true,
            kerberos: cfg!(feature = "kerberos"),
        }
    }
}

/// Specifies the configuration for a connection.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ConnectionConfig {
    /// Specifies the server port to connect to.
    /// If unset, defaults to the default port for the selected transport protocol.
    pub port: Option<u16>,

    /// Specifies the timeout for the connection.
    /// If unset, defaults to [`ConnectionConfig::DEFAULT_TIMEOUT`].
    /// 0 means wait forever.
    /// Access the timeout using the [`ConnectionConfig::timeout()`] method.
    pub timeout: Option<Duration>,

    /// Specifies the minimum and maximum dialects to be used in the connection.
    ///
    /// Note, that if set, the minimum dialect must be less than or equal to the maximum dialect.
    pub min_dialect: Option<Dialect>,

    /// Specifies the minimum and maximum dialects to be used in the connection.
    ///
    /// Note, that if set, the minimum dialect must be less than or equal to the maximum dialect.
    pub max_dialect: Option<Dialect>,

    /// Sets the encryption mode for the connection.
    /// See [EncryptionMode] for more information.
    pub encryption_mode: EncryptionMode,

    /// Sets whether signing may be skipped for guest or anonymous access.
    pub allow_unsigned_guest_access: bool,

    /// Whether to enable compression, if supported by the server and specified connection dialects.
    ///
    /// Note: you must also have compression features enabled when building the crate, otherwise compression
    /// would not be available. *The compression feature is enabled by default.*
    pub compression_enabled: bool,

    /// Multi-channel configuration
    pub multichannel: MultiChannelConfig,

    /// Specifies the client host name to be used in the SMB2 negotiation & session setup.
    pub client_name: Option<String>,

    /// Specifies whether to disable support for Server-to-client notifications.
    /// If set to true, the client will NOT support notifications.
    pub disable_notifications: bool,

    /// Whether to avoid multi-protocol negotiation,
    /// and perform smb2-only negotiation. This results in a
    /// faster negotiation process, but it might fail with some servers,
    pub smb2_only_negotiate: bool,

    /// Specifies the transport protocol to be used for the connection.
    pub transport: TransportConfig,

    /// Configures valid authentication methods (SSPs) for the connection.
    /// See [`AuthMethodsConfig`] for more information.
    pub auth_methods: AuthMethodsConfig,

    /// The number of SMB2 credits to request for the connection.
    /// If not configured, uses a default value.
    ///
    /// The higher number of credits, the more concurrent requests can be sent on the connection.
    /// However, some servers may not issue such high number of credits.
    ///
    /// This is the somewhat similar to the [`-Smb2MaxCredits`](<https://learn.microsoft.com/en-us/powershell/module/smbshare/set-smbserverconfiguration?view=windowsserver2025-ps#-smb2creditsmax>)
    /// parameter in the `Set-SmbServerConfiguration` PowerShell cmdlet, but from the client's side.
    pub credits_backlog: Option<u16>,

    /// The default size, in bytes, of the buffer that can be used for
    /// [`ResourceHandle::query_info`][crate::ResourceHandle::query_info], [`ResourceHandle::query_fs_info`][crate::ResourceHandle::query_fs_info],
    /// [`ResourceHandle::query_security_info`][crate::ResourceHandle::query_security_info], [`Directory::query_quota_info`][crate::Directory::query_quota_info],
    /// their respective `set_*_info` counterparts (such as [`ResourceHandle::set_info`][crate::ResourceHandle::set_info]),
    /// [`Directory::query`][crate::Directory::query] and [`Directory::watch`][crate::Directory::watch] operations.
    pub default_transaction_size: Option<u32>,
}

impl ConnectionConfig {
    pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

    /// Validates common configuration settings.
    pub fn validate(&self) -> crate::Result<()> {
        // Make sure dialects min <= max.
        if let (Some(min), Some(max)) = (self.min_dialect, self.max_dialect)
            && min > max
        {
            return Err(crate::Error::InvalidConfiguration(
                "Minimum dialect is greater than maximum dialect".to_string(),
            ));
        }
        // Make sure transport is supported by the dialects.
        #[cfg(feature = "quic")]
        if let Some(min) = self.min_dialect
            && min < Dialect::Smb0311
            && matches!(self.transport, TransportConfig::Quic(_))
        {
            return Err(crate::Error::InvalidConfiguration(
                "SMB over QUIC is not supported by the selected dialect".to_string(),
            ));
        }

        if let Some(default_transaction_size) = self.default_transaction_size
            && default_transaction_size == 0
        {
            return Err(crate::Error::InvalidConfiguration(
                "Default transaction size cannot be zero".to_string(),
            ));
        }
        Ok(())
    }

    /// Returns the effective timeout to be used if [`timeout`][`Self::timeout`] is not set.
    pub fn timeout(&self) -> Duration {
        self.timeout.unwrap_or(Self::DEFAULT_TIMEOUT)
    }

    pub const DEFAULT_TRANSACTION_SIZE: u32 = 0x10_000;

    /// Returns the effective value to be used if [`default_transaction_size`][`Self::default_transaction_size`] is not set.
    pub fn default_transaction_size(&self) -> u32 {
        self.default_transaction_size
            .unwrap_or(Self::DEFAULT_TRANSACTION_SIZE)
    }
}
