//! Placeholder module to be used when all encryption features are disabled.
use super::*;
use crate::crypto::CryptoError;
use smb_msg::EncryptionCipher;

pub const ENCRYPTING_ALGOS: &[EncryptionCipher] = &[];

pub fn make_encrypting_algo(
    encrypting_algorithm: EncryptionCipher,
    _: &[u8],
) -> Result<Box<dyn EncryptingAlgo>, CryptoError> {
    Err(CryptoError::UnsupportedEncryptionAlgorithm(
        encrypting_algorithm,
    ))
}
