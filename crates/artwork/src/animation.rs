use std::io::{BufRead, BufReader, Cursor};
use std::sync::Arc;
use std::time::Duration;

use fast_image_resize::FilterType;
use image::codecs::{gif::GifDecoder, png::PngDecoder, webp::WebPDecoder};
use image::metadata::{LoopCount, Orientation};
use image::{AnimationDecoder, DynamicImage, ImageDecoder, ImageFormat};
use jxl::api::{
    JxlDecoder, JxlDecoderOptions, JxlOutputBuffer, JxlPixelFormat, ProcessingResult, states,
};

use crate::decode::{decode_error, is_jxl, rgba_image, scale_to_fit};
use crate::{ArtworkError, RgbaImage};

pub struct AnimationFrame {
    pub pixels: RgbaImage,
    pub duration: Duration,
}

/// A sequential decoder owned by the presentation's worker thread.
pub struct Animation {
    bytes: Arc<[u8]>,
    decoder: Decoder,
    render_size: u32,
    remaining_plays: Option<u32>,
}

enum Decoder {
    Image {
        frames: image::Frames<'static>,
        orientation: Orientation,
    },
    Jxl(JxlFrames),
}

impl Animation {
    pub fn new(bytes: Arc<[u8]>, render_size: u32) -> Result<Option<Self>, ArtworkError> {
        let Some((decoder, remaining_plays)) = open_decoder(Arc::clone(&bytes))? else {
            return Ok(None);
        };
        Ok(Some(Self {
            bytes,
            decoder,
            render_size: render_size.max(1),
            remaining_plays,
        }))
    }

    pub fn set_render_size(&mut self, size: u32) {
        self.render_size = size.max(1);
    }

    pub fn next_frame(&mut self) -> Result<Option<AnimationFrame>, ArtworkError> {
        if self.remaining_plays == Some(0) {
            return Ok(None);
        }
        if let Some(frame) = self.decoder.next(self.render_size)? {
            return Ok(Some(frame));
        }
        if let Some(plays) = &mut self.remaining_plays {
            *plays -= 1;
            if *plays == 0 {
                return Ok(None);
            }
        }
        let Some((decoder, _)) = open_decoder(Arc::clone(&self.bytes))? else {
            return Ok(None);
        };
        self.decoder = decoder;
        self.decoder.next(self.render_size)
    }
}

impl Decoder {
    fn next(&mut self, render_size: u32) -> Result<Option<AnimationFrame>, ArtworkError> {
        let (mut image, duration) = match self {
            Self::Image {
                frames,
                orientation,
            } => {
                let Some(frame) = frames.next().transpose().map_err(decode_error)? else {
                    return Ok(None);
                };
                let duration = Duration::from(frame.delay());
                let mut image = DynamicImage::ImageRgba8(frame.into_buffer());
                image.apply_orientation(*orientation);
                (image, duration)
            }
            Self::Jxl(decoder) => {
                let Some(frame) = decoder.next()? else {
                    return Ok(None);
                };
                frame
            }
        };
        image = scale_to_fit(image, render_size, FilterType::Bilinear)?;
        Ok(Some(AnimationFrame {
            pixels: rgba_image(image)?,
            duration,
        }))
    }
}

fn plays(count: LoopCount) -> Option<u32> {
    match count {
        LoopCount::Infinite => None,
        LoopCount::Finite(count) => Some(count.get()),
    }
}

