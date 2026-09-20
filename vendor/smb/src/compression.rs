//! Implements (de)compression logic.
use binrw::prelude::*;
#[cfg(feature = "compress_lz4")]
use lz4_flex;
use smb_msg::*;
#[cfg(feature = "compress_pattern_v1")]
use std::io::Cursor;
use thiserror::Error;

/// Use this struct to decompress a compressed, received message.
#[derive(Debug)]
pub struct Decompressor {
    caps: CompressionCapabilities,
}

impl Decompressor {
    pub fn new(caps: &CompressionCapabilities) -> Decompressor {
        Decompressor { caps: caps.clone() }
    }

    pub fn decompress(
        &self,
        original: &CompressedMessage,
    ) -> Result<(Response, Vec<u8>), CompressionError> {
        let method: Box<dyn CompressionMethod> = match original {
            CompressedMessage::Unchained(_) => Box::new(UnchainedCompression),
            CompressedMessage::Chained(_) => {
                if self.caps.flags.chained() {
                    Box::new(ChainedCompression)
                } else {
                    return Err(CompressionError::UnsupportedCompressionMethod);
                }
            }
        };
        let bytes = method.decompress(original)?;
        let mut cursor = std::io::Cursor::new(&bytes);
        Ok((
            Response::read(&mut cursor)
                .map_err(|_| CompressionError::InvalidDecompressedMessage)?,
            bytes,
        ))
    }
}

#[derive(Debug)]
pub struct Compressor {
    caps: CompressionCapabilities,
}

impl Compressor {
    pub fn new(caps: &CompressionCapabilities) -> Compressor {
        Compressor { caps: caps.clone() }
    }

    pub fn compress(&self, bytes: &[u8]) -> crate::Result<CompressedMessage> {
        // TODO: Add Chained.
        Ok(UnchainedCompression.compress(bytes, &self.caps.compression_algorithms)?)
    }
}

/// This trait describes a (de)compression method, not a specific algorithm.
///
/// A method can be chained or unchained, and this makes an easy abstraction for the decompression logic.
/// See [self::UnchainedCompression] and [self::ChainedCompression] for the actual implementations.
/// See algorithms implemented by using the trait [self::CompressionAlgorithmImpl].
trait CompressionMethod {
    fn decompress(&self, compressed: &CompressedMessage) -> Result<Vec<u8>, CompressionError>;

    fn compress(
        &self,
        data: &[u8],
        algorithms: &[CompressionAlgorithm],
    ) -> Result<CompressedMessage, CompressionError>;

    fn get_compression_algorithm(
        &self,
        algo: CompressionAlgorithm,
    ) -> Result<Box<dyn CompressionAlgorithmImpl>, CompressionError> {
        Ok(match algo {
            CompressionAlgorithm::None => Box::new(NoneCompression),
            #[cfg(feature = "compress_pattern_v1")]
            CompressionAlgorithm::PatternV1 => Box::new(PatternV1Compression),
            #[cfg(feature = "compress_lz4")]
            CompressionAlgorithm::LZ4 => Box::new(Lz4Compression),
            _ => Err(CompressionError::UnsupportedAlgorithm(algo))?,
        })
    }
}

struct UnchainedCompression;

impl UnchainedCompression {
    pub const ALGORITHM_PRIORITY: [CompressionAlgorithm; 1] = [CompressionAlgorithm::LZ4];
}

impl CompressionMethod for UnchainedCompression {
    fn decompress(&self, compressed: &CompressedMessage) -> Result<Vec<u8>, CompressionError> {
        let compressed = match compressed {
            CompressedMessage::Unchained(c) => c,
            _ => panic!("Expected Unchained message"),
        };
        let mut data: Vec<u8> = Vec::<u8>::with_capacity(compressed.original_size as usize);
        self.get_compression_algorithm(compressed.compression_algorithm)?
            .decompress(&compressed.data, Some(compressed.original_size), &mut data)?;
        Ok(data)
    }

