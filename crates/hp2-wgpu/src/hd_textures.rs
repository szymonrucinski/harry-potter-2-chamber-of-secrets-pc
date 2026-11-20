//! External AI-upscaled HD texture override registry and PNG loader.
//!
//! Looks up loose PNG overrides by lowercase Unreal object path
//! (`<package>.<group>.<name>.png` or `<package>.<name>.png`, e.g.
//! `hp2_master.wall.stone01.png` or `menuart.book_bg.png`) inside the
//! `HDTextures/` directory. The directory is indexed once at first use so
//! texture misses cost a single hash lookup with zero filesystem I/O.

use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

/// Decoded RGBA8 HD texture override ready for GPU upload.
pub struct HdTexture {
    pub width: u32,
    pub height: u32,
    pub has_alpha: bool,
    pub rgba: Vec<u8>,
}

struct HdTextureRegistry {
    dir: Option<PathBuf>,
    entries: HashMap<String, PathBuf>,
}

static ENABLED: AtomicBool = AtomicBool::new(true);
static REGISTRY: OnceLock<HdTextureRegistry> = OnceLock::new();

/// Returns whether external HD texture overrides are currently enabled.
pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Enables or disables external HD texture overrides.
pub fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
}

/// Returns `(override_count, directory)` for diagnostics and startup logging.
pub fn summary() -> (usize, Option<PathBuf>) {
    let reg = registry();
    (reg.entries.len(), reg.dir.clone())
}

fn registry() -> &'static HdTextureRegistry {
    REGISTRY.get_or_init(HdTextureRegistry::discover)
}

fn resolve_hd_dir() -> Option<PathBuf> {
    if let Some(env_dir) = std::env::var_os("HP2_HD_TEXTURES_DIR") {
        let p = PathBuf::from(env_dir);
        if p.is_dir() {
            return Some(p);
        }
    }

    let mut candidates: Vec<PathBuf> = Vec::new();

    #[cfg(target_os = "macos")]
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(
            PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("Harry Potter 2")
                .join("Data")
                .join("HDTextures"),
        );
    }

    #[cfg(not(target_os = "macos"))]
    {
        if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
            candidates.push(
                PathBuf::from(xdg)
                    .join("Harry Potter 2")
                    .join("Data")
                    .join("HDTextures"),
            );
        }
        if let Some(home) = std::env::var_os("HOME") {
            candidates.push(
                PathBuf::from(home)
                    .join(".local")
                    .join("share")
                    .join("Harry Potter 2")
                    .join("Data")
                    .join("HDTextures"),
            );
        }
    }

    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("HDTextures"));
        if let Some(parent) = cwd.parent() {
            candidates.push(parent.join("HDTextures"));
        }
    }

    candidates.into_iter().find(|p| p.is_dir())
}

fn index_dir(root: &Path, dir: &Path, entries: &mut HashMap<String, PathBuf>) {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.is_dir() {
            index_dir(root, &path, entries);
            continue;
        }
        let is_png = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("png"));
        if !is_png {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if dir == root {
            entries.insert(stem.to_ascii_lowercase(), path);
        } else if let Ok(rel) = path.strip_prefix(root) {
            let mut key = String::new();
            for comp in rel.components() {
                if let Some(part) = comp.as_os_str().to_str() {
                    if !key.is_empty() {
                        key.push('.');
                    }
                    key.push_str(part);
                }
            }
            if let Some(trimmed) = key.strip_suffix(".png").or_else(|| key.strip_suffix(".PNG")) {
                entries.insert(trimmed.to_ascii_lowercase(), path);
            }
        }
    }
}

impl HdTextureRegistry {
    fn discover() -> Self {
        let dir = resolve_hd_dir();
        let mut entries = HashMap::new();
        if let Some(ref root) = dir {
            index_dir(root, root, &mut entries);
        }
        Self { dir, entries }
    }
}

