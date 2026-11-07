//! Pixel filters ported from CDisplay 1.8 (`TPageBMP::Balance`, `GammaCorrect`,
//! `ReduceYellowing`, `ReduceYellowingByFilling`).
//!
//! The first three were reconstructed instruction-by-instruction from the original
//! x86 binary, including its integer rounding, so output should match CDisplay.
//! The edge filler is a reimplementation from its documented behaviour.

use image::RgbaImage;

/// Result of a colour-balance pass, reported in the image-information overlay.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BalanceOutcome {
    /// Applied; contrast stretch in percent (100 = unchanged).
    Applied(i32),
    /// Image range was too narrow to balance safely (original: range/6 < 64).
    LowRange,
}

/// Histogram colour balance ("Colour Balance On (C+B)").
///
/// CDisplay builds one histogram over `R+G+B` (0..=765), discards
/// `discards_per_1000` of the pixels from the dark end (while below 64) and the
/// bright end (while above 196), then builds a single linear lookup table that
/// is applied to all three channels.
pub fn colour_balance(img: &mut RgbaImage, discards_per_1000: i32) -> BalanceOutcome {
    let mut hist = [0i32; 768];
    let mut lo: i32 = 765;
    let mut hi: i32 = 0;
    for p in img.pixels() {
        let s = p[0] as i32 + p[1] as i32 + p[2] as i32;
        hist[s as usize] += 1;
        lo = lo.min(s);
        hi = hi.max(s);
    }
    if (hi - lo) / 6 < 64 {
        return BalanceOutcome::LowRange;
    }

    let total = img.width() as i64 * img.height() as i64;
    let mut count = (total * discards_per_1000 as i64 / 1000) as i32;
    while count > 0 {
        let can_lo = lo < 64 && count >= hist[lo as usize];
        let can_hi = hi > 196 && count >= hist[hi as usize];
        if !can_lo && !can_hi {
            break;
        }
        if can_lo {
            count -= hist[lo as usize];
            lo += 1;
        }
        if hi > 196 && count >= hist[hi as usize] {
            count -= hist[hi as usize];
            hi -= 1;
        }
    }

    let span = hi - lo;
    let range = (span / 6).max(1);
    let mid = (span / 2 + lo) / 3;
    let mut lut = [0u8; 256];
    for (i, v) in lut.iter_mut().enumerate() {
        let x = (i as i32 - mid) * 127 / range + 127;
        *v = x.clamp(0, 255) as u8;
    }
    apply_lut(img, &lut);
    BalanceOutcome::Applied(12700 / range)
}

/// Gamma lookup as built by CDisplay: `lut[i] = trunc(256 * (i/256)^(1/g))`.
/// `gamma_x100` is the trackbar position (1..=700, 100 = no change).
pub fn gamma_lut(gamma_x100: i32) -> [u8; 256] {
    let g = gamma_x100.max(1) as f64 * 0.01;
    let mut lut = [0u8; 256];
    for (i, v) in lut.iter_mut().enumerate().skip(1) {
        let x = ((i as f64 * 0.00390625).powf(1.0 / g) * 256.0) as i64;
        *v = x.clamp(0, 255) as u8;
    }
    lut
}

