//! Session information and state

use std::sync::Arc;

use crate::dialects::DialectImpl;

use crate::connection::connection_info::ConnectionInfo;
use crate::connection::preauth_hash::PreauthHashValue;
use crate::crypto::{
    CryptoError, DerivedKey, KeyToDerive, kbkdf_hmacsha256, make_encrypting_algo, make_signing_algo,
};
use smb_msg::{Dialect, EncryptionCipher, SessionFlags, SigningAlgorithmId};

use super::{MessageDecryptor, MessageEncryptor, MessageSigner};

#[derive(Debug)]
struct SessionAlgos {
    encryptor: Option<MessageEncryptor>,
    decryptor: Option<MessageDecryptor>,
}

#[derive(Clone)]
struct ChannelAlgos {
    signer: MessageSigner,
}

/// A factory for creating session and channel algorithms.
///
/// See [`SessionAlgos::new_session`] and [`SessionAlgos::new_channel`].
struct SessionAlgosFactory;
impl SessionAlgosFactory {
    const NO_PREAUTH_HASH_DERIVE_SIGN_CTX: &'static [u8] = b"SmbSign\x00";
    const NO_PREAUTH_HASH_DERIVE_ENCRYPT_S2C_CTX: &'static [u8] = b"ServerOut\x00";
    const NO_PREAUTH_HASH_DERIVE_ENCRYPT_C2S_CTX: &'static [u8] = b"ServerIn \x00";

    pub fn new_session(
        session_key: &KeyToDerive,
        preauth_hash: &Option<PreauthHashValue>,
        info: &ConnectionInfo,
    ) -> crate::Result<SessionAlgos> {
        if (info.negotiation.dialect_rev == Dialect::Smb0311) != preauth_hash.is_some() {
            return Err(crate::Error::InvalidMessage(
                "Preauth hash must be present for SMB3.1.1, and not present for SMB3.0.2 or older revisions."
                    .to_string(),
            ));
        }

        if cfg!(feature = "__debug-dump-keys") {
            log::debug!(
                "Building session algorithms for dialect {:?} with session key {:02x?} and preauth hash {:02x?}",
                info.negotiation.dialect_rev,
                session_key,
                preauth_hash.as_ref().map(|h| h.as_ref())
            );
        }

        if info.negotiation.dialect_rev.is_smb3() {
            Self::smb3xx_make_ciphers(session_key, preauth_hash, info)
        } else {
            Ok(SessionAlgos {
                encryptor: None,
                decryptor: None,
            })
        }
    }

    pub fn new_channel(
        channel_session_key: &KeyToDerive,
        preauth_hash: &Option<PreauthHashValue>,
        info: &ConnectionInfo,
    ) -> crate::Result<ChannelAlgos> {
        let deriver = KeyDeriver::new(channel_session_key);
        let signer = if info.negotiation.dialect_rev.is_smb3() {
            Self::smb3xx_make_signer(
                &deriver,
                info.negotiation.signing_algo,
                &info.dialect,
                preauth_hash,
            )?
        } else {
            Self::smb2_make_signer(channel_session_key, info)?
        };

        Ok(ChannelAlgos { signer })
    }

    fn smb2_make_signer(
        session_key: &KeyToDerive,
        info: &ConnectionInfo,
    ) -> Result<MessageSigner, CryptoError> {
        debug_assert!(info.negotiation.dialect_rev < Dialect::Smb030);
        Ok(MessageSigner::new(make_signing_algo(
            SigningAlgorithmId::HmacSha256,
            session_key,
        )?))
    }

    fn smb3xx_make_ciphers(
        session_key: &KeyToDerive,
        preauth_hash: &Option<PreauthHashValue>,
        info: &ConnectionInfo,
    ) -> crate::Result<SessionAlgos> {
        let deriver = KeyDeriver::new(session_key);

        let (enc, dec) = if let Some((e, d)) =
            Self::smb3xx_make_cipher_pair(&deriver, info, preauth_hash)?
        {
            (Some(e), Some(d))
        } else {
            // There's no matching algorithm, so no encryption/decryption.
            // if the encryption is required, then we should fail ASAP.
            if info.config.encryption_mode.is_required() {
                return Err(crate::Error::InvalidMessage(
                    "Encryption is required, seems to be unsupported by the server with current config.".to_string(),
                ));
            };
            (None, None)
        };

        Ok(SessionAlgos {
            encryptor: enc,
            decryptor: dec,
        })
    }

