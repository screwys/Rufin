//! Bounded spectrum analysis.

use gst::prelude::*;
use gstreamer as gst;
use gstreamer_audio as gst_audio;
use rustfft::{Fft, FftPlanner, num_complex::Complex};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

const VISUALIZER_FFT_SIZE: usize = 2_048;
pub const VISUALIZER_BAND_COUNT: usize = 256;
const VISUALIZER_MIN_FREQUENCY_HZ: f64 = 20.0;
const VISUALIZER_MAX_FREQUENCY_HZ: f64 = 20_000.0;
const VISUALIZER_MIN_EMIT_INTERVAL: Duration = Duration::from_millis(33);

pub struct VisualizerFft {
    samples: VecDeque<f32>,
    input: Vec<Complex<f32>>,
    window: Vec<f32>,
    fft: Arc<dyn Fft<f32>>,
    last_emit: Option<Instant>,
    band_sample_rate: u32,
    band_ranges: Vec<(usize, usize)>,
}
impl VisualizerFft {
    pub fn new() -> Self {
        Self::with_size(VISUALIZER_FFT_SIZE)
    }

    pub fn with_size(size: usize) -> Self {
        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(size);
        let window = (0..size)
            .map(|index| {
                let position = index as f32 / (size - 1) as f32;
                0.5 - 0.5 * (2.0 * std::f32::consts::PI * position).cos()
            })
            .collect();
        Self {
            samples: VecDeque::with_capacity(size),
            input: vec![Complex::new(0.0, 0.0); size],
            window,
            fft,
            last_emit: None,
            band_sample_rate: 0,
            band_ranges: Vec::new(),
        }
    }

    pub fn set_size(&mut self, size: usize) {
        if self.input.len() != size {
            *self = Self::with_size(size);
        }
    }

    pub fn clear(&mut self) {
        self.samples.clear();
        self.last_emit = None;
    }

    pub fn push_samples(&mut self, samples: &[f32]) {
        let start = samples.len().saturating_sub(self.input.len());
        for sample in &samples[start..] {
            if self.samples.len() == self.input.len() {
                self.samples.pop_front();
            }
            self.samples.push_back(*sample);
        }
    }

    pub fn push_channel_samples(&mut self, samples: &[f32], channels: usize, channel: usize) {
        for frame in samples.chunks_exact(channels) {
            if self.samples.len() == self.input.len() {
                self.samples.pop_front();
            }
            self.samples.push_back(frame[channel]);
        }
    }

    /// One-sided power at each FFT frequency, normalized by the window's coherent gain.
    pub fn spectrum_power(&mut self) -> Option<Vec<f64>> {
        if self.samples.len() < self.input.len() {
            return None;
        }
        self.transform();
        let coherent_gain = self.window.iter().map(|v| f64::from(*v)).sum::<f64>();
        Some(
            self.input[..=self.input.len() / 2]
                .iter()
                .enumerate()
                .map(|(i, value)| {
                    let factor = if i == 0 || i == self.input.len() / 2 {
                        1.0
                    } else {
                        2.0
                    };
                    f64::from(value.norm_sqr()) * (factor / coherent_gain).powi(2)
                })
                .collect(),
        )
    }

    fn transform(&mut self) {
        for ((slot, sample), window) in self
            .input
            .iter_mut()
            .zip(self.samples.iter().copied())
            .zip(self.window.iter().copied())
        {
            *slot = Complex::new(sample * window, 0.0);
        }
        self.fft.process(&mut self.input);
    }

    pub fn maybe_levels(&mut self, sample_rate: u32) -> Option<Vec<f64>> {
        if self.samples.len() < self.input.len() {
            return None;
        }
        let now = Instant::now();
        if self
            .last_emit
            .is_some_and(|last| now.duration_since(last) < VISUALIZER_MIN_EMIT_INTERVAL)
        {
            return None;
        }
        self.last_emit = Some(now);
        self.levels(sample_rate)
    }

    /// Linear amplitudes in canonical ERB bands; bands above Nyquist remain silent.
    pub fn levels(&mut self, sample_rate: u32) -> Option<Vec<f64>> {
        if self.samples.len() < self.input.len() {
            return None;
        }
        self.transform();
        if self.band_sample_rate != sample_rate || self.band_ranges.is_empty() {
            self.band_ranges = visualizer_band_ranges(sample_rate, self.input.len());
            self.band_sample_rate = sample_rate;
        }
        let coherent_gain = self.window.iter().map(|v| f64::from(*v)).sum::<f64>();
        Some(fft_band_levels(
            &self.input,
            &self.band_ranges,
            coherent_gain,
        ))
    }
}