pub fn gamma_correct(img: &mut RgbaImage, gamma_x100: i32) {
    if gamma_x100 == 100 {
        return;
    }
    apply_lut(img, &gamma_lut(gamma_x100));
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct YellowParams {
    pub red_threshold: u8,
    pub green_threshold: u8,
    pub blue_threshold: u8,
    pub green_boost: u8,
    pub blue_boost: u8,
}

impl Default for YellowParams {
    /// Registry defaults recovered from CDisplay 1.8.
    fn default() -> Self {
        Self {
            red_threshold: 235,
            green_threshold: 220,
            blue_threshold: 127,
            green_boost: 10,
            blue_boost: 20,
        }
    }
}

/// CDisplay's guard: pages whose interior already contains real white
/// (>2% of pixels with every channel above 250, ignoring a 20px border) are left alone.
fn already_has_white(img: &RgbaImage) -> bool {
    let (w, h) = (img.width() as i64, img.height() as i64);
    if w <= 40 || h <= 40 {
        return false;
    }
    let limit = w * h / 50;
    let mut n = 0i64;
    for y in 20..(h - 20) as u32 {
        for x in 20..(w - 20) as u32 {
            let p = img.get_pixel(x, y);
            if p[0] > 250 && p[1] > 250 && p[2] > 250 {
                n += 1;
            }
        }
        if n > limit {
            return true;
        }
    }
    false
}

fn ramp_lut(threshold: u8, boost: u8) -> [u8; 256] {
    let mut lut = [0u8; 256];
    let mut d: i32 = 0;
    for (i, v) in lut.iter_mut().enumerate() {
        let i = i as i32;
        if i > threshold as i32 {
            d += 3;
        }
        d = d.min(boost as i32);
        *v = if i > 255 - d { 255 } else { (i + d) as u8 };
    }
    lut
}

/// "Yellow Reduction On (C+Y)": brightens green and blue of pale-yellow pixels
/// (aged newsprint) toward white. Returns false if the page was skipped.
pub fn reduce_yellowing(img: &mut RgbaImage, p: &YellowParams) -> bool {
    if already_has_white(img) {
        return false;
    }
    let blue = ramp_lut(p.blue_threshold, p.blue_boost);
    let green = ramp_lut(p.green_threshold, p.green_boost);
    for px in img.pixels_mut() {
        if px[0] > p.red_threshold && px[1] > p.green_threshold && px[2] > p.blue_threshold {
            px[2] = blue[px[2] as usize];
            px[1] = green[px[1] as usize];
        }
    }
    true
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EdgeYellowParams {
    pub red_threshold: u8,
    pub green_threshold: u8,
    pub blue_threshold: u8,
    pub red_setting: u8,
    pub green_setting: u8,
    pub blue_setting: u8,
}

impl Default for EdgeYellowParams {
    /// Registry defaults recovered from CDisplay 1.8.
    fn default() -> Self {
        Self {
            red_threshold: 200,
            green_threshold: 150,
            blue_threshold: 120,
            red_setting: 255,
            green_setting: 240,
            blue_setting: 230,
        }
    }
}

/// "Edge Yellow Reduction": flood-fills yellowed paper connected to the page
/// border with a fixed colour. Like the original it can leak into panels that
/// aren't closed by a border line.
pub fn reduce_edge_yellowing(img: &mut RgbaImage, p: &EdgeYellowParams) {
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return;
    }
    let is_paper = |px: &image::Rgba<u8>| {
        px[0] > p.red_threshold && px[1] > p.green_threshold && px[2] > p.blue_threshold
    };
    let mut seen = vec![false; (w * h) as usize];
    let mut stack: Vec<(u32, u32)> = Vec::new();
    for x in 0..w {
        stack.push((x, 0));
        stack.push((x, h - 1));
    }
    for y in 0..h {
        stack.push((0, y));
        stack.push((w - 1, y));
    }
    let fill = image::Rgba([p.red_setting, p.green_setting, p.blue_setting, 255]);
    while let Some((x, y)) = stack.pop() {
        let idx = (y * w + x) as usize;
        if seen[idx] {
            continue;
        }
        seen[idx] = true;
        let px = img.get_pixel(x, y);
        if !is_paper(px) {
            continue;
        }
        let alpha = px[3];
        let mut f = fill;
        f[3] = alpha;
        img.put_pixel(x, y, f);
        if x > 0 {
            stack.push((x - 1, y));
        }
        if x + 1 < w {
            stack.push((x + 1, y));
        }
        if y > 0 {
            stack.push((x, y - 1));
        }
        if y + 1 < h {
            stack.push((x, y + 1));
        }
    }
}

/// "Internal yellow reduction": the same colour test applied everywhere,
/// not only to paper connected to the edge.
pub fn reduce_internal_yellowing(img: &mut RgbaImage, p: &EdgeYellowParams) {
    for px in img.pixels_mut() {
        if px[0] > p.red_threshold && px[1] > p.green_threshold && px[2] > p.blue_threshold {
            px[0] = p.red_setting;
            px[1] = p.green_setting;
            px[2] = p.blue_setting;
        }
    }
}

fn apply_lut(img: &mut RgbaImage, lut: &[u8; 256]) {
    for px in img.pixels_mut() {
        px[0] = lut[px[0] as usize];
        px[1] = lut[px[1] as usize];
        px[2] = lut[px[2] as usize];
    }
}

/// CDisplay's "Auto" background: the average colour of the page's outer edge.
pub fn edge_colour(img: &RgbaImage) -> [u8; 3] {
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return [0, 0, 0];
    }
    let mut sum = [0u64; 3];
    let mut n = 0u64;
    let mut add = |x: u32, y: u32| {
        let p = img.get_pixel(x, y);
        sum[0] += p[0] as u64;
        sum[1] += p[1] as u64;
        sum[2] += p[2] as u64;
        n += 1;
    };
    let step_x = (w / 64).max(1);
    let step_y = (h / 64).max(1);
    for x in (0..w).step_by(step_x as usize) {
        add(x, 0);
        add(x, h - 1);
    }
    for y in (0..h).step_by(step_y as usize) {
        add(0, y);
        add(w - 1, y);
    }
    [(sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8]
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    #[test]
    fn gamma_identity_and_shape() {
        let lut = gamma_lut(100);
        // trunc(256 * i/256) == i
        for (i, v) in lut.iter().enumerate() {
            assert_eq!(*v as usize, i);
        }
        let brighter = gamma_lut(200);
        assert!(brighter[64] > 64);
        assert_eq!(brighter[0], 0);
    }

    #[test]
    fn ramp_matches_original_shape() {
        let l = ramp_lut(127, 20);
        assert_eq!(l[127], 127);
        assert_eq!(l[128], 131); // +3
        assert_eq!(l[129], 135); // +6
        assert_eq!(l[140], 160); // capped at +20
        assert_eq!(l[250], 255);
    }

    #[test]
    fn yellow_reduction_whitens_pale_yellow() {
        let mut img = RgbaImage::from_pixel(100, 100, Rgba([245, 230, 180, 255]));
        assert!(reduce_yellowing(&mut img, &YellowParams::default()));
        let p = img.get_pixel(50, 50);
        assert_eq!(p.0, [245, 240, 200, 255]);
    }

    #[test]
    fn yellow_reduction_skips_white_pages() {
        let mut img = RgbaImage::from_pixel(100, 100, Rgba([255, 255, 255, 255]));
        assert!(!reduce_yellowing(&mut img, &YellowParams::default()));
    }

    #[test]
    fn balance_stretches_and_rejects_flat() {
        let mut flat = RgbaImage::from_pixel(10, 10, Rgba([100, 100, 100, 255]));
        assert_eq!(colour_balance(&mut flat, 3), BalanceOutcome::LowRange);

        let mut img = RgbaImage::new(256, 1);
        for x in 0..256 {
            let v = (40 + x * 160 / 255) as u8; // 40..200
            img.put_pixel(x, 0, Rgba([v, v, v, 255]));
        }
        match colour_balance(&mut img, 0) {
            BalanceOutcome::Applied(pct) => assert!(pct > 100),
            o => panic!("{o:?}"),
        }
        assert!(img.get_pixel(0, 0)[0] < 10);
        assert!(img.get_pixel(255, 0)[0] > 245);
    }

    #[test]
    fn edge_fill_stops_at_lines() {
        let mut img = RgbaImage::from_pixel(20, 20, Rgba([230, 200, 160, 255]));
        for i in 5..15 {
            for j in [5, 14] {
                img.put_pixel(i, j, Rgba([0, 0, 0, 255]));
                img.put_pixel(j, i, Rgba([0, 0, 0, 255]));
            }
        }
        reduce_edge_yellowing(&mut img, &EdgeYellowParams::default());
        assert_eq!(img.get_pixel(0, 0).0, [255, 240, 230, 255]);
        assert_eq!(img.get_pixel(10, 10).0, [230, 200, 160, 255]); // enclosed panel untouched
    }
}
