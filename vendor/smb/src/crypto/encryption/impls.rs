use crypto_common::array::{Array, ArraySize};
use smb_msg::*;

use crate::crypto::{CryptoError, EncryptingAlgo};

/// Returns the nonce to be used for encryption/decryption (trimmed to the required size),
/// as the rest of the nonce is expected to be zero.
pub(crate) fn trim_nonce<U: ArraySize>(
    algo: &dyn EncryptingAlgo,
    nonce: &EncryptionNonce,
) -> Array<u8, U> {
    // Sanity: the rest of the nonce is expected to be zero.
    debug_assert!(nonce[algo.nonce_size()..].iter().all(|&x| x == 0));
    Array::try_from(&nonce[..algo.nonce_size()]).unwrap()
}

/// A list of all the supported encryption algorithms,
/// available in the current build.
pub const ENCRYPTING_ALGOS: &[EncryptionCipher] = &[
    #[cfg(feature = "encrypt_aes128ccm")]
    EncryptionCipher::Aes128Ccm,
    #[cfg(feature = "encrypt_aes256ccm")]
    EncryptionCipher::Aes256Ccm,
    #[cfg(feature = "encrypt_aes128gcm")]
    EncryptionCipher::Aes128Gcm,
    #[cfg(feature = "encrypt_aes256gcm")]
    EncryptionCipher::Aes256Gcm,
];

/// A factory method that instantiates a [`EncryptingAlgo`] implementation
/// based on the provided encryption algorithm and key.
pub fn make_encrypting_algo(
    encrypting_algorithm: EncryptionCipher,
    encrypting_key: &[u8],
) -> Result<Box<dyn EncryptingAlgo>, CryptoError> {
    if !ENCRYPTING_ALGOS.contains(&encrypting_algorithm) {
        return Err(CryptoError::UnsupportedEncryptionAlgorithm(
            encrypting_algorithm,
        ));
    }
    if cfg!(feature = "__debug-dump-keys") {
        log::debug!(
            "Using encryption algorithm {:?} with key {:02x?}",
            encrypting_algorithm,
            encrypting_key
        );
    }
    match encrypting_algorithm {
        #[cfg(feature = "encrypt_aes128ccm")]
        EncryptionCipher::Aes128Ccm => Ok(super::encrypt_ccm::Aes128CcmEncryptor::build(
            encrypting_key,
        )?),
        #[cfg(feature = "encrypt_aes256ccm")]
        EncryptionCipher::Aes256Ccm => Ok(super::encrypt_ccm::Aes256CcmEncryptor::build(
            encrypting_key,
        )?),
        #[cfg(feature = "encrypt_aes128gcm")]
        EncryptionCipher::Aes128Gcm => Ok(super::encrypt_gcm::Aes128GcmEncryptor::build(
            encrypting_key,
        )?),
        #[cfg(feature = "encrypt_aes256gcm")]
        EncryptionCipher::Aes256Gcm => Ok(super::encrypt_gcm::Aes256GcmEncryptor::build(
            encrypting_key,
        )?),
        #[allow(unreachable_patterns)]
        _ => unreachable!("No algorithms should reach the disabled module instead."),
    }
}
