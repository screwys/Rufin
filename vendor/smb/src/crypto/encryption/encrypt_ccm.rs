#[cfg(feature = "encrypt_aes128ccm")]
use aes::Aes128;
#[cfg(feature = "encrypt_aes256ccm")]
use aes::Aes256;
use aes::cipher::{BlockCipherDecrypt, BlockCipherEncrypt, BlockSizeUser};
use ccm::{
    Ccm, KeyInit, KeySizeUser,
    aead::AeadInOut,
    consts::{U11, U16},
};

use crate::crypto::CryptoError;

use super::*;

pub type Aes128CcmEncryptor = CcmEncryptor<Aes128>;
pub type Aes256CcmEncryptor = CcmEncryptor<Aes256>;

#[derive(Clone)]
pub struct CcmEncryptor<C>
where
    C: BlockCipherEncrypt + BlockCipherDecrypt + BlockSizeUser<BlockSize = U16>,
{
    cipher: Ccm<C, U16, U11>,
}

#[cfg(any(feature = "encrypt_aes128ccm", feature = "encrypt_aes256ccm"))]
impl<C> CcmEncryptor<C>
where
    C: BlockCipherEncrypt
        + BlockCipherDecrypt
        + KeySizeUser
        + BlockSizeUser<BlockSize = U16>
        + KeyInit
        + Send
        + Clone
        + Sync
        + 'static,
{
    pub fn build(encrypting_key: &[u8]) -> Result<Box<dyn EncryptingAlgo>, CryptoError> {
        Ok(Box::new(Self {
            cipher: Ccm::<C, U16, U11>::new_from_slice(encrypting_key)?,
        }))
    }
}

impl<C> EncryptingAlgo for CcmEncryptor<C>
where
    C: BlockCipherEncrypt
        + BlockCipherDecrypt
        + BlockSizeUser<BlockSize = U16>
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
        let signature = self
            .cipher
            .encrypt_inout_detached(&nonce, header_data, payload.into())?;

        Ok(EncryptionResult {
            signature: u128::from_le_bytes(signature.into()),
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
        11
    }

    fn clone_box(&self) -> Box<dyn EncryptingAlgo> {
        Box::new(self.clone())
    }
}

impl<C> std::fmt::Debug for CcmEncryptor<C>
where
    C: BlockCipherEncrypt + BlockSizeUser<BlockSize = U16> + BlockCipherDecrypt,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Ccm128Encrypter")
    }
}
