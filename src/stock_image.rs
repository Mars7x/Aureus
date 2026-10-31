use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use image::GenericImageView;
use gtk::gdk::prelude::TextureExt;

use crate::storage;

const MAX_STOCK_IMAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_STOCK_IMAGE_DIMENSION: u32 = 4096;
const MAX_STOCK_IMAGE_PIXELS: u64 = 16_777_216;
const MAX_STOCK_IMAGE_DECODE_BYTES: u64 = 128 * 1024 * 1024;
const STORED_STOCK_IMAGE_DIMENSION: u32 = 512;

/// Elbstream (formerly Parqet) serves square brand logos keyed by exchange
/// symbol, including Yahoo-style suffixes such as `RY.TO` and `NTOA.MU`. Its
/// free tier requires a visible, linked attribution wherever logos appear.
const BRAND_LOGO_URL: &str = "https://api.elbstream.com/logos/symbol";
pub const BRAND_LOGO_ATTRIBUTION_URL: &str = "https://elbstream.com";
const BRAND_LOGO_REQUEST_SIZE: u32 = 256;
const MAX_BRAND_LOGO_BYTES: u64 = 2 * 1024 * 1024;
const BRAND_LOGO_REFRESH_SECONDS: u64 = 30 * 24 * 60 * 60;
const MISSING_BRAND_LOGO_RETRY_SECONDS: u64 = 7 * 24 * 60 * 60;

static BRAND_LOGOS_ENABLED: AtomicBool = AtomicBool::new(true);

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

/// Picture shown for a stock: the user's own picture always wins, then a
/// cached brand logo when brand logos are enabled.
pub fn load_display_image(provider_symbol: &str) -> Option<StockImageData> {
    if let Ok(Some(image)) = load_stock_image(provider_symbol) {
        return Some(image);
    }
    if !brand_logos_enabled() {
        return None;
    }
    let path = brand_logo_path(provider_symbol).ok()?;
    let bytes = fs::read(path).ok()?;
    stock_image_from_bytes(bytes).ok()
}

pub fn brand_logos_enabled() -> bool {
    BRAND_LOGOS_ENABLED.load(Ordering::Relaxed)
}

pub fn set_brand_logos_enabled(enabled: bool) {
    BRAND_LOGOS_ENABLED.store(enabled, Ordering::Relaxed);
}

/// Whether a background brand-logo request is worthwhile. Cached logos are
/// refreshed occasionally, and symbols the service does not know are retried
/// only after a longer delay.
pub fn brand_logo_needs_fetch(provider_symbol: &str) -> bool {
    if !brand_logos_enabled() || has_stock_image(provider_symbol) {
        return false;
    }
    let (Ok(logo), Ok(missing)) = (
        brand_logo_path(provider_symbol),
        missing_brand_logo_path(provider_symbol),
    ) else {
        return false;
    };
    let now = SystemTime::now();
    if let Some(age) = file_age(&logo, now) {
        return age >= Duration::from_secs(BRAND_LOGO_REFRESH_SECONDS);
    }
    match file_age(&missing, now) {
        Some(age) => age >= Duration::from_secs(MISSING_BRAND_LOGO_RETRY_SECONDS),
        None => true,
    }
}

fn file_age(path: &Path, now: SystemTime) -> Option<Duration> {
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    Some(now.duration_since(modified).unwrap_or_default())
}

fn brand_logo_requests() -> &'static Mutex<HashSet<String>> {
    static REQUESTS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    REQUESTS.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Claims the single in-flight request for a symbol. Returns false when another
/// widget already requested the same logo.
pub fn begin_brand_logo_request(provider_symbol: &str) -> bool {
    let mut requests = brand_logo_requests()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    requests.insert(picture_key(provider_symbol))
}

pub fn finish_brand_logo_request(provider_symbol: &str) {
    let mut requests = brand_logo_requests()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    requests.remove(&picture_key(provider_symbol));
}

fn brand_logo_agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(15)))
            .user_agent(concat!("Aureus/", env!("CARGO_PKG_VERSION")))
            .build();
        ureq::Agent::new_with_config(config)
    })
}

fn brand_logo_url(provider_symbol: &str) -> String {
    format!(
        "{BRAND_LOGO_URL}/{}?format=png&size={BRAND_LOGO_REQUEST_SIZE}",
        urlencoding::encode(provider_symbol.trim())
    )
}