    fn compress(
        &self,
        data: &[u8],
        allowed_algorithms: &[CompressionAlgorithm],
    ) -> Result<CompressedMessage, CompressionError> {
        // Check what algos are supported.
        for algo in Self::ALGORITHM_PRIORITY.iter() {
            if !allowed_algorithms.contains(algo) {
                continue;
            }

            let algo_impl = self.get_compression_algorithm(*algo)?;
            let compressed = algo_impl.compress(data)?;
            return Ok(CompressedMessage::Unchained(CompressedUnchainedMessage {
                compression_algorithm: *algo,
                data: compressed,
                original_size: data.len() as u32,
            }));
        }

        Err(CompressionError::NoSupportedCompressionAlgorithm)
    }
}

struct ChainedCompression;

impl CompressionMethod for ChainedCompression {
    fn decompress(&self, compressed: &CompressedMessage) -> Result<Vec<u8>, CompressionError> {
        let compressed = match compressed {
            CompressedMessage::Chained(c) => c,
            _ => panic!("Expected Chained message"),
        };

        if compressed.original_size < Header::STRUCT_SIZE as u32 {
            Err(CompressionError::InvalidCompressedMessage)?;
        }

        // TODO: There should be a safer way to implement an append-only,
        // size-limited vector.
        let mut data = Vec::with_capacity(compressed.original_size as usize);

        for item in compressed.items.iter() {
            let len_before = data.len();
            self.get_compression_algorithm(item.compression_algorithm)?
                .decompress(&item.payload_data, item.original_size, &mut data)?;
            let len_after = data.len();
            if len_after > compressed.original_size as usize {
                return Err(CompressionError::ChainedCompressionFailed(
                    "Decompressed size exceeds the expected size".to_string(),
                ))?;
            }
            if let Some(original_size) = item.original_size
                && len_after - len_before != original_size as usize
            {
                Err(CompressionError::ChainedCompressionFailed(
                    "Decompressed size does not match the item expected size".to_string(),
                ))?;
            }
        }

        if data.len() != compressed.original_size as usize {
            Err(CompressionError::ChainedCompressionFailed(
                "Decompressed size does not match the expected size".to_string(),
            ))?;
        }

        Ok(data)
    }

    fn compress(
        &self,
        _data: &[u8],
        _algorithms: &[CompressionAlgorithm],
    ) -> Result<CompressedMessage, CompressionError> {
        todo!()
    }
}

/// This trait describes a compression algorithm -- an algorithm that takes a chunk of memory and compresses or decompresses it.
trait CompressionAlgorithmImpl {
    /// Decompress the compressed data into the output buffer.
    ///
    /// The original size is optional, and is only used for chained decompression.
    ///
    fn decompress(
        &self,
        compressed: &[u8],
        original_size: Option<u32>,
        out: &mut Vec<u8>,
    ) -> Result<(), CompressionError>;

    /// Compress the data into a new buffer.
    fn compress(&self, data: &[u8]) -> Result<Vec<u8>, CompressionError>;
}

pub const SUPPORTED_ALGORITHMS: &[CompressionAlgorithm] = &[
    CompressionAlgorithm::None,
    #[cfg(feature = "compress_pattern_v1")]
    CompressionAlgorithm::PatternV1,
    #[cfg(feature = "compress_lz4")]
    CompressionAlgorithm::LZ4,
];

struct NoneCompression;

impl CompressionAlgorithmImpl for NoneCompression {
    fn decompress(
        &self,
        compressed: &[u8],
        original_size: Option<u32>,
        out: &mut Vec<u8>,
    ) -> Result<(), CompressionError> {
        debug_assert!(original_size.is_none());

        out.extend_from_slice(compressed);
        Ok(())
    }

    fn compress(&self, data: &[u8]) -> Result<Vec<u8>, CompressionError> {
        Ok(data.to_owned())
    }
}

