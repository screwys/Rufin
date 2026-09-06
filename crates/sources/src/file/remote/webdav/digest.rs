//! Digest auth-int for uploads without retaining the complete file in memory.

use std::io::Read;
use std::path::Path;

use digest_auth::{AlgorithmType, AuthorizationHeader};
use sha2::{Digest, digest::DynDigest};

use crate::{SourceError, SourceResult};

pub(crate) async fn file_response(
    header: &mut AuthorizationHeader,
    method: &reqwest::Method,
    username: &str,
    password: &str,
    path: &Path,
) -> SourceResult<()> {
    let algorithm = header.algorithm;
    let path = path.to_path_buf();
    let entity_hash = tokio::task::spawn_blocking(move || {
        let mut digest: Box<dyn DynDigest> = match algorithm.algo {
            AlgorithmType::MD5 => Box::new(md5_digest::Md5::new()),
            AlgorithmType::SHA2_256 => Box::new(sha2::Sha256::new()),
            AlgorithmType::SHA2_512_256 => Box::new(sha2::Sha512_256::new()),
        };
        let mut file = std::fs::File::open(path)?;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            digest.update(&buffer[..count]);
        }
        Ok::<_, std::io::Error>(
            digest
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>(),
        )
    })
    .await
    .map_err(|e| SourceError::Other(e.to_string()))?
    .map_err(|e| SourceError::Other(e.to_string()))?;

    let cnonce = header.cnonce.as_deref().expect("auth-int client nonce");
    let mut ha1 = algorithm.hash_str(&format!("{username}:{}:{password}", header.realm));
    if algorithm.sess {
        ha1 = algorithm.hash_str(&format!("{ha1}:{}:{cnonce}", header.nonce));
    }
    let ha2 = algorithm.hash_str(&format!("{method}:{}:{entity_hash}", header.uri));
    header.response = algorithm.hash_str(&format!(
        "{ha1}:{}:{:08x}:{cnonce}:auth-int:{ha2}",
        header.nonce, header.nc
    ));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn streamed_upload_matches_digest_auth_entity_calculation() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let bytes: Vec<u8> = (0..150_000).map(|index| (index % 251) as u8).collect();
        std::fs::write(file.path(), &bytes).unwrap();
        for algorithm in [
            "MD5",
            "MD5-sess",
            "SHA-256",
            "SHA-256-sess",
            "SHA-512-256",
            "SHA-512-256-sess",
        ] {
            let challenge = format!(
                "Digest realm=\"music\", nonce=\"server-nonce\", qop=\"auth-int\", algorithm={algorithm}"
            );
            let mut context = digest_auth::AuthContext::new_with_method(
                "user",
                "password",
                "/music/a%20b.flac",
                Some(&bytes),
                digest_auth::HttpMethod::PUT,
            );
            context.set_custom_cnonce("client-nonce");
            let expected = digest_auth::parse(&challenge)
                .unwrap()
                .respond(&context)
                .unwrap();
            context.body = Some((&[][..]).into());
            let mut streamed = digest_auth::parse(&challenge)
                .unwrap()
                .respond(&context)
                .unwrap();
            file_response(
                &mut streamed,
                &reqwest::Method::PUT,
                "user",
                "password",
                file.path(),
            )
            .await
            .unwrap();
            assert_eq!(streamed.to_string(), expected.to_string(), "{algorithm}");
        }
    }
}
