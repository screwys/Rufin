use std::io::{BufRead, Cursor, Seek};
use std::sync::{Arc, Once};

use fast_image_resize::FilterType;
use image::{
    DynamicImage, ImageDecoder, ImageEncoder, ImageFormat, ImageReader, metadata::Orientation,
};

use crate::{ArtworkError, ArtworkKey};

fn preview_dimensions(width: u32, height: u32) -> (u32, u32) {
    let longest = width.max(height).max(32);
    (
        ((u64::from(width) * 32) / u64::from(longest)).max(1) as u32,
        ((u64::from(height) * 32) / u64::from(longest)).max(1) as u32,
    )
}

#[derive(Clone, Debug)]
pub struct RgbaImage {
    width: u32,
    height: u32,
    row_stride: u32,
    rgba: Arc<Vec<u8>>,
}

impl AsRef<[u8]> for RgbaImage {
    fn as_ref(&self) -> &[u8] {
        self.rgba()
    }
}

impl RgbaImage {
    pub fn encode_png(&self) -> Result<Vec<u8>, ArtworkError> {
        let mut bytes = Vec::new();
        image::codecs::png::PngEncoder::new(&mut bytes)
            .write_image(
                self.rgba(),
                self.width,
                self.height,
                image::ExtendedColorType::Rgba8,
            )
            .map_err(decode_error)?;
        Ok(bytes)
    }

    pub(crate) fn from_blur_hash(hash: &str) -> Result<Self, ArtworkError> {
        Ok(Self {
            width: 32,
            height: 32,
            row_stride: 128,
            rgba: Arc::new(blurhash::decode(hash, 32, 32, 1.0).map_err(decode_error)?),
        })
    }

    pub(crate) fn blur_hash(&self) -> Result<String, ArtworkError> {
        let resized;
        let image = if self.width.max(self.height) > 32 {
            let (width, height) = preview_dimensions(self.width, self.height);
            resized = self.resized(width, height, FilterType::Bilinear)?;
            &resized
        } else {
            self
        };
        blurhash::encode(4, 3, image.width, image.height, image.rgba()).map_err(decode_error)
    }

    pub const fn width(&self) -> u32 {
        self.width
    }

    pub const fn height(&self) -> u32 {
        self.height
    }

    pub const fn row_stride(&self) -> u32 {
        self.row_stride
    }

    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }

    pub fn resized_to_fit(&self, size: u32) -> Result<Self, ArtworkError> {
        let longest = self.width.max(self.height());
        if longest <= size {
            return Ok(self.clone());
        }
        let width = (u64::from(self.width) * u64::from(size) / u64::from(longest)).max(1) as u32;
        let height = (u64::from(self.height) * u64::from(size) / u64::from(longest)).max(1) as u32;
        self.resized_exact(width, height)
    }

    pub fn resized_exact(&self, width: u32, height: u32) -> Result<Self, ArtworkError> {
        self.resized(width, height, FilterType::Lanczos3)
    }

    fn resized(&self, width: u32, height: u32, filter: FilterType) -> Result<Self, ArtworkError> {
        let source = fast_image_resize::images::ImageRef::new(
            self.width,
            self.height,
            self.rgba(),
            fast_image_resize::PixelType::U8x4,
        )
        .map_err(decode_error)?;
        let (width, height) = (width.max(1), height.max(1));
        let row_stride = width
            .checked_mul(4)
            .ok_or_else(|| decode_error("artwork dimensions overflowed"))?;
        let mut resized = fast_image_resize::images::Image::new(
            width,
            height,
            fast_image_resize::PixelType::U8x4,
        );
        let options = fast_image_resize::ResizeOptions::new()
            .resize_alg(fast_image_resize::ResizeAlg::Convolution(filter))
            .use_alpha(false);
        fast_image_resize::Resizer::new()
            .resize(&source, &mut resized, &options)
            .map_err(decode_error)?;
        Ok(Self {
            width,
            height,
            row_stride,
            rgba: Arc::new(resized.into_vec()),
        })
    }
}

#[derive(Clone, Debug)]
pub struct DecodedImage {
    key: ArtworkKey,
    pub(crate) pixels: RgbaImage,
    animation_bytes: Option<Arc<[u8]>>,
}

impl DecodedImage {
    pub fn pixels(&self) -> &RgbaImage {
        &self.pixels
    }