    fn smb3xx_make_signer(
        deriver: &KeyDeriver,
        signing_algo: Option<SigningAlgorithmId>,
        dialect: &Arc<DialectImpl>,
        preauth_hash: &Option<PreauthHashValue>,
    ) -> Result<MessageSigner, CryptoError> {
        let signing_key = deriver.derive(
            dialect.get_signing_derive_label(),
            Self::preauth_hash_or(preauth_hash, Self::NO_PREAUTH_HASH_DERIVE_SIGN_CTX),
        )?;
        let signing_algo = match signing_algo {
            Some(a) => a,
            None => dialect.default_signing_algo(),
        };
        Ok(MessageSigner::new(make_signing_algo(
            signing_algo,
            &signing_key,
        )?))
    }

    fn smb3xx_make_cipher_pair(
        deriver: &KeyDeriver,
        info: &ConnectionInfo,
        preauth_hash: &Option<PreauthHashValue>,
    ) -> Result<Option<(MessageEncryptor, MessageDecryptor)>, CryptoError> {
        // Not supported
        if !info.dialect.supports_encryption() {
            return Ok(None);
        }
        // Disabled in config
        if info.config.encryption_mode.is_disabled() {
            return Ok(None);
        }

        // If dialect is 3.1.1, then cipher is taken from negotiation.
        let cipher = if info.negotiation.dialect_rev == Dialect::Smb0311 {
            match info.negotiation.encryption_cipher {
                Some(x) => x,
                None => return Ok(None),
            }
        } else {
            // Otherwise, we use AES-128-CCM.
            EncryptionCipher::Aes128Ccm
        };

        // Check if the cipher is supported in the current build.
        if !crate::crypto::ENCRYPTING_ALGOS.contains(&cipher) {
            return Ok(None);
        }

        // Make the keys.
        let enc_key = deriver.derive(
            info.dialect.c2s_encrypt_key_derive_label(),
            Self::preauth_hash_or(preauth_hash, Self::NO_PREAUTH_HASH_DERIVE_ENCRYPT_C2S_CTX),
        )?;
        let dec_key = deriver.derive(
            info.dialect.s2c_encrypt_key_derive_label(),
            Self::preauth_hash_or(preauth_hash, Self::NO_PREAUTH_HASH_DERIVE_ENCRYPT_S2C_CTX),
        )?;

        Ok(Some((
            MessageEncryptor::new(make_encrypting_algo(cipher, &enc_key)?),
            MessageDecryptor::new(make_encrypting_algo(cipher, &dec_key)?),
        )))
    }

    fn preauth_hash_or<'a>(
        preauth_hash: &'a Option<PreauthHashValue>,
        else_val: &'a [u8],
    ) -> &'a [u8] {
        preauth_hash
            .as_ref()
            .map(|h| h.as_ref())
            .unwrap_or(else_val)
    }
}

#[derive(Debug, Default)]
enum SessionInfoState {
    #[default]
    /// Initial state.
    Initial,
    /// The session is being set up, but not fully ready yet.
    SettingUp {
        algos: SessionAlgos,
        allow_unsigned: bool,
    },
    /// The session is ready for use, with the given algorithms and flags.
    Ready {
        algos: SessionAlgos,
        flags: SessionFlags,
        force_encryption: bool,
    },
    /// The session is invalid, and should not be used anymore.
    Invalid,
}

/// Holds the information of a session, to be used for actions requiring data from session,
/// without accessing the entire session object.
/// This struct should be single-per-session, and wrapped in a shared pointer.
pub struct SessionInfo {
    session_id: u64,
    state: Option<SessionInfoState>,
}

#[derive(Clone)]
pub struct ChannelInfo {
    id: u32,
    algos: ChannelAlgos,
    valid: bool,

    #[cfg(feature = "ksmbd-multichannel-compat")]
    /// Indicates whether this channel was created temporarily for multichannel setup.
    /// This is relevant for compatibility with ksmbd. See [`crate::connection::Transformer::verify_plain_incoming`]
    binding: bool,
}

impl ChannelInfo {
    pub fn new(
        internal_id: u32,
        channel_session_key: &KeyToDerive,
        preauth_hash: &Option<PreauthHashValue>,
        info: &ConnectionInfo,
    ) -> crate::Result<Self> {
        let algos = SessionAlgosFactory::new_channel(channel_session_key, preauth_hash, info)?;
        Ok(Self {
            id: internal_id,
            algos,
            valid: true,
            #[cfg(feature = "ksmbd-multichannel-compat")]
            binding: false,
        })
    }

