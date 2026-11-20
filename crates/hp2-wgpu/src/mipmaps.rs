//! CPU mip-chain generation for uncompressed textures.
//!
//! The ported upload path sent only mip 0 for RGBA8 textures (DXT1 textures carry their own
//! chain), so distant surfaces were minified from full resolution and shimmered. The chain is a
//! 2x2 box filter: computed in linear light for sRGB textures, colour weighted by alpha so cut-out
//! edges do not darken, and for textures with transparency the alpha of every level is rescaled
//! so the fraction of texels that pass the 50% mask test matches mip 0 (masked foliage and fences
//! otherwise thin out and vanish with distance).

use std::sync::LazyLock;

/// Number of levels in a full chain down to 1x1.
pub fn level_count(width: u32, height: u32) -> u32 {
    32 - width.max(height).max(1).leading_zeros()
}

fn srgb_to_linear(v: u8) -> f32 {
    let c = f32::from(v) / 255.;
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn linear_to_srgb(c: f32) -> u8 {
    let c = c.clamp(0., 1.);
    let v = if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * c.powf(1. / 2.4) - 0.055 };
    (v * 255. + 0.5) as u8
}

/// Precomputed exact sRGB u8 -> linear f32 lookup table (avoids millions of `powf` calls on HD textures).
static SRGB_TO_LINEAR_LUT: LazyLock<[f32; 256]> =
    LazyLock::new(|| std::array::from_fn(|i| srgb_to_linear(i as u8)));

/// Precomputed linear f32 -> sRGB u8 lookup table (16 KB, fits in L1 cache, <=0.2 LSB quantization error).
const LINEAR_LUT_LEN: usize = 16384;
const LINEAR_LUT_MAX: f32 = (LINEAR_LUT_LEN - 1) as f32;
static LINEAR_TO_SRGB_LUT: LazyLock<Box<[u8; LINEAR_LUT_LEN]>> = LazyLock::new(|| {
    let mut table = Box::new([0u8; LINEAR_LUT_LEN]);
    for (i, slot) in table.iter_mut().enumerate() {
        *slot = linear_to_srgb(i as f32 / LINEAR_LUT_MAX);
    }
    table
});

#[inline]
fn linear_to_srgb_fast(c: f32, lut: &[u8; LINEAR_LUT_LEN]) -> u8 {
    let idx = (c.clamp(0., 1.) * LINEAR_LUT_MAX + 0.5) as usize;
    lut[idx.min(LINEAR_LUT_LEN - 1)]
}

/// Fraction of texels whose alpha would pass a 0.5 test after scaling by `scale`.
fn coverage(rgba: &[u8], scale: f32) -> f32 {
    let texels = rgba.len() / 4;
    if texels == 0 {
        return 0.;
    }
    let threshold = 127.5 / scale.max(f32::MIN_POSITIVE);
    if threshold > 255.0 {
        return 0.;
    }
    let min_alpha = threshold.ceil() as u8;
    let pass = rgba.chunks_exact(4).filter(|p| p[3] >= min_alpha).count();
    pass as f32 / texels as f32
}

/// Scales `rgba`'s alpha so its 0.5-test coverage is closest to `target`.
fn preserve_coverage(rgba: &mut [u8], target: f32) {
    let (mut lo, mut hi) = (0.0_f32, 8.0_f32);
    for _ in 0..16 {
        let mid = (lo + hi) / 2.;
        if coverage(rgba, mid) < target { lo = mid } else { hi = mid }
    }
    // Coverage moves in steps; take whichever bound lands closer to the target.
    let scale = if (coverage(rgba, lo) - target).abs() < (coverage(rgba, hi) - target).abs() { lo } else { hi };
    for p in rgba.chunks_exact_mut(4) {
        p[3] = (f32::from(p[3]) * scale).round().clamp(0., 255.) as u8;
    }
}

