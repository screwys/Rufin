use std::io::{BufRead, Cursor, Seek};
use std::path::Path;
use std::sync::{Arc, Once};

use fast_image_resize::FilterType;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, metadata::Orientation};

use crate::{ArtworkError, ArtworkKey};

pub(crate) struct NormalizedImage {
    image: DynamicImage,
    bytes: Vec<u8>,
}

impl NormalizedImage {
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }
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

    pub fn resized_exact(&self, width: u32, height: u32) -> Result<Self, ArtworkError> {
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
            .resize_alg(fast_image_resize::ResizeAlg::Convolution(
                FilterType::Lanczos3,
            ))
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
    pixels: RgbaImage,
}

impl DecodedImage {
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

pub fn square_thumbnail_png(bytes: &[u8], size: u32) -> Result<Vec<u8>, ArtworkError> {
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
        resize_exact(cropped, target, target, FilterType::Bilinear)?
    };
    encode_png(&thumbnail)
}

pub(crate) fn decode_cached(
    path: &Path,
    key: ArtworkKey,
    render_size: u32,
) -> Result<DecodedImage, ArtworkError> {
    register_decoders();
    let reader = ImageReader::open(path)
        .and_then(ImageReader::with_guessed_format)
        .map_err(decode_error)?;
    let image = decode_reader(reader)?;
    decoded_image(image, key, render_size)
}

pub(crate) fn decode_normalized(
    image: NormalizedImage,
    key: ArtworkKey,
    render_size: u32,
) -> Result<DecodedImage, ArtworkError> {
    decoded_image(image.image, key, render_size)
}

fn decoded_image(
    image: DynamicImage,
    key: ArtworkKey,
    render_size: u32,
) -> Result<DecodedImage, ArtworkError> {
    Ok(DecodedImage {
        key,
        pixels: rgba_image(scale_to_fit(
            image,
            render_size.max(1),
            FilterType::Lanczos3,
        )?)?,
    })
}

pub(crate) fn normalize_for_cache(
    bytes: &[u8],
    size: u32,
) -> Result<NormalizedImage, ArtworkError> {
    register_decoders();
    let image = decode_reader(
        ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .map_err(decode_error)?,
    )?;
    let image = scale_to_fit(image, size.max(1), FilterType::Bilinear)?;
    let bytes = encode_png(&image)?;
    Ok(NormalizedImage { image, bytes })
}

fn decode_reader<R>(reader: ImageReader<R>) -> Result<DynamicImage, ArtworkError>
where
    R: BufRead + Seek,
{
    let mut decoder = reader.into_decoder().map_err(decode_error)?;
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
    resize_exact(image, scaled_width, scaled_height, filter)
}

fn resize_exact(
    image: DynamicImage,
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
        .resize(&image, &mut resized, &options)
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
    fn normalization_preserves_aspect_ratio_and_emits_png() {
        let normalized = normalize_for_cache(&wide_png(), 100).expect("normalize image");
        assert_eq!(&normalized.bytes()[..8], b"\x89PNG\r\n\x1a\n");
        let decoded = decode_rgba(normalized.bytes(), 100).expect("decode normalized image");
        assert_eq!((decoded.width(), decoded.height()), (100, 50));
        assert_eq!(decoded.row_stride(), 400);
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

pub(crate) fn decode_original(
    bytes: &[u8],
    key: ArtworkKey,
    render_size: u32,
) -> Result<DecodedImage, ArtworkError> {
    Ok(DecodedImage {
        key,
        pixels: decode_rgba(bytes, render_size)?,
    })
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