    #[cfg(feature = "ksmbd-multichannel-compat")]
    pub(crate) fn with_binding(mut self, binding: bool) -> Self {
        self.binding = binding;
        self
    }

    #[cfg(feature = "ksmbd-multichannel-compat")]
    pub fn is_binding(&self) -> bool {
        self.binding
    }

    pub fn signer(&self) -> crate::Result<&MessageSigner> {
        if !self.valid {
            return Err(crate::Error::InvalidState(
                "Channel is not valid, cannot get signer.".to_string(),
            ));
        }
        Ok(&self.algos.signer)
    }

    pub fn invalidate(&mut self) {
        self.valid = false;
    }

    pub fn id(&self) -> u32 {
        self.id
    }
}

impl SessionInfo {
    /// Creates a new session info object.
    pub fn new(session_id: u64) -> Self {
        Self {
            session_id,
            state: Some(SessionInfoState::Initial),
        }
    }

    /// # Returns
    /// The session's ID.
    pub fn id(&self) -> u64 {
        self.session_id
    }

    /// Starts the session setup process.
    pub fn setup(
        &mut self,
        session_key: &KeyToDerive,
        preauth_hash: &Option<PreauthHashValue>,
        info: &ConnectionInfo,
    ) -> crate::Result<()> {
        if !matches!(self.state, Some(SessionInfoState::Initial)) {
            return Err(crate::Error::InvalidState(
                "Session is not in state initialized, cannot set up.".to_string(),
            ));
        }

        let algos = SessionAlgosFactory::new_session(session_key, preauth_hash, info)?;
        log::trace!("Session algos set up: {algos:?}");

        let info_allows_unsigned = info.config.allow_unsigned_guest_access;

        self.state = Some(SessionInfoState::SettingUp {
            algos,
            allow_unsigned: info_allows_unsigned,
        });

        Ok(())
    }

    /// Turns the session into a ready state.
    ///
    /// Verifies the session flags against the connection config, and sets them in the session info.
    pub fn ready(&mut self, flags: SessionFlags, conn_info: &ConnectionInfo) -> crate::Result<()> {
        if !self.is_setting_up() {
            return Err(crate::Error::InvalidState(
                "Session is not set up, cannot set flags.".to_string(),
            ));
        }

        // When session flags are finally set, make sure the server accepts encryption,
        // if it is required for us. Also, make sure it is not a null/guest session.

        let force_encryption = if conn_info.config.encryption_mode.is_required() {
            if !flags.encrypt_data() {
                log::debug!(
                    "Note! session does not require encryption, but it is required by the connection config. Forcing encryption."
                );
            }
            let encryption_ok =
                if let SessionInfoState::SettingUp { algos, .. } = self.state.as_ref().unwrap() {
                    algos.encryptor.is_some() && algos.decryptor.is_some()
                } else {
                    false
                };
            if !encryption_ok {
                return Err(crate::Error::InvalidMessage(
                    "Encryption is required by the connection config, but is not available!"
                        .to_string(),
                ));
            }
            true
        } else {
            false
        };

        if !conn_info.config.allow_unsigned_guest_access && flags.is_guest_or_null_session() {
            return Err(crate::Error::InvalidMessage(
                "Signing may be disabled to allow guest or anonymous logins.".to_string(),
            ));
        }

        self.state = match self.state.take() {
            Some(SessionInfoState::SettingUp { algos, .. }) => Some(SessionInfoState::Ready {
                algos,
                flags,
                force_encryption,
            }),
            _ => unreachable!(),
        };
        log::debug!("Session {} flags set: {:?}", self.session_id, flags);
        Ok(())
    }

    /// Changes the state of the session to be invalid,
    /// so it can no longer be used.
    pub fn invalidate(&mut self) {
        log::debug!("Invalidating session {}", self.session_id);
        self.state = Some(SessionInfoState::Invalid);
    }

    /// Returns whether the session is only initialized (i.e. in the initial state).
    pub fn is_initial(&self) -> bool {
        matches!(self.state, Some(SessionInfoState::Initial))
    }

    /// Returns whether the session is setting up.
    pub fn is_setting_up(&self) -> bool {
        matches!(self.state, Some(SessionInfoState::SettingUp { .. }))
    }

    /// Returns whether the session is ready for use.
    pub fn is_ready(&self) -> bool {
        matches!(self.state, Some(SessionInfoState::Ready { .. }))
    }

