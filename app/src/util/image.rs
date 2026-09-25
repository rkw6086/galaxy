//! Shared image processing utilities for agent mode.
//!
//! This module provides common functionality for processing images before they are
//! sent to the AI agent, whether attached by the user or read via the read_files tool.

use std::io::Cursor;
use std::path::Path;

use image::codecs::jpeg::JpegEncoder;
use image::{
    DynamicImage, ExtendedColorType, GenericImageView, ImageEncoder, ImageError, ImageFormat,
};
use mime_guess::from_path;

/// Max image size is 3.75 MB.
/// The max size of an image we will send is 5MB. However, due to the 33% inflation of Base64, this means
/// the largest size a user can attach is actually ~3.75MB.
pub const MAX_IMAGE_SIZE_BYTES: usize = 3750 * 1000;
const JPEG_MIME_TYPE: &str = "image/jpeg";
const PNG_MIME_TYPE: &str = "image/png";
const MIN_READABLE_IMAGE_DIMENSION: u32 = 512;
const JPEG_QUALITIES: &[u8] = &[85, 75, 65, 55];
const MAX_IMAGE_ENCODE_ATTEMPTS: usize = 8;

/// Cap on individual image bytes when relaying a dropped/pasted image to a
/// CLI agent (Claude Code etc.) via the system clipboard. CLI agents handle
/// their own compression, so this limit only exists to keep us from loading
/// arbitrarily large files into memory.
pub const MAX_IMAGE_SIZE_BYTES_FOR_CLI_AGENT: usize = 500 * 1_000_000;

/// How many leading bytes of a file are enough for `infer_mime_type` to
/// match a magic-number signature. Callers that already have the full bytes
/// in memory should pass only the first `MIME_SNIFF_BYTES` to avoid handing
/// arbitrarily large slices to the sniffer.
pub const MIME_SNIFF_BYTES: usize = 8 * 1024;

/// Returns the MIME type for `path`, preferring magic-byte detection from
/// `file_bytes` and falling back to the path's extension when the magic
/// bytes don't yield a confident match. Falls all the way back to
/// `application/octet-stream`. `file_bytes` only needs to contain the first
/// `MIME_SNIFF_BYTES` of the file for the magic-number check.
pub fn infer_mime_type(path: &Path, file_bytes: &[u8]) -> String {
    infer::get(file_bytes)
        .map(|kind| kind.mime_type().to_string())
        .unwrap_or_else(|| from_path(path).first_or_octet_stream().to_string())
}

/// 1.15 Megapixels
pub const MAX_IMAGE_PIXELS: f64 = 1150. * 1000.;

/// Maximum dimension (width or height) for images.
pub const MAX_IMAGE_DIMENSION: f64 = 2000.;

/// Maximum number of images that can be attached per query/task.
pub const MAX_IMAGE_COUNT_FOR_QUERY: usize = 20;

/// Minimum bytes needed for image format detection using magic number signatures.
pub const MIN_IMAGE_HEADER_SIZE: usize = 8;

/// Supported image MIME types for agent mode.
pub const SUPPORTED_IMAGE_MIME_TYPES: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/jpg",
    "image/gif",
    "image/webp",
];

/// Checks if the given MIME type is a supported image type.
pub fn is_supported_image_mime_type(mime_type: &str) -> bool {
    SUPPORTED_IMAGE_MIME_TYPES.contains(&mime_type)
}

#[derive(Debug)]
pub struct ProcessedImage {
    pub data: Vec<u8>,
    pub mime_type: String,
}

/// Resizes and re-encodes an image until it fits the model-facing byte budget.
///
/// The source bytes are never modified. Opaque images may be converted to JPEG
/// because that is the most reliable way to fit photo-like uploads under the
/// provider cap; images with transparency stay PNG so alpha is preserved.
pub fn prepare_image_for_agent(image: &[u8]) -> Result<Option<ProcessedImage>, ImageError> {
    let img = image::load_from_memory(image)?;
    let original_format = image::guess_format(image)?;
    let original_mime_type = mime_type_for_format(original_format);

    if image.len() <= MAX_IMAGE_SIZE_BYTES && image_within_dimension_limits(&img) {
        return Ok(Some(ProcessedImage {
            data: image.to_vec(),
            mime_type: original_mime_type.to_string(),
        }));
    }

    let has_transparency = image_has_transparency(&img);
    let mut width = target_dimension(img.width(), img.height()).0;
    let mut height = target_dimension(img.width(), img.height()).1;
    let mut encode_attempts = 0;

    while encode_attempts < MAX_IMAGE_ENCODE_ATTEMPTS {
        let candidate = if width == img.width() && height == img.height() {
            img.clone()
        } else {
            img.thumbnail(width, height)
        };

        if has_transparency {
            encode_attempts += 1;
            let data = encode_png(&candidate)?;
            if data.len() <= MAX_IMAGE_SIZE_BYTES {
                return Ok(Some(ProcessedImage {
                    data,
                    mime_type: PNG_MIME_TYPE.to_string(),
                }));
            }
        } else {
            encode_attempts += 1;
            let data = encode_png(&candidate)?;
            if data.len() <= MAX_IMAGE_SIZE_BYTES {
                return Ok(Some(ProcessedImage {
                    data,
                    mime_type: PNG_MIME_TYPE.to_string(),
                }));
            }

            for quality in JPEG_QUALITIES {
                if encode_attempts >= MAX_IMAGE_ENCODE_ATTEMPTS {
                    break;
                }
                encode_attempts += 1;
                let data = encode_jpeg(&candidate, *quality)?;
                if data.len() <= MAX_IMAGE_SIZE_BYTES {
                    return Ok(Some(ProcessedImage {
                        data,
                        mime_type: JPEG_MIME_TYPE.to_string(),
                    }));
                }
            }
        }

        let next = next_smaller_dimensions(width, height);
        if next == (width, height) {
            break;
        }
        (width, height) = next;
    }

    Ok(None)
}