pub fn copy_visualizer_samples(
    pad: &gst::Pad,
    buffer: &gst::Buffer,
    max_frames: usize,
) -> Option<(Vec<f32>, u32)> {
    let caps = pad.current_caps()?;
    let info = gst_audio::AudioInfo::from_caps(caps.as_ref()).ok()?;
    let map = buffer.map_readable().ok()?;
    copy_audio_samples(map.as_slice(), &info, max_frames).map(|samples| (samples, info.rate()))
}
pub fn copy_audio_samples(
    bytes: &[u8],
    info: &gst_audio::AudioInfo,
    max_frames: usize,
) -> Option<Vec<f32>> {
    if !info.is_valid() || info.layout() != gst_audio::AudioLayout::Interleaved {
        return None;
    }
    let channels = info.channels() as usize;
    let frame_size = info.bpf() as usize;
    let frames = (bytes.len() / frame_size).min(max_frames);
    if frames == 0 {
        return None;
    }
    let format = info.format_info();
    let sample_size = if format.is_float() { 8 } else { 4 };
    let mut unpacked = vec![0; frames * channels * sample_size];
    // GStreamer handles integer alignment, signedness and byte order. Its
    // canonical unpacked formats are native-endian S32 and F64.
    format.unpack(
        gst_audio::AudioPackFlags::empty(),
        &mut unpacked,
        &bytes[..frames * frame_size],
    );
    Some(
        unpacked
            .chunks_exact(channels * sample_size)
            .map(|frame| {
                let total: f32 = frame
                    .chunks_exact(sample_size)
                    .map(|sample| {
                        let value = if format.is_float() {
                            f64::from_ne_bytes(sample.try_into().unwrap()) as f32
                        } else {
                            i32::from_ne_bytes(sample.try_into().unwrap()) as f32 / 2_147_483_648.0
                        };
                        if value.is_finite() {
                            value.clamp(-1.0, 1.0)
                        } else {
                            0.0
                        }
                    })
                    .sum();
                total / channels as f32
            })
            .collect(),
    )
}

/// Frequency at an ERB position on the canonical 20 Hz to 20 kHz spectrum.
/// Band `i` has its center at `(i + 0.5) / VISUALIZER_BAND_COUNT`.
pub fn visualizer_frequency(position: f64) -> f64 {
    let minimum = frequency_to_erb(VISUALIZER_MIN_FREQUENCY_HZ);
    let maximum = frequency_to_erb(VISUALIZER_MAX_FREQUENCY_HZ);
    (10.0_f64.powf((minimum + (maximum - minimum) * position.clamp(0.0, 1.0)) / 21.4) - 1.0)
        / 0.00437
}

/// ERB position of a frequency on the canonical 20 Hz to 20 kHz spectrum.
pub fn visualizer_position(frequency: f64) -> f64 {
    let minimum = frequency_to_erb(VISUALIZER_MIN_FREQUENCY_HZ);
    let maximum = frequency_to_erb(VISUALIZER_MAX_FREQUENCY_HZ);
    (frequency_to_erb(frequency.clamp(VISUALIZER_MIN_FREQUENCY_HZ, VISUALIZER_MAX_FREQUENCY_HZ))
        - minimum)
        / (maximum - minimum)
}

fn frequency_to_erb(frequency: f64) -> f64 {
    21.4 * (1.0 + 0.00437 * frequency).log10()
}

fn visualizer_band_ranges(sample_rate: u32, size: usize) -> Vec<(usize, usize)> {
    let half = size / 2;
    let nyquist = f64::from(sample_rate) / 2.0;
    (0..VISUALIZER_BAND_COUNT)
        .map(|band| {
            let lower_hz = visualizer_frequency(band as f64 / VISUALIZER_BAND_COUNT as f64);
            if lower_hz >= nyquist {
                return (0, 0);
            }
            let upper_hz = visualizer_frequency((band + 1) as f64 / VISUALIZER_BAND_COUNT as f64);
            let bins_per_hz = size as f64 / f64::from(sample_rate);
            let lower = ((lower_hz * bins_per_hz).floor() as usize).max(1);
            let upper = ((upper_hz * bins_per_hz).ceil() as usize)
                .max(lower + 1)
                .min(half + 1);
            (lower, upper)
        })
        .collect()
}

fn fft_band_levels(
    input: &[Complex<f32>],
    ranges: &[(usize, usize)],
    coherent_gain: f64,
) -> Vec<f64> {
    ranges
        .iter()
        .map(|&(lower, upper)| {
            if lower == upper {
                return 0.0;
            }
            let mut total = 0.0;
            let mut peak = 0.0_f64;
            for (index, value) in input[lower..upper].iter().enumerate() {
                let factor = if lower + index == input.len() / 2 {
                    1.0
                } else {
                    2.0
                };
                let amplitude = f64::from(value.norm()) * factor / coherent_gain;
                total += amplitude;
                peak = peak.max(amplitude);
            }
            let average = total / (upper - lower) as f64;
            (average * 0.4 + peak * 0.6).clamp(0.0, 1.0)
        })
        .collect()
}