/// Blocking download of a brand logo into the rebuildable cache. Returns
/// whether a logo is now cached. A 404 is the service's authoritative "no logo"
/// answer and is remembered; transient failures are not, so they are retried.
pub fn fetch_brand_logo(provider_symbol: &str) -> Result<bool, String> {
    let logo = brand_logo_path(provider_symbol)?;
    let missing = missing_brand_logo_path(provider_symbol)?;
    let mut response = match brand_logo_agent()
        .get(&brand_logo_url(provider_symbol))
        .call()
    {
        Ok(response) => response,
        Err(ureq::Error::StatusCode(404)) => {
            let _ = fs::remove_file(&logo);
            write_atomically(&missing, &[])?;
            return Ok(false);
        }
        Err(error) => return Err(format!("Could not download brand logo: {error}")),
    };
    let bytes = response
        .body_mut()
        .with_config()
        .limit(MAX_BRAND_LOGO_BYTES)
        .read_to_vec()
        .map_err(|error| format!("Could not read brand logo: {error}"))?;
    let normalized = normalize_brand_logo(&bytes)?;
    write_atomically(&logo, &normalized)?;
    let _ = fs::remove_file(&missing);
    Ok(true)
}

/// Decodes untrusted downloaded bytes under the same limits as user pictures
/// and re-encodes them as a bounded PNG, so the cache never holds raw responses.
fn normalize_brand_logo(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let decoded = decode_raster_with_limits(bytes)
        .ok_or_else(|| "The brand logo is not a supported image".to_string())?;
    validate_image_dimensions(&decoded)?;
    let image = if decoded.width() > STORED_STOCK_IMAGE_DIMENSION
        || decoded.height() > STORED_STOCK_IMAGE_DIMENSION
    {
        decoded.thumbnail(STORED_STOCK_IMAGE_DIMENSION, STORED_STOCK_IMAGE_DIMENSION)
    } else {
        decoded
    };
    let mut normalized = Cursor::new(Vec::new());
    image
        .write_to(&mut normalized, image::ImageFormat::Png)
        .map_err(|error| format!("Could not prepare brand logo: {error}"))?;
    Ok(normalized.into_inner())
}

fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("Could not create brand logo folder: {error}"))?;
    }
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, bytes).map_err(|error| format!("Could not save brand logo: {error}"))?;
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(format!("Could not finish saving brand logo: {error}"));
    }
    Ok(())
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

fn brand_logo_path(provider_symbol: &str) -> Result<PathBuf, String> {
    let safe = picture_key(provider_symbol);
    if safe.is_empty() {
        return Err("Missing stock symbol".into());
    }
    Ok(storage::brand_logos_dir().join(format!("{safe}.png")))
}

fn missing_brand_logo_path(provider_symbol: &str) -> Result<PathBuf, String> {
    let safe = picture_key(provider_symbol);
    if safe.is_empty() {
        return Err("Missing stock symbol".into());
    }
    Ok(storage::brand_logos_dir().join(format!("{safe}.missing")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let image = image::DynamicImage::new_rgba8(width, height);
        let mut bytes = Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        bytes.into_inner()
    }

    #[test]
    fn brand_logo_url_encodes_provider_symbols() {
        assert_eq!(
            brand_logo_url("RY.TO"),
            "https://api.elbstream.com/logos/symbol/RY.TO?format=png&size=256"
        );
        assert_eq!(
            brand_logo_url(" ^GSPC "),
            "https://api.elbstream.com/logos/symbol/%5EGSPC?format=png&size=256"
        );
        assert!(!brand_logo_url("A/B").contains("A/B"));
    }

    #[test]
    fn brand_logos_are_reencoded_and_bounded() {
        let normalized = normalize_brand_logo(&png(1024, 1024)).unwrap();
        let image = decode_raster_with_limits(&normalized).unwrap();
        assert_eq!(image.dimensions(), (512, 512));
        assert_eq!(&normalized[..8], b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn non_image_brand_logo_responses_are_rejected() {
        assert!(normalize_brand_logo(b"Symbol Image not found").is_err());
        assert!(normalize_brand_logo(b"<html>rate limited</html>").is_err());
        assert!(normalize_brand_logo(&png(4097, 1)).is_err());
    }

    /// Live check against Elbstream: `cargo test -- --ignored brand_logo_service`.
    #[test]
    #[ignore = "requires network access"]
    fn brand_logo_service_caches_known_and_remembers_unknown_symbols() {
        let known = "RY.TO";
        let unknown = "AUREUS-NO-SUCH-SYMBOL";
        assert!(fetch_brand_logo(known).unwrap());
        let cached = fs::read(brand_logo_path(known).unwrap()).unwrap();
        assert!(decode_raster_with_limits(&cached).is_some());

        assert!(!fetch_brand_logo(unknown).unwrap());
        assert!(missing_brand_logo_path(unknown).unwrap().exists());
        assert!(!brand_logo_path(unknown).unwrap().exists());

        for path in [
            brand_logo_path(known).unwrap(),
            missing_brand_logo_path(unknown).unwrap(),
        ] {
            let _ = fs::remove_file(path);
        }
    }

    #[test]
    fn brand_logo_cache_paths_use_safe_keys() {
        let logo = brand_logo_path("../evil/RY.TO").unwrap();
        assert_eq!(logo.file_name().unwrap(), ".._EVIL_RY.TO.png");
        assert_eq!(logo.parent().unwrap(), storage::brand_logos_dir());
        assert!(brand_logo_path("  ").is_err());
    }
}
