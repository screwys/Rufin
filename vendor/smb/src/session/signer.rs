//! Message signing implementation.

use binrw::prelude::*;
use std::io::Cursor;

use crate::{Error, crypto};
use smb_msg::Header;
use smb_transport::IoVec;

/// A struct for writing and verifying SMB message signatures.
///
/// This struct is NOT thread-safe, use clones for concurrent access.
#[derive(Debug)]
pub struct MessageSigner {
    signing_algo: Box<dyn crypto::SigningAlgo>,
}

impl MessageSigner {
    pub fn new(signing_algo: Box<dyn crypto::SigningAlgo>) -> MessageSigner {
        MessageSigner { signing_algo }
    }

    /// Verifies the signature of a message.
    ///
    /// This function assumes that the provided raw_data contains the plain message header at the beginning of the first buffer.
    pub fn verify_signature(&mut self, header: &mut Header, data: &IoVec) -> crate::Result<()> {
        let calculated_signature = self._calculate_signature(header, data)?;
        if calculated_signature != header.signature {
            return Err(Error::SignatureVerificationFailed);
        }
        Ok(())
    }

    /// Signs a message.
    ///
    /// This function assumes that the provided iovec contains the plain message header at the beginning of the first buffer.
    pub fn sign_message(&mut self, header: &mut Header, all_data: &mut IoVec) -> crate::Result<()> {
        header.signature = self._calculate_signature(header, all_data)?;

        // Update raw data to include the signature.
        let header_buffer = all_data.get_mut(0).unwrap();
        debug_assert!(
            header_buffer.len() >= Header::STRUCT_SIZE,
            "First buffer must contain the entire header."
        );
        let mut header_writer = Cursor::new(&mut header_buffer[0..Header::STRUCT_SIZE]);
        header.write(&mut header_writer)?;
        Ok(())
    }

    fn _calculate_signature(&mut self, header: &mut Header, data: &IoVec) -> crate::Result<u128> {
        // Write header with signature set to 0.
        let signature_backup = header.signature;
        header.signature = 0;
        let mut header_bytes = Cursor::new([0; Header::STRUCT_SIZE]);
        header.write(&mut header_bytes)?;
        header.signature = signature_backup;

        // Start signing session with the header.
        self.signing_algo.start(header);
        self.signing_algo.update(&header_bytes.into_inner());

        if data.first().unwrap().len() >= Header::STRUCT_SIZE {
            // If the first buffer is larger than the header, we need to skip the header part.
            self.signing_algo
                .update(&data.first().unwrap()[Header::STRUCT_SIZE..]);
        }

        // It is assumed here, too, that the first buffer is the header.
        for buf in data.iter().skip(1) {
            self.signing_algo.update(buf);
        }

        Ok(self.signing_algo.finalize())
    }
}

impl Clone for MessageSigner {
    fn clone(&self) -> Self {
        MessageSigner {
            signing_algo: self.signing_algo.clone_box(),
        }
    }
}

#[cfg(all(test, feature = "sign_gmac"))]
mod tests {
    use crate::crypto::make_signing_algo;

    use super::*;

    const TEST_SIGNING_KEY: [u8; 16] = [
        0xAC, 0x36, 0xE9, 0x54, 0x3C, 0xD8, 0x88, 0xF0, 0xA8, 0x41, 0x23, 0xE4, 0x6B, 0xB2, 0xA0,
        0xD7,
    ];

    #[test]
    #[cfg(feature = "sign_gmac")]
    fn test_calc_signature() {
        // Some random session logoff request for testing.

        use smb_msg::SigningAlgorithmId;
        use smb_transport::{IoVec, IoVecBuf};

        let header_data = vec![
            0xfeu8, 0x53, 0x4d, 0x42, 0x40, 0x0, 0x1, 0x0, 0x0, 0x0, 0x0, 0x0, 0x2, 0x0, 0x1, 0x0,
            0x18, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x9, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0,
            0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x53, 0x20, 0xc, 0x21, 0x0, 0x0, 0x0, 0x0, 0x76,
            0x23, 0x4b, 0x3c, 0x81, 0x2f, 0x51, 0xab, 0x8a, 0x5c, 0xf9, 0xfa, 0x43, 0xd4, 0xeb,
            0x28,
        ];
        let next_data = vec![0x4, 0x0, 0x0, 0x0];
        let mut header = Header::read_le(&mut Cursor::new(
            &header_data.as_slice()[..Header::STRUCT_SIZE],
        ))
        .unwrap();

        let mut signer = MessageSigner::new(
            make_signing_algo(SigningAlgorithmId::AesGmac, &TEST_SIGNING_KEY).unwrap(),
        );

        let iovec = IoVec::from(vec![IoVecBuf::from(header_data), IoVecBuf::from(next_data)]);
        let signature = signer._calculate_signature(&mut header, &iovec).unwrap();
        assert_eq!(signature, 0x28ebd443faf95c8aab512f813c4b2376);
    }
}
