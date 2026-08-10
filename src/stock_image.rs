use std::collections::HashMap;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use image::GenericImageView;

use crate::storage;

const MAX_STOCK_IMAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_STOCK_IMAGE_DIMENSION: i32 = 4096;
const MAX_STOCK_IMAGE_PIXELS: i64 = 16_777_216;
const MAX_STOCK_IMAGE_DECODE_BYTES: u64 = 128 * 1024 * 1024;
const STORED_STOCK_IMAGE_DIMENSION: u32 = 512;

#[derive(Clone, Debug)]
pub struct StockImageData {
    pub bytes: Vec<u8>,
    /// Dominant non-white colors from the visible circular crop, most populated first.
    pub colors: Vec<(f64, f64, f64)>,
}

pub fn load_stock_image(provider_symbol: &str) -> Result<Option<StockImageData>, String> {
    let path = stock_image_path(provider_symbol)?;
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(&path).map_err(|error| format!("Could not read stock picture: {error}"))?;
    stock_image_from_bytes(bytes).map(Some)
}

pub fn save_stock_image(provider_symbol: &str, source: &Path) -> Result<StockImageData, String> {
    let bytes = fs::read(source).map_err(|error| format!("Could not read selected picture: {error}"))?;
    if bytes.len() > MAX_STOCK_IMAGE_BYTES {
        return Err("The selected picture is larger than 8 MB".into());
    }

    // Decode the actual file contents rather than trusting the extension. The
    // image crate handles common raster formats, jxl-oxide supplies JPEG XL,
    // and GdkPixbuf/librsvg provides the GNOME image-loading path used for SVG.
// Normalize the
    // stored copy to PNG so every GTK/libadwaita surface can display it
    // consistently after selection.
    let decoded = decode_selected_image(&bytes, source)?;
    let normalized_image = if decoded.width() > STORED_STOCK_IMAGE_DIMENSION
        || decoded.height() > STORED_STOCK_IMAGE_DIMENSION
    {
        decoded.thumbnail(STORED_STOCK_IMAGE_DIMENSION, STORED_STOCK_IMAGE_DIMENSION)
    } else {
        decoded
    };
    let colors = dominant_colors(&normalized_image);
    let mut normalized = Cursor::new(Vec::new());
    normalized_image
        .write_to(&mut normalized, image::ImageFormat::Png)
        .map_err(|error| format!("Could not prepare stock picture: {error}"))?;
    let image = StockImageData {
        bytes: normalized.into_inner(),
        colors,
    };

    let path = stock_image_path(provider_symbol)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("Could not create stock picture folder: {error}"))?;
    }
    let temporary = path.with_extension("img.tmp");
    fs::write(&temporary, &image.bytes)
        .map_err(|error| format!("Could not save stock picture: {error}"))?;
    if let Err(error) = fs::rename(&temporary, &path) {
        let _ = fs::remove_file(&temporary);
        return Err(format!("Could not finish saving stock picture: {error}"));
    }
    Ok(image)
}

pub fn has_stock_image(provider_symbol: &str) -> bool {
    stock_image_path(provider_symbol)
        .map(|path| path.exists())
        .unwrap_or(false)
}

pub fn remove_stock_image(provider_symbol: &str) -> Result<bool, String> {
    let path = stock_image_path(provider_symbol)?;
    if !path.exists() {
        return Ok(false);
    }
    fs::remove_file(path).map_err(|error| format!("Could not delete stock picture: {error}"))?;
    Ok(true)
}

fn stock_image_from_bytes(bytes: Vec<u8>) -> Result<StockImageData, String> {
    let image = decode_raster_with_limits(&bytes)
        .ok_or_else(|| "Could not decode saved stock picture".to_string())?;
    validate_image_dimensions(&image)?;
    let colors = dominant_colors(&image);
    Ok(StockImageData { bytes, colors })
}

fn decode_selected_image(bytes: &[u8], source: &Path) -> Result<image::DynamicImage, String> {
    if let Some(image) = decode_raster_with_limits(bytes) {
        validate_image_dimensions(&image)?;
        return Ok(image);
    }

    if let Ok(decoder) = jxl_oxide::integration::JxlDecoder::new(Cursor::new(bytes)) {
        if let Ok(image) = image::DynamicImage::from_decoder(decoder) {
            validate_image_dimensions(&image)?;
            return Ok(image);
        }
    }

    // SVG is not decoded by the image crate. Use GdkPixbuf here instead of
    // GdkTexture: the GNOME runtime's librsvg pixbuf loader rasterizes SVGs,
    // and requesting a bounded size avoids ever materializing a huge vector
    // canvas. The resulting pixels are immediately normalized to PNG like
    // every other custom stock picture.
    let pixbuf = gdk_pixbuf::Pixbuf::from_file_at_scale(
        source,
        STORED_STOCK_IMAGE_DIMENSION as i32,
        STORED_STOCK_IMAGE_DIMENSION as i32,
        true,
    )
    .map_err(|_| "The selected file is not a supported image".to_string())?;
    validate_dimensions(pixbuf.width(), pixbuf.height())?;

    let png = pixbuf
        .save_to_bufferv("png", &[])
        .map_err(|_| "Could not prepare the selected picture".to_string())?;
    let image = decode_raster_with_limits(&png)
        .ok_or_else(|| "Could not prepare the selected picture".to_string())?;
    validate_image_dimensions(&image)?;
    Ok(image)
}

