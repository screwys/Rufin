mod encryption;
mod kbkdf;
mod signing;

pub use encryption::{ENCRYPTING_ALGOS, EncryptingAlgo, make_encrypting_algo};
pub use kbkdf::{DerivedKey, KeyToDerive, kbkdf_hmacsha256};
pub use signing::{SIGNING_ALGOS, SigningAlgo, make_signing_algo};

use crypto_common::InvalidLength;
use thiserror::Error;

use smb_msg::{EncryptionCipher, SigningAlgorithmId};

#[derive(Debug, Error)]
pub enum CryptoError {
    #[error("Invalid length")]
    InvalidLength(#[from] InvalidLength),
    #[error("Unsupported encryption algorithm {0:?}")]
    UnsupportedEncryptionAlgorithm(EncryptionCipher),
    #[error("Unsupported signing algorithm")]
    UnsupportedSigningAlgorithm(SigningAlgorithmId),
    #[cfg(any(
        feature = "encrypt_aes128ccm",
        feature = "encrypt_aes256ccm",
        feature = "encrypt_aes128gcm",
        feature = "encrypt_aes256gcm"
    ))]
    #[error("AEAD calculation error")]
    AeadError(#[from] aead::Error),
}