#[cfg(feature = "compress_pattern_v1")]
struct PatternV1Compression;

#[cfg(feature = "compress_pattern_v1")]
#[binrw::binrw]
#[derive(Debug, PartialEq, Eq)]
#[brw(little)]
pub struct PatternV1Payload {
    pattern: u8,
    #[bw(calc = 0)]
    #[br(temp)]
    _reserved1: u8,
    #[bw(calc = 0)]
    #[br(temp)]
    _reserved2: u16,
    repetitions: u32,
}

#[cfg(feature = "compress_pattern_v1")]
impl CompressionAlgorithmImpl for PatternV1Compression {
    fn decompress(
        &self,
        compressed: &[u8],
        original_size: Option<u32>,
        out: &mut Vec<u8>,
    ) -> Result<(), CompressionError> {
        debug_assert!(original_size.is_none());
        assert!(compressed.len() == 8);
        let mut cursor = Cursor::new(&compressed);

        let parsed_payload = match PatternV1Payload::read(&mut cursor) {
            Ok(p) => p,
            Err(e) => return Err(CompressionError::PatternV1InvalidPayload(e)),
        };
        out.extend(std::iter::repeat_n(
            parsed_payload.pattern,
            parsed_payload.repetitions as usize,
        ));

        Ok(())
    }

    fn compress(&self, _data: &[u8]) -> Result<Vec<u8>, CompressionError> {
        todo!()
    }
}

#[cfg(feature = "compress_lz4")]
struct Lz4Compression;

#[cfg(feature = "compress_lz4")]
impl CompressionAlgorithmImpl for Lz4Compression {
    fn decompress(
        &self,
        compressed: &[u8],
        original_size: Option<u32>,
        out: &mut Vec<u8>,
    ) -> Result<(), CompressionError> {
        let start_index = out.len();
        out.resize(start_index + original_size.unwrap() as usize, 0);

        let size = lz4_flex::decompress_into(compressed, &mut out[start_index..])?;

        if size != original_size.unwrap() as usize {
            Err(CompressionError::PatternV1InvalidDecompressedSize)?;
        }
        Ok(())
    }

    fn compress(&self, data: &[u8]) -> Result<Vec<u8>, CompressionError> {
        Ok(lz4_flex::compress(data))
    }
}

#[derive(Error, Debug)]
pub enum CompressionError {
    // --- General
    #[error("Unsupported compression algorithm {0}")]
    UnsupportedAlgorithm(CompressionAlgorithm),
    #[error("Decompressed message is invalid")]
    InvalidDecompressedMessage,
    #[error("Invalid compressed message")]
    InvalidCompressedMessage,
    #[error("Chained compression error: {0}")]
    ChainedCompressionFailed(String),
    #[error("Unsupported compression method for message.")]
    UnsupportedCompressionMethod,
    #[error("There is no supported compression algorithm available.")]
    NoSupportedCompressionAlgorithm,

