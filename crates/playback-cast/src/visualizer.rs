use std::io::{Read, Seek, SeekFrom, Write};

use audio_processing::{VISUALIZER_BAND_COUNT, VisualizerFrame};

const RECORD_BYTES: u64 = 8 + VISUALIZER_BAND_COUNT as u64 * 4;

pub(crate) struct VisualizerHistory {
    file: tempfile::NamedTempFile,
    frames: u64,
}

impl VisualizerHistory {
    pub(crate) fn new() -> std::io::Result<Self> {
        Ok(Self {
            file: tempfile::NamedTempFile::new()?,
            frames: 0,
        })
    }

    pub(crate) fn append(&mut self, frames: Vec<VisualizerFrame>) -> std::io::Result<()> {
        if frames.is_empty() {
            return Ok(());
        }
        self.file.seek(SeekFrom::End(0))?;
        for frame in frames {
            debug_assert_eq!(frame.levels.len(), VISUALIZER_BAND_COUNT);
            let mut bytes = [0_u8; RECORD_BYTES as usize];
            bytes[..8].copy_from_slice(&frame.position_millis.to_le_bytes());
            for (bytes, level) in bytes[8..].chunks_exact_mut(4).zip(frame.levels) {
                bytes.copy_from_slice(&(level as f32).to_le_bytes());
            }
            self.file.write_all(&bytes)?;
            self.frames += 1;
        }
        Ok(())
    }

    pub(crate) fn at(&mut self, position_millis: u64) -> std::io::Result<Option<Vec<f64>>> {
        let mut lower = 0;
        let mut upper = self.frames;
        let mut timestamp = [0_u8; 8];
        while lower < upper {
            let middle = lower + (upper - lower) / 2;
            self.file.seek(SeekFrom::Start(middle * RECORD_BYTES))?;
            self.file.read_exact(&mut timestamp)?;
            if u64::from_le_bytes(timestamp) <= position_millis {
                lower = middle + 1;
            } else {
                upper = middle;
            }
        }
        if lower == 0 {
            return Ok(None);
        }
        self.file
            .seek(SeekFrom::Start((lower - 1) * RECORD_BYTES + 8))?;
        let mut bytes = [0_u8; VISUALIZER_BAND_COUNT * 4];
        self.file.read_exact(&mut bytes)?;
        Ok(Some(
            bytes
                .chunks_exact(4)
                .map(|value| f64::from(f32::from_le_bytes(value.try_into().unwrap())))
                .collect(),
        ))
    }
}