/// Looks up and decodes an HD PNG override for `object_path` (e.g.
/// `hp2_master.wall.stone01`). Returns `None` if HD textures are disabled,
/// no override exists, the PNG fails to decode, or `masked` is true and the
/// PNG carries no alpha channel.
pub fn load(object_path: &str, masked: bool) -> Option<HdTexture> {
    if !is_enabled() {
        return None;
    }
    let key = object_path.to_ascii_lowercase();
    let path = registry().entries.get(&key)?;
    decode_png(path, masked)
}

fn decode_png(path: &Path, masked: bool) -> Option<HdTexture> {
    let file = File::open(path).ok()?;
    let mut decoder = png::Decoder::new(BufReader::new(file));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().ok()?;
    let buf_size = reader.output_buffer_size()?;
    let mut buf = vec![0u8; buf_size];
    let info = reader.next_frame(&mut buf).ok()?;
    let width = info.width;
    let height = info.height;
    if width == 0 || height == 0 {
        return None;
    }
    let pixel_count = (width as usize).checked_mul(height as usize)?;
    let bytes = &buf[..info.buffer_size()];

    let (has_alpha, rgba) = match info.color_type {
        png::ColorType::Rgba => {
            if bytes.len() != pixel_count.checked_mul(4)? {
                return None;
            }
            (true, bytes.to_vec())
        }
        png::ColorType::Rgb => {
            if masked || bytes.len() != pixel_count.checked_mul(3)? {
                return None;
            }
            let mut out = Vec::with_capacity(pixel_count.checked_mul(4)?);
            for rgb in bytes.chunks_exact(3) {
                out.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
            (false, out)
        }
        png::ColorType::GrayscaleAlpha => {
            if bytes.len() != pixel_count.checked_mul(2)? {
                return None;
            }
            let mut out = Vec::with_capacity(pixel_count.checked_mul(4)?);
            for ga in bytes.chunks_exact(2) {
                out.extend_from_slice(&[ga[0], ga[0], ga[0], ga[1]]);
            }
            (true, out)
        }
        png::ColorType::Grayscale => {
            if masked || bytes.len() != pixel_count {
                return None;
            }
            let mut out = Vec::with_capacity(pixel_count.checked_mul(4)?);
            for &g in bytes {
                out.extend_from_slice(&[g, g, g, 255]);
            }
            (false, out)
        }
        png::ColorType::Indexed => return None,
    };

    Some(HdTexture {
        width,
        height,
        has_alpha,
        rgba,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_round_trips() {
        let prev = is_enabled();
        set_enabled(false);
        assert!(!is_enabled());
        set_enabled(true);
        assert!(is_enabled());
        set_enabled(prev);
    }

    #[test]
    fn decode_png_handles_rgb_and_rgba_and_respects_masked_flag() {
        let dir = std::env::temp_dir().join(format!("hp2_hd_tex_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);

        // Write a 2x1 RGB PNG (opaque).
        let rgb_path = dir.join("opaque.png");
        {
            let file = File::create(&rgb_path).unwrap();
            let mut encoder = png::Encoder::new(file, 2, 1);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&[10, 20, 30, 40, 50, 60]).unwrap();
        }
        let decoded_rgb = decode_png(&rgb_path, false).expect("unmasked RGB should decode");
        assert_eq!((decoded_rgb.width, decoded_rgb.height), (2, 1));
        assert!(!decoded_rgb.has_alpha);
        assert_eq!(decoded_rgb.rgba, vec![10, 20, 30, 255, 40, 50, 60, 255]);
        assert!(
            decode_png(&rgb_path, true).is_none(),
            "opaque RGB PNG must be rejected for masked texture lookup"
        );

        // Write a 2x1 RGBA PNG (masked).
        let rgba_path = dir.join("masked.png");
        {
            let file = File::create(&rgba_path).unwrap();
            let mut encoder = png::Encoder::new(file, 2, 1);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer
                .write_image_data(&[10, 20, 30, 0, 40, 50, 60, 255])
                .unwrap();
        }
        let decoded_rgba = decode_png(&rgba_path, true).expect("masked RGBA should decode");
        assert_eq!((decoded_rgba.width, decoded_rgba.height), (2, 1));
        assert!(decoded_rgba.has_alpha);
        assert_eq!(decoded_rgba.rgba, vec![10, 20, 30, 0, 40, 50, 60, 255]);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