fn open_decoder(bytes: Arc<[u8]>) -> Result<Option<(Decoder, Option<u32>)>, ArtworkError> {
    if is_jxl(&bytes) {
        let Some((decoder, count)) = JxlFrames::new(bytes)? else {
            return Ok(None);
        };
        return Ok(Some((Decoder::Jxl(decoder), count)));
    }
    let format = image::guess_format(&bytes).map_err(decode_error)?;
    let (frames, count, orientation) = match format {
        ImageFormat::Gif => {
            // GIF stores repetitions after the first play. The image adapter's
            // loop count also treats a missing repeat extension as infinite.
            let mut options = gif::DecodeOptions::new();
            options.skip_frame_decoding(true);
            let mut metadata = options
                .read_info(Cursor::new(Arc::clone(&bytes)))
                .map_err(decode_error)?;
            metadata.read_next_frame().map_err(decode_error)?;
            if metadata.read_next_frame().map_err(decode_error)?.is_none() {
                return Ok(None);
            }
            let count = match metadata.repeat() {
                gif::Repeat::Infinite => None,
                gif::Repeat::Finite(repeats) => Some(u32::from(repeats) + 1),
            };
            let decoder = GifDecoder::new(Cursor::new(bytes)).map_err(decode_error)?;
            (decoder.into_frames(), count, Orientation::NoTransforms)
        }
        ImageFormat::Png => {
            let mut decoder = PngDecoder::new(Cursor::new(bytes)).map_err(decode_error)?;
            if !decoder.is_apng().map_err(decode_error)? {
                return Ok(None);
            }
            let orientation = decoder.orientation().map_err(decode_error)?;
            let decoder = decoder.apng().map_err(decode_error)?;
            let count = plays(decoder.loop_count());
            (decoder.into_frames(), count, orientation)
        }
        ImageFormat::WebP => {
            let mut decoder = WebPDecoder::new(Cursor::new(bytes)).map_err(decode_error)?;
            if !decoder.has_animation() {
                return Ok(None);
            }
            let orientation = decoder.orientation().map_err(decode_error)?;
            let count = plays(decoder.loop_count());
            (decoder.into_frames(), count, orientation)
        }
        _ => return Ok(None),
    };
    Ok(Some((
        Decoder::Image {
            frames,
            orientation,
        },
        count,
    )))
}

struct JxlFrames {
    input: BufReader<Cursor<Arc<[u8]>>>,
    decoder: Option<JxlDecoder<states::WithImageInfo>>,
}

impl JxlFrames {
    fn new(bytes: Arc<[u8]>) -> Result<Option<(Self, Option<u32>)>, ArtworkError> {
        let mut input = BufReader::new(Cursor::new(bytes));
        let decoder = JxlDecoder::new(JxlDecoderOptions::default());
        let mut decoder = complete(decoder, &mut input, |decoder, input| {
            decoder.process(input, None)
        })?;
        let Some(animation) = &decoder.basic_info().animation else {
            return Ok(None);
        };
        let count = animation.num_loops;
        decoder
            .set_pixel_format(JxlPixelFormat::rgba8(
                decoder.basic_info().extra_channels.len(),
            ))
            .map_err(decode_error)?;
        Ok(Some((
            Self {
                input,
                decoder: Some(decoder),
            },
            (count != 0).then_some(count),
        )))
    }

    fn next(&mut self) -> Result<Option<(DynamicImage, Duration)>, ArtworkError> {
        let Some(decoder) = self.decoder.take() else {
            return Ok(None);
        };
        if !decoder.has_more_frames() {
            return Ok(None);
        }
        let decoder = complete(decoder, &mut self.input, |decoder, input| {
            decoder.process(input, None)
        })?;
        let header = decoder.frame_header();
        let width = u32::try_from(header.size.0).map_err(decode_error)?;
        let height = u32::try_from(header.size.1).map_err(decode_error)?;
        let mut image = image::RgbaImage::new(width, height);
        let mut output = JxlOutputBuffer::new(image.as_mut(), height as usize, width as usize * 4);
        self.decoder = Some(complete(decoder, &mut self.input, |decoder, input| {
            decoder.process(input, std::slice::from_mut(&mut output), None)
        })?);
        let duration = Duration::try_from_secs_f64(header.duration.unwrap_or_default() / 1000.0)
            .map_err(decode_error)?;
        Ok(Some((DynamicImage::ImageRgba8(image), duration)))
    }
}

fn complete<T, F>(
    mut decoder: F,
    input: &mut BufReader<Cursor<Arc<[u8]>>>,
    mut process: impl FnMut(
        F,
        &mut BufReader<Cursor<Arc<[u8]>>>,
    ) -> jxl::error::Result<ProcessingResult<T, F>>,
) -> Result<T, ArtworkError> {
    loop {
        match process(decoder, input).map_err(decode_error)? {
            ProcessingResult::Complete { result } => return Ok(result),
            ProcessingResult::NeedsMoreInput { fallback, .. } => {
                if input.fill_buf().map_err(decode_error)?.is_empty() {
                    return Err(decode_error("truncated JPEG XL image"));
                }
                decoder = fallback;
            }
        }
    }
}