    pub(crate) fn blur_hash(&self) -> Result<String, ArtworkError> {
        self.pixels.blur_hash()
    }

    pub fn animation_bytes(&self) -> Option<&Arc<[u8]>> {
        self.animation_bytes.as_ref()
    }

    pub fn key(&self) -> &ArtworkKey {
        &self.key
    }

    pub const fn width(&self) -> u32 {
        self.pixels.width()
    }

    pub const fn height(&self) -> u32 {
        self.pixels.height()
    }

    pub const fn row_stride(&self) -> u32 {
        self.pixels.row_stride()
    }

    pub fn rgba(&self) -> &[u8] {
        self.pixels.rgba()
    }
}

pub fn decode_rgba(bytes: &[u8], render_size: u32) -> Result<RgbaImage, ArtworkError> {
    register_decoders();
    let image = decode_reader(
        ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .map_err(decode_error)?,
    )?;
    rgba_image(scale_to_fit(
        image,
        render_size.max(1),
        FilterType::Lanczos3,
    )?)
}

pub fn decode_cover(bytes: &[u8], render_size: u32) -> Result<RgbaImage, ArtworkError> {
    register_decoders();
    let image = decode_reader(
        ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .map_err(decode_error)?,
    )?;
    rgba_image(cover_poster(&image, render_size)?)
}

pub fn square_thumbnail_png(bytes: &[u8], size: u32) -> Result<Vec<u8>, ArtworkError> {
    encode_png(&square_thumbnail(bytes, size)?)
}

pub fn collage_png(images: &[Vec<u8>], size: u32) -> Result<Vec<u8>, ArtworkError> {
    if images.is_empty() {
        return Err(ArtworkError::Decode(
            "No track artwork is available for this playlist".into(),
        ));
    }
    if images.len() == 1 {
        return square_thumbnail_png(&images[0], size);
    }
    let cell = size.max(2) / 2;
    let tiles = images
        .iter()
        .take(4)
        .map(|bytes| square_thumbnail(bytes, cell))
        .collect::<Result<Vec<_>, _>>()?;
    let mut canvas = DynamicImage::new_rgba8(cell * 2, cell * 2);
    for index in 0..4 {
        image::imageops::overlay(
            &mut canvas,
            &tiles[index % tiles.len()],
            (index % 2) as i64 * i64::from(cell),
            (index / 2) as i64 * i64::from(cell),
        );
    }
    encode_png(&canvas)
}

fn square_thumbnail(bytes: &[u8], size: u32) -> Result<DynamicImage, ArtworkError> {
    register_decoders();
    let image = decode_reader(
        ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .map_err(decode_error)?,
    )?;
    let target = size.max(1);
    let crop_size = image.width().min(image.height());
    if crop_size == 0 {
        return Err(ArtworkError::Decode(
            "artwork dimensions were empty".to_string(),
        ));
    }
    let crop_x = (image.width() - crop_size) / 2;
    let crop_y = (image.height() - crop_size) / 2;
    let cropped = image.crop_imm(crop_x, crop_y, crop_size, crop_size);
    let thumbnail = if crop_size == target {
        cropped
    } else {
        resize_exact(&cropped, target, target, FilterType::Bilinear)?
    };
    Ok(thumbnail)
}

// Playback tiles fill a square. Size the shorter edge for that tile while
// keeping the full image available to the playback background.
fn cover_poster(image: &DynamicImage, size: u32) -> Result<DynamicImage, ArtworkError> {
    let shortest = image.width().min(image.height()).max(1);
    let size = size.max(1).min(shortest);
    let width = ((u64::from(image.width()) * u64::from(size) + u64::from(shortest / 2))
        / u64::from(shortest)) as u32;
    let height = ((u64::from(image.height()) * u64::from(size) + u64::from(shortest / 2))
        / u64::from(shortest)) as u32;
    resize_exact(image, width, height, FilterType::Lanczos3)
}

fn decode_reader<R>(reader: ImageReader<R>) -> Result<DynamicImage, ArtworkError>
where
    R: BufRead + Seek,
{
    decode_image(reader.into_decoder().map_err(decode_error)?)
}

fn decode_image(mut decoder: impl ImageDecoder) -> Result<DynamicImage, ArtworkError> {
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let mut image = DynamicImage::from_decoder(decoder).map_err(decode_error)?;
    image.apply_orientation(orientation);
    if image.color().has_alpha() {
        Ok(DynamicImage::ImageRgba8(image.into_rgba8()))
    } else {
        Ok(DynamicImage::ImageRgb8(image.into_rgb8()))
    }
}

