use sha2::{Digest, Sha512};

use smb_msg::HashAlgorithm;
use smb_transport::IoVec;

pub type PreauthHashValue = [u8; 64];

pub const SUPPORTED_ALGOS: &[HashAlgorithm] = &[HashAlgorithm::Sha512];

#[derive(Debug, Clone)]
pub enum PreauthHashState {
    /// This state always transitions to itself, and calling `unwrap_final_hash` returns `None`.
    Unsupported,

    InProgress(PreauthHashValue),
    Finished(PreauthHashValue),
}

impl PreauthHashState {
    pub fn begin() -> PreauthHashState {
        PreauthHashState::InProgress([0; 64])
    }

    pub fn unsupported() -> PreauthHashState {
        PreauthHashState::Unsupported
    }

    pub fn next(self, data: &IoVec) -> PreauthHashState {
        match self {
            PreauthHashState::InProgress(hash) => {
                let mut hasher = Sha512::new();
                hasher.update(hash);

                for data in data.iter() {
                    hasher.update(data.as_ref());
                }

                PreauthHashState::InProgress(hasher.finalize().into())
            }
            PreauthHashState::Unsupported => PreauthHashState::Unsupported,
            _ => panic!("Preauth hash not started/already finished."),
        }
    }

    pub fn finish(self) -> PreauthHashState {
        match self {
            PreauthHashState::InProgress(hash) => PreauthHashState::Finished(hash),
            PreauthHashState::Unsupported => PreauthHashState::Unsupported,
            _ => panic!("Preauth hash not started"),
        }
    }

    pub fn unwrap_final_hash(&self) -> Option<&PreauthHashValue> {
        match self {
            PreauthHashState::Finished(hash) => Some(hash),
            PreauthHashState::Unsupported => None,
            _ => panic!("Preauth hash not finished"),
        }
    }
}
