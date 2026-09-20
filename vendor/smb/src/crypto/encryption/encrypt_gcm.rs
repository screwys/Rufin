use aes::cipher::{BlockCipherDecrypt, BlockCipherEncrypt};
use aes_gcm::{AesGcm, KeyInit, KeySizeUser, aead::AeadInOut};
use crypto_common::typenum;

use crate::crypto::CryptoError;

use super::*;

pub type Aes128GcmEncryptor = AesGcmEncryptor<aes::Aes128>;
pub type Aes256GcmEncryptor = AesGcmEncryptor<aes::Aes256>;

#[derive(Clone)]
pub struct AesGcmEncryptor<T> {
    cipher: AesGcm<T, typenum::U12>,
}

impl<T> AesGcmEncryptor<T>
where
    T: BlockCipherEncrypt<BlockSize = typenum::U16>
        + BlockCipherDecrypt<BlockSize = typenum::U16>
        + KeySizeUser
        + KeyInit
        + Send
        + Clone
        + Sync
        + 'static,
{
    pub fn build(encrypting_key: &[u8]) -> Result<Box<dyn EncryptingAlgo>, CryptoError> {
        Ok(Box::new(Self {
            cipher: AesGcm::<T, typenum::U12>::new_from_slice(encrypting_key)?,
        }))
    }
}

impl<T> EncryptingAlgo for AesGcmEncryptor<T>
where
    T: BlockCipherEncrypt<BlockSize = typenum::U16>
        + BlockCipherDecrypt
        + KeyInit
        + KeySizeUser
        + Send
        + Clone
        + Sync
        + 'static,
{
    fn encrypt(
        &mut self,
        payload: &mut [u8],
        header_data: &[u8],
        nonce: &EncryptionNonce,
    ) -> Result<EncryptionResult, CryptoError> {
        let nonce = trim_nonce(self, nonce);
        let tag = self
            .cipher
            .encrypt_inout_detached(&nonce, header_data, payload.into())?;
        Ok(EncryptionResult {
            signature: u128::from_le_bytes(tag.into()),
        })
    }

    fn decrypt(
        &mut self,
        payload: &mut [u8],
        header_data: &[u8],
        nonce: &EncryptionNonce,
        signature: u128,
    ) -> Result<(), CryptoError> {
        let nonce = trim_nonce(self, nonce);
        self.cipher.decrypt_inout_detached(
            &nonce,
            header_data,
            payload.into(),
            &signature.to_le_bytes().into(),
        )?;
        Ok(())
    }

    fn nonce_size(&self) -> usize {
        12
    }

    fn clone_box(&self) -> Box<dyn EncryptingAlgo> {
        Box::new(self.clone())
    }
}

impl<T> std::fmt::Debug for AesGcmEncryptor<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AesGcmEncrypter")
    }
}