pub(crate) fn scale_to_fit(
    image: DynamicImage,
    target: u32,
    filter: FilterType,
) -> Result<DynamicImage, ArtworkError> {
    let width = image.width();
    let height = image.height();
    if width == 0 || height == 0 {
        return Err(ArtworkError::Decode(
            "artwork dimensions were empty".to_string(),
        ));
    }
    let longest = width.max(height);
    if longest <= target {
        return Ok(image);
    }
    let scaled_width = (u64::from(width) * u64::from(target) / u64::from(longest)).max(1);
    let scaled_height = (u64::from(height) * u64::from(target) / u64::from(longest)).max(1);
    let scaled_width = u32::try_from(scaled_width)
        .map_err(|_| ArtworkError::Decode("scaled artwork width was invalid".to_string()))?;
    let scaled_height = u32::try_from(scaled_height)
        .map_err(|_| ArtworkError::Decode("scaled artwork height was invalid".to_string()))?;
    resize_exact(&image, scaled_width, scaled_height, filter)
}

fn resize_exact(
    image: &DynamicImage,
    width: u32,
    height: u32,
    filter: FilterType,
) -> Result<DynamicImage, ArtworkError> {
    let mut resized = if image.color().has_alpha() {
        DynamicImage::new_rgba8(width, height)
    } else {
        DynamicImage::new_rgb8(width, height)
    };
    let options = fast_image_resize::ResizeOptions::new()
        .resize_alg(fast_image_resize::ResizeAlg::Convolution(filter))
        .use_alpha(false);
    fast_image_resize::Resizer::new()
        .resize(image, &mut resized, &options)
        .map_err(decode_error)?;
    Ok(resized)
}

pub(crate) fn rgba_image(image: DynamicImage) -> Result<RgbaImage, ArtworkError> {
    rgba_buffer(image.into_rgba8())
}

fn rgba_buffer(image: image::RgbaImage) -> Result<RgbaImage, ArtworkError> {
    let width = image.width();
    let height = image.height();
    let row_stride = width
        .checked_mul(4)
        .ok_or_else(|| ArtworkError::Decode("artwork dimensions overflowed".to_string()))?;
    let expected = usize::try_from(row_stride)
        .ok()
        .and_then(|stride| stride.checked_mul(height as usize))
        .ok_or_else(|| ArtworkError::Decode("artwork dimensions overflowed".to_string()))?;
    let rgba = image.into_raw();
    if rgba.len() != expected {
        return Err(ArtworkError::Decode(
            "artwork pixel data was truncated".to_string(),
        ));
    }
    Ok(RgbaImage {
        width,
        height,
        row_stride,
        rgba: Arc::new(rgba),
    })
}

fn encode_png(image: &DynamicImage) -> Result<Vec<u8>, ArtworkError> {
    let mut bytes = Cursor::new(Vec::new());
    image
        .write_to(&mut bytes, ImageFormat::Png)
        .map_err(decode_error)?;
    Ok(bytes.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wide_png() -> Vec<u8> {
        let image = DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            400,
            200,
            image::Rgb([20, 40, 60]),
        ));
        encode_png(&image).expect("encode fixture")
    }

    #[test]
    fn decoding_preserves_aspect_ratio_and_preview_colors() {
        let decoded = decode_rgba(&wide_png(), 100).expect("decode image");
        assert_eq!((decoded.width(), decoded.height()), (100, 50));
        assert_eq!(decoded.row_stride(), 400);
        let preview = RgbaImage::from_blur_hash(&decoded.blur_hash().unwrap()).unwrap();
        assert_eq!(
            (preview.width(), preview.height(), preview.row_stride()),
            (32, 32, 128)
        );
        let encoded = preview.encode_png().unwrap();
        assert_eq!(decode_rgba(&encoded, 32).unwrap().rgba(), preview.rgba());
        assert!(RgbaImage::from_blur_hash("invalid").is_err());
    }

    #[test]
    fn square_thumbnail_crops_before_scaling() {
        let thumbnail = square_thumbnail_png(&wide_png(), 64).expect("make thumbnail");
        let decoded = decode_rgba(&thumbnail, 64).expect("decode thumbnail");
        assert_eq!((decoded.width(), decoded.height()), (64, 64));
    }
}