    /// Returns whether the session was invalidated (by calling [`SessionInfo::invalidate`]).
    pub fn is_invalid(&self) -> bool {
        matches!(self.state, Some(SessionInfoState::Invalid))
    }

    /// Returns whether encryption is needed for this session:
    /// * Either because the session flags require it,
    /// * Or because the connection config requires it.
    ///   If the session is not ready, it will return an error.
    pub fn should_encrypt(&self) -> crate::Result<bool> {
        match &self.state {
            Some(SessionInfoState::Ready {
                flags,
                force_encryption,
                ..
            }) => Ok(flags.encrypt_data() || *force_encryption),
            _ => Err(crate::Error::InvalidState(
                "Session is not ready!".to_string(),
            )),
        }
    }

    /// Returns whether the session is a guest or anonymous session.
    /// If the session is not setting up or ready, it will return an error.
    pub fn allow_unsigned(&self) -> crate::Result<bool> {
        match &self.state {
            Some(SessionInfoState::Ready { flags, .. }) => Ok(flags.is_guest_or_null_session()),
            Some(SessionInfoState::SettingUp { allow_unsigned, .. }) => Ok(*allow_unsigned),
            _ => Err(crate::Error::InvalidState(
                "Session is not setting up or ready!".to_string(),
            )),
        }
    }

    pub fn decryptor(&self) -> crate::Result<Option<&MessageDecryptor>> {
        match &self.state {
            Some(SessionInfoState::Ready { algos, .. }) => Ok(algos.decryptor.as_ref()),
            _ => Err(crate::Error::InvalidState(
                "Session is not ready, cannot get decryptor.".to_string(),
            )),
        }
    }

    pub fn encryptor(&self) -> crate::Result<Option<&MessageEncryptor>> {
        match &self.state {
            Some(SessionInfoState::Ready { algos, .. }) => Ok(algos.encryptor.as_ref()),
            _ => Err(crate::Error::InvalidState(
                "Session is not ready, cannot get encryptor.".to_string(),
            )),
        }
    }
}

/// A helper struct for deriving SMB2 keys from a session key and preauth hash.
///
/// This is relevant for SMB3+ dialects.
struct KeyDeriver<'a> {
    session_key: &'a KeyToDerive,
}

impl<'a> KeyDeriver<'a> {
    #[inline]
    pub fn new(session_key: &'a KeyToDerive) -> Self {
        Self { session_key }
    }

    #[inline]
    pub fn derive(&self, label: &[u8], context: &'a [u8]) -> Result<DerivedKey, CryptoError> {
        kbkdf_hmacsha256(self.session_key, label, context)
    }
}

#[cfg(test)]
mod tests {
    use super::KeyDeriver;

    static SESSION_KEY: [u8; 16] = [
        0xDA, 0x90, 0xB1, 0xDF, 0x80, 0x5C, 0x34, 0x9F, 0x88, 0x86, 0xBA, 0x02, 0x9E, 0xA4, 0x5C,
        0xB6,
    ];

    static PREAUTH_HASH: [u8; 64] = [
        0x47, 0x95, 0x78, 0xb1, 0x87, 0x23, 0x05, 0x6a, 0x4c, 0x3e, 0x6f, 0x73, 0x2f, 0x36, 0xf1,
        0x9c, 0xcc, 0xdd, 0x51, 0x6f, 0x49, 0x56, 0x6b, 0xa0, 0x43, 0xce, 0x59, 0x6a, 0x13, 0x42,
        0x27, 0xd9, 0x64, 0xef, 0x0a, 0xa6, 0xa6, 0x27, 0x1a, 0xfe, 0x4f, 0xe6, 0x4b, 0x4d, 0x8c,
        0xb2, 0xe6, 0xa1, 0x95, 0x11, 0xed, 0xbb, 0xf6, 0xd7, 0x7d, 0xce, 0xf0, 0x33, 0xda, 0xed,
        0x8c, 0x71, 0x81, 0xb2,
    ];

    static SIGNING_KEY: [u8; 16] = [
        0x6D, 0xAC, 0xCE, 0xDE, 0x5B, 0x4E, 0x36, 0x08, 0xAD, 0x6E, 0xA5, 0x47, 0x33, 0xCA, 0x31,
        0x63,
    ];

    #[test]
    pub fn test_key_deriver() {
        let d = KeyDeriver::new(&SESSION_KEY);
        let k = d.derive(b"SMBSigningKey\x00", &PREAUTH_HASH).unwrap();
        assert_eq!(k, SIGNING_KEY);
    }
}