fn stock_image_limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_STOCK_IMAGE_DIMENSION as u32);
    limits.max_image_height = Some(MAX_STOCK_IMAGE_DIMENSION as u32);
    limits.max_alloc = Some(MAX_STOCK_IMAGE_DECODE_BYTES);
    limits
}

fn decode_raster_with_limits(bytes: &[u8]) -> Option<image::DynamicImage> {
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    reader.limits(stock_image_limits());
    reader.decode().ok()
}

fn validate_image_dimensions(image: &image::DynamicImage) -> Result<(), String> {
    let (width, height) = image.dimensions();
    validate_dimensions(width as i32, height as i32)
}

fn validate_dimensions(width: i32, height: i32) -> Result<(), String> {
    let pixels = i64::from(width).saturating_mul(i64::from(height));
    if width <= 0
        || height <= 0
        || width > MAX_STOCK_IMAGE_DIMENSION
        || height > MAX_STOCK_IMAGE_DIMENSION
        || pixels > MAX_STOCK_IMAGE_PIXELS
    {
        return Err("The selected picture is too large to render safely".into());
    }
    Ok(())
}

fn dominant_colors(image: &image::DynamicImage) -> Vec<(f64, f64, f64)> {
    let rgba = image.to_rgba8();
    let (width, height) = image.dimensions();
    let step = ((width.max(height) / 96).max(1)) as usize;
    let mut buckets = HashMap::<(u8, u8, u8), usize>::new();
    let center_x = (f64::from(width) - 1.0) / 2.0;
    let center_y = (f64::from(height) - 1.0) / 2.0;
    let radius = f64::from(width.min(height)) / 2.0;

    for y in (0..height as usize).step_by(step) {
        for x in (0..width as usize).step_by(step) {
            // Stock pictures are displayed through AdwAvatar, which clips to a
            // circle. Sample that same circle for Allocation colors.
            let dx = x as f64 - center_x;
            let dy = y as f64 - center_y;
            if dx * dx + dy * dy > radius * radius {
                continue;
            }
            let pixel = rgba.get_pixel(x as u32, y as u32).0;
            if pixel[3] < 48 {
                continue;
            }
            let bucket = (pixel[0] & 0xE0, pixel[1] & 0xE0, pixel[2] & 0xE0);
            *buckets.entry(bucket).or_default() += pixel[3] as usize;
        }
    }

    let mut ranked = buckets.into_iter().collect::<Vec<_>>();
    ranked.sort_by(|a, b| b.1.cmp(&a.1));

    let mut colors = Vec::new();
    for ((r, g, b), _) in ranked {
        let candidate = (
            (f64::from(r) + 15.5) / 255.0,
            (f64::from(g) + 15.5) / 255.0,
            (f64::from(b) + 15.5) / 255.0,
        );
        if is_near_white(candidate) {
            continue;
        }
        if colors
            .iter()
            .all(|existing| color_distance(*existing, candidate) >= 0.10)
        {
            colors.push(candidate);
        }
        if colors.len() >= 8 {
            break;
        }
    }
    colors
}

fn is_near_white(color: (f64, f64, f64)) -> bool {
    color.0 > 0.88 && color.1 > 0.88 && color.2 > 0.88
}

fn color_distance(a: (f64, f64, f64), b: (f64, f64, f64)) -> f64 {
    let dr = a.0 - b.0;
    let dg = a.1 - b.1;
    let db = a.2 - b.2;
    (dr * dr + dg * dg + db * db).sqrt()
}

pub fn picture_key(provider_symbol: &str) -> String {
    provider_symbol
        .trim()
        .to_ascii_uppercase()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn stock_image_path(provider_symbol: &str) -> Result<PathBuf, String> {
    let safe = picture_key(provider_symbol);
    if safe.is_empty() {
        return Err("Missing stock symbol".into());
    }
    Ok(storage::stock_pictures_dir().join(format!("{safe}.img")))
}
