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
    searched: Vec<PathBuf>,
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

/// One startup log line: whether overrides are on, how many PNGs were indexed and from where,
/// or every directory that was searched when none exists. The folder is indexed once per run,
/// so a folder created after launch needs a restart.
pub fn summary_line(enabled: bool) -> String {
    let reg = registry();
    match &reg.dir {
        Some(dir) => format!(
            "WgpuDrv: HDTextures={enabled} ({} overrides indexed from {})",
            reg.entries.len(),
            dir.display()
        ),
        None => {
            let searched: Vec<String> = reg.searched.iter().map(|p| p.display().to_string()).collect();
            format!(
                "WgpuDrv: HDTextures={enabled} (0 overrides: no HDTextures folder; searched {})",
                if searched.is_empty() { "nothing".to_owned() } else { searched.join(", ") }
            )
        }
    }
}

fn registry() -> &'static HdTextureRegistry {
    REGISTRY.get_or_init(HdTextureRegistry::discover)
}

/// Every directory an `HDTextures` folder is looked for in, in priority order.
fn candidate_dirs() -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(env_dir) = std::env::var_os("HP2_HD_TEXTURES_DIR") {
        candidates.push(PathBuf::from(env_dir));
    }

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

    candidates
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
        let searched = candidate_dirs();
        let dir = searched.iter().find(|p| p.is_dir()).cloned();
        let mut entries = HashMap::new();
        if let Some(ref root) = dir {
            index_dir(root, root, &mut entries);
        }
        Self { dir, searched, entries }
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
    let (out_color, _) = reader.output_color_type();
    if masked && matches!(out_color, png::ColorType::Rgb | png::ColorType::Grayscale) {
        return None;
    }
    let header_width = reader.info().width;
    let header_height = reader.info().height;
    if header_width == 0 || header_height == 0 {
        return None;
    }
    let header_rgba_len = (header_width as usize)
        .checked_mul(header_height as usize)?
        .checked_mul(4)?;
    let buf_size = reader.output_buffer_size()?;
    let mut buf = vec![0u8; header_rgba_len.max(buf_size)];
    let info = reader.next_frame(&mut buf[..buf_size]).ok()?;
    let width = info.width;
    let height = info.height;
    if width == 0 || height == 0 {
        return None;
    }
    let pixel_count = (width as usize).checked_mul(height as usize)?;
    let rgba_len = pixel_count.checked_mul(4)?;
    let frame_len = info.buffer_size();

    let has_alpha = match info.color_type {
        png::ColorType::Rgba => {
            if frame_len != rgba_len {
                return None;
            }
            buf.truncate(rgba_len);
            true
        }
        png::ColorType::Rgb => {
            if masked || frame_len != pixel_count.checked_mul(3)? {
                return None;
            }
            if buf.len() < rgba_len {
                buf.resize(rgba_len, 0);
            }
            for i in (0..pixel_count).rev() {
                let s = i * 3;
                let d = i * 4;
                let [r, g, b] = [buf[s], buf[s + 1], buf[s + 2]];
                buf[d] = r;
                buf[d + 1] = g;
                buf[d + 2] = b;
                buf[d + 3] = 255;
            }
            buf.truncate(rgba_len);
            false
        }
        png::ColorType::GrayscaleAlpha => {
            if frame_len != pixel_count.checked_mul(2)? {
                return None;
            }
            if buf.len() < rgba_len {
                buf.resize(rgba_len, 0);
            }
            for i in (0..pixel_count).rev() {
                let s = i * 2;
                let d = i * 4;
                let g = buf[s];
                let a = buf[s + 1];
                buf[d] = g;
                buf[d + 1] = g;
                buf[d + 2] = g;
                buf[d + 3] = a;
            }
            buf.truncate(rgba_len);
            true
        }
        png::ColorType::Grayscale => {
            if masked || frame_len != pixel_count {
                return None;
            }
            if buf.len() < rgba_len {
                buf.resize(rgba_len, 0);
            }
            for i in (0..pixel_count).rev() {
                let d = i * 4;
                let g = buf[i];
                buf[d] = g;
                buf[d + 1] = g;
                buf[d + 2] = g;
                buf[d + 3] = 255;
            }
            buf.truncate(rgba_len);
            false
        }
        png::ColorType::Indexed => return None,
    };

    Some(HdTexture {
        width,
        height,
        has_alpha,
        rgba: buf,
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
    fn index_keys_match_lowercase_object_paths() {
        let dir = std::env::temp_dir().join(format!("hp2_hd_index_test_{}", std::process::id()));
        let nested = dir.join("HP2_Master").join("Walls");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(dir.join("HP2_Menu.Icons.HP2TopToolbarLeft.png"), b"").unwrap();
        std::fs::write(dir.join("menuart.coslogo1.png"), b"").unwrap();
        std::fs::write(nested.join("HWStone1.PNG"), b"").unwrap();
        std::fs::write(dir.join("notes.txt"), b"").unwrap();
        let mut entries = HashMap::new();
        index_dir(&dir, &dir, &mut entries);
        let mut keys: Vec<_> = entries.keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, ["hp2_master.walls.hwstone1", "hp2_menu.icons.hp2toptoolbarleft", "menuart.coslogo1"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn decode_png_handles_rgb_and_rgba_and_respects_masked_flag() {
        let dir = std::env::temp_dir().join(format!("hp2_hd_tex_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);

        // Write a 3x2 RGB PNG (opaque) to cover multi-pixel backwards in-place expansion.
        let rgb_path = dir.join("opaque.png");
        let rgb_pixels: [u8; 18] = [
            10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120, 130, 140, 150, 160, 170, 180,
        ];
        {
            let file = File::create(&rgb_path).unwrap();
            let mut encoder = png::Encoder::new(file, 3, 2);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&rgb_pixels).unwrap();
        }
        let decoded_rgb = decode_png(&rgb_path, false).expect("unmasked RGB should decode");
        assert_eq!((decoded_rgb.width, decoded_rgb.height), (3, 2));
        assert!(!decoded_rgb.has_alpha);
        assert_eq!(
            decoded_rgb.rgba,
            vec![
                10, 20, 30, 255, 40, 50, 60, 255, 70, 80, 90, 255, 100, 110, 120, 255, 130, 140,
                150, 255, 160, 170, 180, 255,
            ]
        );
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
