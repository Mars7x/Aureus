use std::collections::HashMap;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use image::GenericImageView;
use gtk::gdk::prelude::TextureExt;

use crate::storage;

const MAX_STOCK_IMAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_STOCK_IMAGE_DIMENSION: u32 = 4096;
const MAX_STOCK_IMAGE_PIXELS: u64 = 16_777_216;
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

pub async fn save_stock_image(
    provider_symbol: &str,
    source: &Path,
) -> Result<StockImageData, String> {
    let metadata = fs::metadata(source)
        .map_err(|error| format!("Could not read selected picture: {error}"))?;
    if metadata.len() > MAX_STOCK_IMAGE_BYTES as u64 {
        return Err("The selected picture is larger than 8 MB".into());
    }

    // Glycin is the GNOME image-loading path. It detects the real file type,
    // decodes it in its loader sandbox, and handles raster and vector formats
    // (including SVG) through the configured runtime loaders.
    let file = gtk::gio::File::for_path(source);

    // Prefer Glycin's normal sandbox selection. In installed Flatpaks this
    // normally uses flatpak-spawn for an additional image-loader sandbox.
    // Local Builder/flatpak-builder development runs are not always installed
    // under the final app ID, so that nested sandbox can fail to launch even
    // though the configured image loader itself is available. If that happens,
    // retry Glycin without the nested sandbox; Aureus is still contained by its
    // surrounding Flatpak sandbox.
    let glycin_image = match glycin::Loader::new(file.clone()).load().await {
        Ok(image) => image,
        Err(sandbox_error) => {
            let mut fallback_loader = glycin::Loader::new(file);
            fallback_loader.sandbox_selector(glycin::SandboxSelector::NotSandboxed);
            fallback_loader.load().await.map_err(|loader_error| {
                format!(
                    "Glycin could not load this image (sandboxed: {sandbox_error}; fallback: {loader_error})"
                )
            })?
        }
    };

    let details = glycin_image.details();
    validate_dimensions(details.width(), details.height())?;
    let (target_width, target_height) = fitted_dimensions(
        details.width(),
        details.height(),
        STORED_STOCK_IMAGE_DIMENSION,
    );

    let frame = glycin_image
        .specific_frame(glycin::FrameRequest::new().scale(target_width, target_height))
        .await
        .map_err(|_| "Could not decode the selected picture".to_string())?;
    validate_dimensions(frame.width(), frame.height())?;

    // FrameRequest scaling is advisory, so normalize once more after converting
    // the Glycin frame to PNG. This guarantees stored pictures remain bounded.
    let texture = frame.texture();
    let png = texture.save_to_png_bytes();
    let decoded = decode_raster_with_limits(png.as_ref())
        .ok_or_else(|| "Could not prepare the selected picture".to_string())?;
    validate_image_dimensions(&decoded)?;

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

fn fitted_dimensions(width: u32, height: u32, maximum: u32) -> (u32, u32) {
    if width <= maximum && height <= maximum {
        return (width.max(1), height.max(1));
    }

    if width >= height {
        let scaled_height = ((u64::from(height) * u64::from(maximum)) / u64::from(width))
            .max(1) as u32;
        (maximum, scaled_height)
    } else {
        let scaled_width = ((u64::from(width) * u64::from(maximum)) / u64::from(height))
            .max(1) as u32;
        (scaled_width, maximum)
    }
}

fn stock_image_limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_STOCK_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_STOCK_IMAGE_DIMENSION);
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
    validate_dimensions(width, height)
}

fn validate_dimensions(width: u32, height: u32) -> Result<(), String> {
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if width == 0
        || height == 0
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