pub(crate) fn decode_error(error: impl std::fmt::Display) -> ArtworkError {
    ArtworkError::Decode(error.to_string())
}

fn register_decoders() {
    static REGISTER: Once = Once::new();
    REGISTER.call_once(|| {
        jxl_image_rs_integration::register_image_decoding_hook();
    });
}

pub(crate) fn decode_display<R: BufRead + Seek>(
    reader: R,
    key: ArtworkKey,
    render_size: u32,
    cache_size: Option<u32>,
    decode: bool,
) -> Result<(Option<DecodedImage>, Option<Vec<u8>>), ArtworkError> {
    register_decoders();
    let decoder = ImageReader::new(reader)
        .with_guessed_format()
        .map_err(decode_error)?
        .into_decoder()
        .map_err(decode_error)?;
    let (width, height) = decoder.dimensions();
    let normalize = cache_size.filter(|size| width.max(height) > *size);
    if !decode && normalize.is_none() {
        return Ok((None, None));
    }
    let source = decode_image(decoder)?;
    let source = if let Some(size) = normalize {
        scale_to_fit(source, size, FilterType::Bilinear)?
    } else {
        source
    };
    let encoded = normalize.map(|_| encode_png(&source)).transpose()?;
    let image = if decode {
        Some(DecodedImage {
            key,
            pixels: rgba_image(scale_to_fit(source, render_size, FilterType::Lanczos3)?)?,
            animation_bytes: None,
        })
    } else {
        None
    };
    Ok((image, encoded))
}

pub(crate) fn cached_poster(
    path: &std::path::Path,
    render_size: u32,
) -> Result<Option<RgbaImage>, ArtworkError> {
    register_decoders();
    let decoder = ImageReader::open(path)
        .map_err(decode_error)?
        .with_guessed_format()
        .map_err(decode_error)?
        .into_decoder()
        .map_err(decode_error)?;
    let (width, height) = decoder.dimensions();
    if width.min(height) < render_size {
        return Ok(None);
    }
    rgba_image(cover_poster(&decode_image(decoder)?, render_size)?).map(Some)
}

pub(crate) fn decode_original(
    bytes: Arc<[u8]>,
    key: ArtworkKey,
    render_size: u32,
    poster: Option<RgbaImage>,
    display_size: Option<u32>,
) -> Result<(DecodedImage, Option<Vec<u8>>), ArtworkError> {
    let (pixels, display) = if let Some(pixels) = poster {
        (pixels, None)
    } else {
        register_decoders();
        let source = decode_reader(
            ImageReader::new(Cursor::new(bytes.as_ref()))
                .with_guessed_format()
                .map_err(decode_error)?,
        )?;
        let pixels = rgba_image(cover_poster(&source, render_size)?)?;
        let display = display_size
            .filter(|size| source.width().max(source.height()) > *size)
            .map(|size| encode_png(&scale_to_fit(source, size, FilterType::Bilinear)?))
            .transpose()?;
        (pixels, display)
    };
    let animation_bytes = match crate::Animation::new(Arc::clone(&bytes), 1) {
        Ok(animation) => animation.is_some().then_some(bytes),
        Err(error) => {
            tracing::warn!(%error, "cover animation could not be opened");
            None
        }
    };
    Ok((
        DecodedImage {
            key,
            pixels,
            animation_bytes,
        },
        display,
    ))
}

pub(crate) fn original_extension(bytes: &[u8]) -> Result<&'static str, ArtworkError> {
    if is_jxl(bytes) {
        return Ok("jxl");
    }
    Ok(match image::guess_format(bytes).map_err(decode_error)? {
        ImageFormat::Jpeg => "jpg",
        ImageFormat::Png => "png",
        ImageFormat::Gif => "gif",
        ImageFormat::WebP => "webp",
        ImageFormat::Tiff => "tiff",
        ImageFormat::Bmp => "bmp",
        format => {
            return Err(decode_error(format!(
                "unsupported artwork format {format:?}"
            )));
        }
    })
}

pub(crate) fn is_jxl(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\xff\x0a") || bytes.starts_with(b"\x00\x00\x00\x0cJXL \x0d\x0a\x87\x0a")
}

pub fn image_mime(bytes: &[u8]) -> Option<&'static str> {
    Some(match original_extension(bytes).ok()? {
        "jpg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "tiff" => "image/tiff",
        "bmp" => "image/bmp",
        "jxl" => "image/jxl",
        _ => return None,
    })
}