/// Backwards-compatible helper for callers that only need processed bytes.
pub fn resize_image(image: &[u8]) -> Result<Vec<u8>, ImageError> {
    prepare_image_for_agent(image)?.map_or_else(
        || {
            Err(ImageError::Limits(image::error::LimitError::from_kind(
                image::error::LimitErrorKind::InsufficientMemory,
            )))
        },
        |processed| Ok(processed.data),
    )
}

fn image_within_dimension_limits(img: &DynamicImage) -> bool {
    let (width, height) = img.dimensions();
    let pixels = (width as f64) * (height as f64);
    pixels <= MAX_IMAGE_PIXELS
        && (width as f64) <= MAX_IMAGE_DIMENSION
        && (height as f64) <= MAX_IMAGE_DIMENSION
}

fn target_dimension(width: u32, height: u32) -> (u32, u32) {
    let pixels = (width as f64) * (height as f64);
    let pixel_scale = if pixels > MAX_IMAGE_PIXELS {
        (MAX_IMAGE_PIXELS / pixels).sqrt()
    } else {
        1.0
    };
    let dimension_scale = (MAX_IMAGE_DIMENSION / width as f64)
        .min(MAX_IMAGE_DIMENSION / height as f64)
        .min(1.0);
    let scale = pixel_scale.min(dimension_scale).min(1.0);
    scaled_dimensions(width, height, scale)
}

fn next_smaller_dimensions(width: u32, height: u32) -> (u32, u32) {
    if width <= MIN_READABLE_IMAGE_DIMENSION && height <= MIN_READABLE_IMAGE_DIMENSION {
        return (width, height);
    }

    let next_width = ((width as f64) * 0.85).round() as u32;
    let next_height = ((height as f64) * 0.85).round() as u32;
    (
        next_width.max(MIN_READABLE_IMAGE_DIMENSION.min(width)),
        next_height.max(MIN_READABLE_IMAGE_DIMENSION.min(height)),
    )
}

fn scaled_dimensions(width: u32, height: u32, scale: f64) -> (u32, u32) {
    (
        ((width as f64) * scale).round().max(1.0) as u32,
        ((height as f64) * scale).round().max(1.0) as u32,
    )
}

fn image_has_transparency(img: &DynamicImage) -> bool {
    if !img.color().has_alpha() {
        return false;
    }
    img.to_rgba8().pixels().any(|pixel| pixel.0[3] < 255)
}

fn encode_image_format(img: &DynamicImage, format: ImageFormat) -> Result<Vec<u8>, ImageError> {
    let mut data = Vec::new();
    img.write_to(&mut Cursor::new(&mut data), format)?;
    Ok(data)
}

fn encode_png(img: &DynamicImage) -> Result<Vec<u8>, ImageError> {
    encode_image_format(img, ImageFormat::Png)
}

fn encode_jpeg(img: &DynamicImage, quality: u8) -> Result<Vec<u8>, ImageError> {
    let rgb = img.to_rgb8();
    let mut data = Vec::new();
    JpegEncoder::new_with_quality(&mut data, quality).write_image(
        rgb.as_raw(),
        rgb.width(),
        rgb.height(),
        ExtendedColorType::Rgb8,
    )?;
    Ok(data)
}

fn mime_type_for_format(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Png => PNG_MIME_TYPE,
        ImageFormat::Jpeg => JPEG_MIME_TYPE,
        ImageFormat::Gif => "image/gif",
        ImageFormat::WebP => "image/webp",
        ImageFormat::Pnm => "image/x-portable-anymap",
        ImageFormat::Tiff => "image/tiff",
        ImageFormat::Tga => "image/x-tga",
        ImageFormat::Dds => "image/vnd-ms.dds",
        ImageFormat::Bmp => "image/bmp",
        ImageFormat::Ico => "image/x-icon",
        ImageFormat::Hdr => "image/vnd.radiance",
        ImageFormat::OpenExr => "image/x-exr",
        ImageFormat::Farbfeld => "image/farbfeld",
        ImageFormat::Avif => "image/avif",
        ImageFormat::Qoi => "image/qoi",
        _ => "application/octet-stream",
    }
}

/// Result of processing an image for agent mode.
#[derive(Debug)]
pub enum ProcessImageResult {
    /// Image was successfully processed and is within size limits.
    Success {
        /// The processed image bytes (resized/re-encoded if needed).
        data: Vec<u8>,
        /// MIME type matching the processed image bytes.
        mime_type: String,
    },
    /// Image is too large even after resizing.
    TooLarge,
    /// Error processing the image.
    Error(ImageError),
}

/// Processes an image for agent mode: resizes if needed and checks size limits.
///
/// This applies the same processing that user-attached images go through.
pub fn process_image_for_agent(image_data: &[u8]) -> ProcessImageResult {
    match prepare_image_for_agent(image_data) {
        Ok(Some(processed)) => ProcessImageResult::Success {
            data: processed.data,
            mime_type: processed.mime_type,
        },
        Ok(None) => ProcessImageResult::TooLarge,
        Err(err) => ProcessImageResult::Error(err),
    }
}

#[cfg(test)]
#[path = "image_tests.rs"]
mod tests;