/// Levels 1.. of the chain for a `width` x `height` RGBA8 image (level 0 is the input).
pub fn generate(width: u32, height: u32, rgba: &[u8], srgb: bool) -> Vec<(u32, u32, Vec<u8>)> {
    assert_eq!(rgba.len(), width as usize * height as usize * 4);
    let translucent = rgba.chunks_exact(4).any(|p| p[3] < 255);
    let target = if translucent { coverage(rgba, 1.) } else { 1. };
    let srgb_lut = srgb.then(|| &*SRGB_TO_LINEAR_LUT);
    let lin_lut = srgb.then(|| &**LINEAR_TO_SRGB_LUT);

    let total_levels = level_count(width, height).saturating_sub(1) as usize;
    let mut levels: Vec<(u32, u32, Vec<u8>)> = Vec::with_capacity(total_levels);
    let (mut w, mut h) = (width.max(1), height.max(1));

    while w > 1 || h > 1 {
        let source: &[u8] = match levels.last() {
            Some((_, _, prev)) => prev.as_slice(),
            None => rgba,
        };
        let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
        let mut out = vec![0_u8; nw as usize * nh as usize * 4];
        let stride = w as usize * 4;
        let even_box = w >= 2 && h >= 2 && w.is_multiple_of(2) && h.is_multiple_of(2);

        if !translucent && !srgb && even_box {
            // Fast integer box filter for opaque linear textures (auto-vectorizes on NEON/SSE).
            for y in 0..nh as usize {
                let r0 = &source[(y * 2) * stride..(y * 2 + 1) * stride];
                let r1 = &source[(y * 2 + 1) * stride..(y * 2 + 2) * stride];
                let dst_row = &mut out[y * nw as usize * 4..(y + 1) * nw as usize * 4];
                for x in 0..nw as usize {
                    let s0 = x * 8;
                    let s1 = s0 + 4;
                    let d = x * 4;
                    dst_row[d] = ((u16::from(r0[s0]) + u16::from(r0[s1]) + u16::from(r1[s0]) + u16::from(r1[s1]) + 2) >> 2) as u8;
                    dst_row[d + 1] = ((u16::from(r0[s0 + 1]) + u16::from(r0[s1 + 1]) + u16::from(r1[s0 + 1]) + u16::from(r1[s1 + 1]) + 2) >> 2) as u8;
                    dst_row[d + 2] = ((u16::from(r0[s0 + 2]) + u16::from(r0[s1 + 2]) + u16::from(r1[s0 + 2]) + u16::from(r1[s1 + 2]) + 2) >> 2) as u8;
                    dst_row[d + 3] = 255;
                }
            }
        } else if !translucent && srgb && even_box {
            // Fast LUT linear-light box filter for opaque sRGB textures (zero powf, zero alpha division).
            let s2l = srgb_lut.expect("srgb enabled");
            let l2s = lin_lut.expect("srgb enabled");
            for y in 0..nh as usize {
                let r0 = &source[(y * 2) * stride..(y * 2 + 1) * stride];
                let r1 = &source[(y * 2 + 1) * stride..(y * 2 + 2) * stride];
                let dst_row = &mut out[y * nw as usize * 4..(y + 1) * nw as usize * 4];
                for x in 0..nw as usize {
                    let s0 = x * 8;
                    let s1 = s0 + 4;
                    let d = x * 4;
                    for c in 0..3 {
                        let avg = 0.25 * (
                            s2l[r0[s0 + c] as usize]
                            + s2l[r0[s1 + c] as usize]
                            + s2l[r1[s0 + c] as usize]
                            + s2l[r1[s1 + c] as usize]
                        );
                        dst_row[d + c] = linear_to_srgb_fast(avg, l2s);
                    }
                    dst_row[d + 3] = 255;
                }
            }
        } else {
            // General path (handles odd/1D tails and alpha-weighted translucent/masked textures).
            let s2l = srgb_lut;
            let l2s = lin_lut;
            for y in 0..nh {
                let sy0 = (y * 2).min(h - 1) as usize * stride;
                let sy1 = (y * 2 + 1).min(h - 1) as usize * stride;
                for x in 0..nw {
                    let sx0 = (x * 2).min(w - 1) as usize * 4;
                    let sx1 = (x * 2 + 1).min(w - 1) as usize * 4;
                    let indices = [sy0 + sx0, sy0 + sx1, sy1 + sx0, sy1 + sx1];
                    let mut color = [0_f32; 3];
                    let (mut alpha, mut weight, mut plain) = (0_f32, 0_f32, [0_f32; 3]);
                    for i in indices {
                        let a = f32::from(source[i + 3]) * (1. / 255.);
                        if let Some(tbl) = s2l {
                            for c in 0..3 {
                                let v = tbl[source[i + c] as usize];
                                color[c] += v * a;
                                plain[c] += v;
                            }
                        } else {
                            for c in 0..3 {
                                let v = f32::from(source[i + c]) * (1. / 255.);
                                color[c] += v * a;
                                plain[c] += v;
                            }
                        }
                        alpha += a;
                        weight += a;
                    }
                    let o = (y as usize * nw as usize + x as usize) * 4;
                    let inv_weight = if weight > 0. { 1. / weight } else { 0. };
                    for c in 0..3 {
                        // Alpha-weighted where anything is visible; plain average for fully clear texels.
                        let v = if weight > 0. { color[c] * inv_weight } else { plain[c] * 0.25 };
                        out[o + c] = if let Some(tbl) = l2s {
                            linear_to_srgb_fast(v, tbl)
                        } else {
                            (v.clamp(0., 1.) * 255. + 0.5) as u8
                        };
                    }
                    out[o + 3] = (alpha * (255. * 0.25) + 0.5) as u8;
                }
            }
            if translucent && target > 0. && target < 1. {
                preserve_coverage(&mut out, target);
            }
        }

        levels.push((nw, nh, out));
        w = nw;
        h = nh;
    }
    levels
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_reaches_one_by_one() {
        assert_eq!(level_count(256, 64), 9);
        assert_eq!(level_count(1, 1), 1);
        let levels = generate(4, 2, &[255; 32], false);
        assert_eq!(levels.iter().map(|(w, h, _)| (*w, *h)).collect::<Vec<_>>(), vec![(2, 1), (1, 1)]);
    }

    #[test]
    fn opaque_averages_and_srgb_averages_in_linear_light() {
        let px = |v: u8| [v, v, v, 255];
        let img: Vec<u8> = [px(0), px(255), px(0), px(255)].concat();
        assert_eq!(generate(2, 2, &img, false)[0].2, vec![128, 128, 128, 255]);
        // Linear mid-grey is about 188 in sRGB, not 128.
        assert_eq!(generate(2, 2, &img, true)[0].2[0], 188);
    }

    #[test]
    fn clear_texels_do_not_darken_visible_colour() {
        let img: Vec<u8> = [[200, 100, 50, 255], [0, 0, 0, 0], [0, 0, 0, 0], [200, 100, 50, 255]].concat();
        let level = &generate(2, 2, &img, false)[0].2;
        assert_eq!(&level[..3], &[200, 100, 50]);
    }

    #[test]
    fn masked_coverage_is_preserved_down_the_chain() {
        // An opaque disc on a clear background (a leaf or a fence post): coverage must stay near
        // the original fraction instead of shrinking with every level.
        let mut img = Vec::new();
        for y in 0..32 {
            for x in 0..32 {
                let (dx, dy) = (x as f32 - 15.5, y as f32 - 15.5);
                img.extend_from_slice(&[0, 255, 0, if dx * dx + dy * dy < 81. { 255 } else { 0 }]);
            }
        }
        let target = coverage(&img, 1.);
        for (_, _, level) in generate(32, 32, &img, false).iter().take(3) {
            assert!((coverage(level, 1.) - target).abs() < 0.08, "coverage {} vs {target}", coverage(level, 1.));
        }
    }
}