    // --- LZ4
    #[cfg(feature = "compress_lz4")]
    #[error("LZ4 compression error: {0}")]
    Lz4CompressError(#[from] lz4_flex::block::CompressError),
    #[cfg(feature = "compress_lz4")]
    #[error("LZ4 decompression error: {0}")]
    Lz4DecompressError(#[from] lz4_flex::block::DecompressError),

    // --- PatternV1
    #[cfg(feature = "compress_pattern_v1")]
    #[error("PatternV1 invalid compressed payload")]
    PatternV1InvalidPayload(binrw::Error),
    #[cfg(feature = "compress_pattern_v1")]
    #[error("PatternV1 invalid decompressed size")]
    PatternV1InvalidDecompressedSize,
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    pub fn test_none_algorithm_decompression() {
        let compressed = vec![1, 2, 3, 4, 5];
        let mut out = vec![];
        super::NoneCompression
            .decompress(&compressed, None, &mut out)
            .unwrap();
        assert_eq!(compressed, out);
    }

    #[cfg(feature = "compress_pattern_v1")]
    #[test]
    pub fn test_pattern_v1_algorithm_decompression() {
        let pattern_v1_payload_buffer = vec!['h' as u8, 0x0, 0x0, 0x0, 0xee, 0x1, 0x0, 0x0];
        let mut out = vec![];
        super::PatternV1Compression
            .decompress(&pattern_v1_payload_buffer, None, &mut out)
            .unwrap();
    }

    #[cfg(feature = "compress_pattern_v1")]
    #[test]
    pub fn test_chained_decompression() {
        let parsed_message = CompressedMessage::Chained(CompressedChainedMessage {
            original_size: 1104,
            items: vec![
                CompressedChainedItem {
                    compression_algorithm: CompressionAlgorithm::None,
                    flags: 1,
                    original_size: None,
                    payload_data: vec![
                        0xfe, 0x53, 0x4d, 0x42, 0x40, 0x0, 0x1, 0x0, 0x0, 0x0, 0x0, 0x0, 0x8, 0x0,
                        0x1, 0x0, 0x19, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x7, 0x0, 0x0, 0x0, 0x0,
                        0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x1, 0x0, 0x0, 0x0, 0x51, 0x0, 0x0, 0x0,
                        0x0, 0x34, 0x0, 0x0, 0xa, 0x9b, 0xe1, 0x41, 0x4b, 0x98, 0x8c, 0xf0, 0xd4,
                        0xcd, 0x0, 0xa3, 0xfa, 0x8a, 0x7c, 0x64, 0x11, 0x0, 0x50, 0x0, 0x0, 0x4,
                        0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0,
                    ],
                },
                CompressedChainedItem {
                    compression_algorithm: CompressionAlgorithm::PatternV1,
                    flags: 0,
                    original_size: None,
                    payload_data: vec![0x64, 0x0, 0x0, 0x0, 0x0, 0x4, 0x0, 0x0],
                },
            ],
        });

        let decompressor = Decompressor::new(&CompressionCapabilities {
            flags: CompressionCapsFlags::new().with_chained(true),
            compression_algorithms: vec![
                CompressionAlgorithm::None,
                CompressionAlgorithm::PatternV1,
            ],
        });
        let (dmsg, draw) = decompressor.decompress(&parsed_message).unwrap();
        assert_eq!(
            draw[..80],
            vec![
                254, 83, 77, 66, 64, 0, 1, 0, 0, 0, 0, 0, 8, 0, 1, 0, 25, 0, 0, 0, 0, 0, 0, 0, 7,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 81, 0, 0, 0, 0, 52, 0, 0, 10, 155,
                225, 65, 75, 152, 140, 240, 212, 205, 0, 163, 250, 138, 124, 100, 17, 0, 80, 0, 0,
                4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0
            ]
        );
        assert_eq!(draw[80..], vec![0x64; 0x400]);
        // This is a compressed read response, let's unwrap it.
        let plain_unwrapped = match dmsg {
            Response::Plain(p) => p,
            _ => panic!("Expected plain message"),
        };
        // Validate header
        assert_eq!(
            plain_unwrapped.header,
            Header {
                credit_charge: 1,
                status: Status::Success as u32,
                command: Command::Read,
                credit_request: 1,
                flags: HeaderFlags::new()
                    .with_server_to_redir(true)
                    .with_signed(true)
                    .with_priority_mask(1),
                next_command: 0,
                message_id: 7,
                tree_id: Some(1),
                async_id: None,
                session_id: 0x340000000051,
                signature: 133569463218962867026972765300193336074
            }
        );
        // unwrap & validate read response.
        let read_response = plain_unwrapped.content.to_read().unwrap();
        assert_eq!(
            read_response,
            ReadResponse {
                buffer: vec![0x64; 0x400]
            }
        )
    }
}