#[cfg(test)]
mod visualizer_tests {
    use super::*;
    #[test]
    fn visualizer_fft_distinguishes_silence_from_bounded_energy() {
        for size in [1_024, 2_048, 4_096, 8_192, 16_384, 32_768] {
            let mut fft = VisualizerFft::new();
            fft.set_size(size);
            fft.push_samples(&vec![0.0; size]);
            let silence = fft.levels(48_000).expect("full FFT window");
            assert_eq!(silence, vec![0.0; VISUALIZER_BAND_COUNT]);

            fft.clear();
            let bin = (1_000.0 * size as f64 / 48_000.0).round() as usize;
            let samples = (0..size)
                .map(|index| {
                    let phase =
                        2.0 * std::f32::consts::PI * bin as f32 * index as f32 / size as f32;
                    phase.sin() * 0.8
                })
                .collect::<Vec<_>>();
            fft.push_samples(&samples);
            let power = fft.spectrum_power().expect("full FFT window");
            assert!((power[bin].sqrt() - 0.8).abs() < 1e-4);
            let levels = fft.levels(48_000).expect("full FFT window");
            let peak = levels.iter().copied().fold(0.0_f64, f64::max);
            assert!((0.48..=0.801).contains(&peak), "{size}: {peak}");
            assert!(levels.iter().all(|level| (0.0..=1.0).contains(level)));
        }
    }

    #[test]
    fn visualizer_bands_follow_frequency_across_sample_rates() {
        let tone = |sample_rate: u32, frequency: f32| {
            let mut fft = VisualizerFft::new();
            let samples = (0..VISUALIZER_FFT_SIZE)
                .map(|index| {
                    let phase =
                        2.0 * std::f32::consts::PI * frequency * index as f32 / sample_rate as f32;
                    phase.sin() * 0.8
                })
                .collect::<Vec<_>>();
            fft.push_samples(&samples);
            fft.levels(sample_rate).expect("full FFT window")
        };
        let peak_band = |levels: &[f64]| {
            levels
                .iter()
                .enumerate()
                .max_by(|(_, left), (_, right)| left.total_cmp(right))
                .map(|(index, _)| index)
                .expect("visualizer band")
        };

        let at_44k = peak_band(&tone(44_100, 1_000.0));
        let at_48k = peak_band(&tone(48_000, 1_000.0));
        assert!(at_44k.abs_diff(at_48k) <= 1);
        assert!(peak_band(&tone(48_000, 8_000.0)) > at_48k);
        for sample_rate in [8_000, 16_000, 22_050, 32_000, 44_100, 48_000, 96_000] {
            let levels = tone(sample_rate, 1_000.0);
            let band = peak_band(&levels);
            let frequency =
                visualizer_frequency((band as f64 + 0.5) / VISUALIZER_BAND_COUNT as f64);
            assert!(
                (frequency - 1_000.0).abs() < 60.0,
                "{sample_rate}: {frequency}"
            );
            for (band, level) in levels.iter().enumerate() {
                if visualizer_frequency(band as f64 / VISUALIZER_BAND_COUNT as f64)
                    >= f64::from(sample_rate) / 2.0
                {
                    assert_eq!(*level, 0.0);
                }
            }
        }
    }

    #[test]
    fn visualizer_axis_round_trips_band_centers() {
        for band in 0..VISUALIZER_BAND_COUNT {
            let position = (band as f64 + 0.5) / VISUALIZER_BAND_COUNT as f64;
            assert!((visualizer_position(visualizer_frequency(position)) - position).abs() < 1e-12);
        }
        assert!((visualizer_frequency(0.0) - 20.0).abs() < 1e-12);
        assert!((visualizer_frequency(1.0) - 20_000.0).abs() < 1e-9);
    }

    #[test]
    fn visualizer_preserves_relative_amplitude() {
        let mut fft = VisualizerFft::new();
        let samples = (0..VISUALIZER_FFT_SIZE)
            .map(|index| {
                let phase =
                    2.0 * std::f32::consts::PI * 48.0 * index as f32 / VISUALIZER_FFT_SIZE as f32;
                phase.sin() * 0.02
            })
            .collect::<Vec<_>>();
        fft.push_samples(&samples);
        let levels = fft.levels(48_000).expect("full FFT window");
        let peak = levels.iter().copied().fold(0.0_f64, f64::max);

        assert!(peak > 0.01 && peak <= 0.0201);
        fft.clear();
        fft.push_samples(
            &samples
                .iter()
                .map(|sample| sample * 10.0)
                .collect::<Vec<_>>(),
        );
        let loud = fft.levels(48_000).expect("full FFT window");
        for (quiet, loud) in levels.iter().zip(loud) {
            assert!((loud - quiet * 10.0).abs() < 1e-6);
        }
    }
}
