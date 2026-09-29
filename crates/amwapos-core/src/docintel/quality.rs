//! Page image quality and deterministic preprocessing.
//!
//! Measured on a downscaled grayscale copy: resolution, sharpness (variance
//! of the Laplacian), exposure, contrast, clipped highlights (glare), JPEG
//! compression (bytes per pixel), content touching the edges (cropping) and
//! skew. The original file is never changed; preprocessing produces separate
//! variants, and the OCR worker keeps whichever reads best by measured OCR
//! signals (mean word confidence × words), not by assumption.

use image::{DynamicImage, GenericImageView, GrayImage, Luma};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PageQuality {
    pub page: u32,
    pub width: u32,
    pub height: u32,
    /// Variance of the Laplacian (higher = sharper).
    pub sharpness: i64,
    pub brightness: i64,
    pub contrast: i64,
    /// Share of clipped white pixels, per mille.
    pub clipped_permille: i64,
    pub bytes_per_kpixel: Option<i64>,
    /// Estimated skew in tenths of a degree (positive = clockwise).
    pub skew_decideg: i64,
    /// good | usable | poor | unusable
    pub status: String,
    pub issues: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct DocQuality {
    pub status: String,
    pub pages: Vec<PageQuality>,
    pub messages: Vec<String>,
}

fn rank(s: &str) -> u8 {
    match s {
        "good" => 0,
        "usable" => 1,
        "poor" => 2,
        _ => 3,
    }
}

pub fn worst(a: &str, b: &str) -> &'static str {
    match rank(a).max(rank(b)) {
        0 => "good",
        1 => "usable",
        2 => "poor",
        _ => "unusable",
    }
}

/// Gray copy with the longest side at most `max` pixels.
pub fn gray_small(img: &DynamicImage, max: u32) -> GrayImage {
    let (w, h) = img.dimensions();
    let g = img.to_luma8();
    if w.max(h) <= max {
        return g;
    }
    let s = max as f32 / w.max(h) as f32;
    image::imageops::resize(
        &g,
        ((w as f32 * s).round() as u32).max(1),
        ((h as f32 * s).round() as u32).max(1),
        image::imageops::FilterType::Triangle,
    )
}

fn laplacian_var(g: &GrayImage) -> i64 {
    let (w, h) = g.dimensions();
    if w < 3 || h < 3 {
        return 0;
    }
    let (mut sum, mut sq, mut n) = (0f64, 0f64, 0f64);
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let c = g.get_pixel(x, y)[0] as f64;
            let v = g.get_pixel(x - 1, y)[0] as f64
                + g.get_pixel(x + 1, y)[0] as f64
                + g.get_pixel(x, y - 1)[0] as f64
                + g.get_pixel(x, y + 1)[0] as f64
                - 4.0 * c;
            sum += v;
            sq += v * v;
            n += 1.0;
        }
    }
    let mean = sum / n;
    (sq / n - mean * mean).round() as i64
}

fn stats(g: &GrayImage) -> (i64, i64, i64) {
    let n = (g.width() * g.height()).max(1) as f64;
    let mut sum = 0f64;
    let mut sq = 0f64;
    let mut clipped = 0f64;
    for p in g.pixels() {
        let v = p[0] as f64;
        sum += v;
        sq += v * v;
        if p[0] >= 253 {
            clipped += 1.0;
        }
    }
    let mean = sum / n;
    let sd = (sq / n - mean * mean).max(0.0).sqrt();
    (mean.round() as i64, sd.round() as i64, (clipped * 1000.0 / n).round() as i64)
}

/// Dark "ink" pixels in the outer border strips (content running off the page).
fn edge_ink(g: &GrayImage) -> bool {
    let (w, h) = g.dimensions();
    let bw = (w / 60).max(2);
    let bh = (h / 60).max(2);
    let dark = |x: u32, y: u32| g.get_pixel(x, y)[0] < 90;
    let mut sides = 0;
    let frac = |cnt: u32, total: u32| cnt * 100 / total.max(1);
    let mut c = 0;
    for y in 0..h {
        for x in 0..bw {
            c += dark(x, y) as u32;
        }
    }
    if frac(c, h * bw) >= 6 {
        sides += 1;
    }
    c = 0;
    for y in 0..h {
        for x in w - bw..w {
            c += dark(x, y) as u32;
        }
    }
    if frac(c, h * bw) >= 6 {
        sides += 1;
    }
    c = 0;
    for y in h - bh..h {
        for x in 0..w {
            c += dark(x, y) as u32;
        }
    }
    if frac(c, w * bh) >= 6 {
        sides += 1;
    }
    sides >= 1
}

/// Binarized row-profile variance at a small rotation: text lines are
/// horizontal when the variance of row ink counts is highest.
fn profile_score(g: &GrayImage, deg: f32) -> f64 {
    let (w, h) = g.dimensions();
    let (s, c) = deg.to_radians().sin_cos();
    let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
    let mut rows = vec![0f64; h as usize];
    for y in (0..h).step_by(1) {
        for x in (0..w).step_by(2) {
            if g.get_pixel(x, y)[0] < 128 {
                let ry = (-(x as f32 - cx) * s + (y as f32 - cy) * c + cy).round();
                if ry >= 0.0 && (ry as usize) < rows.len() {
                    rows[ry as usize] += 1.0;
                }
            }
        }
    }
    let n = rows.len() as f64;
    let mean = rows.iter().sum::<f64>() / n;
    rows.iter().map(|r| (r - mean) * (r - mean)).sum::<f64>() / n
}

/// Skew estimate within ±6°, in tenths of a degree.
pub fn estimate_skew(g: &GrayImage) -> i64 {
    let small = if g.width().max(g.height()) > 700 {
        let s = 700.0 / g.width().max(g.height()) as f32;
        image::imageops::resize(g, (g.width() as f32 * s) as u32, (g.height() as f32 * s) as u32, image::imageops::FilterType::Triangle)
    } else {
        g.clone()
    };
    let mut best = (0i64, profile_score(&small, 0.0));
    let mut d = -60;
    while d <= 60 {
        if d != 0 {
            let sc = profile_score(&small, d as f32 / 10.0);
            if sc > best.1 * 1.02 {
                best = (d, sc);
            }
        }
        d += 5;
    }
    best.0
}

/// Assess one page. `file_bytes` = size of the page's encoded image when known.
pub fn assess(img: &DynamicImage, page: u32, file_bytes: Option<u64>, is_jpeg: bool) -> PageQuality {
    let (w, h) = img.dimensions();
    let g = gray_small(img, 1400);
    let sharp = laplacian_var(&gray_small(img, 1000));
    let (bright, contrast, clipped) = stats(&g);
    let skew = estimate_skew(&g);
    let bpk = file_bytes.map(|b| (b as i64 * 1000) / (w as i64 * h as i64).max(1));
    let mut issues = vec![];
    let mut status = "good";
    let mut flag = |s: &'static str, msg: String| {
        status = worst(status, s);
        issues.push(msg);
    };
    let short = w.min(h);
    if short < 300 {
        flag("unusable", format!("very low resolution ({w}×{h}); the text cannot be read reliably"));
    } else if short < 700 {
        flag("poor", format!("low resolution ({w}×{h}); small print may be misread"));
    }
    if sharp < 60 {
        flag("poor", "heavily blurred; quantity and price values may be unreliable".into());
    } else if sharp < 180 {
        flag("usable", "slightly blurred".into());
    }
    if bright < 70 {
        flag("poor", "very dark; retake it in better light".into());
    } else if bright < 110 {
        flag("usable", "dark exposure".into());
    }
    if contrast < 22 {
        flag("poor", "very low contrast (faded or washed out)".into());
    }
    if clipped > 120 && bright < 215 {
        flag("usable", "bright glare on part of the page may hide text".into());
    }
    if is_jpeg && bpk.is_some_and(|b| b < 60) {
        flag("usable", "heavy JPEG compression".into());
    }
    if edge_ink(&g) {
        flag("usable", "content touches the edge of the image; part of the document may be cut off".into());
    }
    if skew.abs() >= 15 {
        issues.push(format!("tilted about {}.{}°; straightened for reading", skew.abs() / 10, skew.abs() % 10));
    }
    PageQuality {
        page,
        width: w,
        height: h,
        sharpness: sharp,
        brightness: bright,
        contrast,
        clipped_permille: clipped,
        bytes_per_kpixel: bpk,
        skew_decideg: skew,
        status: status.into(),
        issues,
    }
}

/// 64-bit difference hash for duplicate-page detection.
pub fn dhash(img: &DynamicImage) -> u64 {
    let g = image::imageops::resize(&img.to_luma8(), 9, 8, image::imageops::FilterType::Triangle);
    let mut h = 0u64;
    for y in 0..8 {
        for x in 0..8 {
            h = (h << 1) | (g.get_pixel(x, y)[0] > g.get_pixel(x + 1, y)[0]) as u64;
        }
    }
    h
}

/// Combine page results; `hashes` flags repeated pages.
pub fn combine(mut pages: Vec<PageQuality>, hashes: &[u64]) -> DocQuality {
    let mut messages = vec![];
    for i in 1..hashes.len() {
        if let Some(j) = (0..i).find(|j| (hashes[*j] ^ hashes[i]).count_ones() <= 3) {
            if let Some(p) = pages.get_mut(i) {
                p.issues.push(format!("looks the same as page {}", j + 1));
                p.status = worst(&p.status, "usable").into();
            }
        }
    }
    let mut status = "good";
    for p in &pages {
        status = worst(status, &p.status);
        for i in &p.issues {
            messages.push(format!("Page {}: {i}.", p.page));
        }
    }
    DocQuality { status: status.into(), pages, messages }
}

// ---------------------------------------------------------------- preprocessing

/// Rotate a grayscale image by a small angle (nearest neighbour, white fill).
pub fn rotate_small(g: &GrayImage, decideg: i64) -> GrayImage {
    if decideg == 0 {
        return g.clone();
    }
    let (w, h) = g.dimensions();
    let (s, c) = ((decideg as f32 / 10.0).to_radians()).sin_cos();
    let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
    GrayImage::from_fn(w, h, |x, y| {
        let (dx, dy) = (x as f32 - cx, y as f32 - cy);
        let sx = (dx * c - dy * s + cx).round();
        let sy = (dx * s + dy * c + cy).round();
        if sx >= 0.0 && sy >= 0.0 && (sx as u32) < w && (sy as u32) < h {
            *g.get_pixel(sx as u32, sy as u32)
        } else {
            Luma([255])
        }
    })
}

/// Stretch the 2nd–98th percentile of gray levels over the full range.
pub fn normalize_contrast(g: &GrayImage) -> GrayImage {
    let mut hist = [0u64; 256];
    for p in g.pixels() {
        hist[p[0] as usize] += 1;
    }
    let n: u64 = hist.iter().sum();
    let pick = |q: u64| {
        let mut acc = 0;
        for (i, v) in hist.iter().enumerate() {
            acc += v;
            if acc * 100 >= n * q {
                return i as i32;
            }
        }
        255
    };
    let (lo, hi) = (pick(2), pick(98));
    if hi - lo < 10 {
        return g.clone();
    }
    let mut out = g.clone();
    for p in out.pixels_mut() {
        let v = ((p[0] as i32 - lo) * 255 / (hi - lo)).clamp(0, 255);
        p[0] = v as u8;
    }
    out
}

/// Preprocessing variants to try, cheapest first. The original is always
/// first so a variant is only kept when it measurably reads better.
pub fn variants(img: &DynamicImage, q: &PageQuality) -> Vec<(&'static str, DynamicImage)> {
    let mut out = vec![("original", img.clone())];
    let g = img.to_luma8();
    let mut cleaned = normalize_contrast(&g);
    let mut name = "gray_contrast";
    if q.skew_decideg.abs() >= 8 {
        // The estimate is from a scaled copy; the angle is the same.
        cleaned = rotate_small(&cleaned, -q.skew_decideg);
        name = "deskew_contrast";
    }
    out.push((name, DynamicImage::ImageLuma8(cleaned)));
    out
}

/// Quarter-turn rotations for orientation (tried when the upright read is poor).
pub fn rotations(img: &DynamicImage) -> Vec<(u32, DynamicImage)> {
    vec![(90, img.rotate90()), (180, img.rotate180()), (270, img.rotate270())]
}

/// Text read sideways: most multi-word lines are taller than they are wide.
pub fn looks_rotated(page: &super::layout::Page) -> bool {
    let boxes: Vec<[u32; 4]> = page.lines.iter().filter(|l| l.words.len() >= 2).filter_map(|l| l.bbox()).collect();
    if boxes.len() < 3 {
        return false;
    }
    let tall = boxes.iter().filter(|b| b[3] > b[2] * 3 / 2).count();
    tall * 2 > boxes.len()
}

/// Measured OCR quality used to pick a variant: words × mean confidence,
/// counting only words with a plausible shape.
pub fn ocr_score(layout_page: &super::layout::Page) -> i64 {
    let mut words = 0i64;
    let mut conf = 0i64;
    for l in &layout_page.lines {
        for w in &l.words {
            let alnum = w.text.chars().filter(|c| c.is_alphanumeric()).count();
            if alnum >= 2 && alnum * 2 >= w.text.chars().count() {
                words += 1;
                conf += w.conf;
            }
        }
    }
    if words == 0 {
        0
    } else {
        conf / words * words.min(400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A page of dark "text" bars on white.
    fn page(w: u32, h: u32) -> GrayImage {
        GrayImage::from_fn(w, h, |x, y| {
            let row = (y / 24) % 2 == 0 && y > 60 && y < h - 60;
            let word = (x / 36) % 3 != 2 && x > 60 && x < w - 60;
            if row && word && (y % 24) < 12 {
                Luma([20])
            } else {
                Luma([245])
            }
        })
    }

    #[test]
    fn sharp_page_is_good_and_blur_is_detected() {
        let g = DynamicImage::ImageLuma8(page(1200, 1600));
        let q = assess(&g, 1, None, false);
        assert_eq!(q.status, "good", "{q:?}");
        let blurred = DynamicImage::ImageLuma8(image::imageops::blur(&page(1200, 1600), 6.0));
        let q = assess(&blurred, 2, None, false);
        assert!(q.issues.iter().any(|i| i.contains("blurred")), "{q:?}");
        assert_eq!(q.status, "poor");
    }

    #[test]
    fn low_resolution_dark_and_cropped() {
        let q = assess(&DynamicImage::ImageLuma8(page(250, 300)), 1, None, false);
        assert_eq!(q.status, "unusable");
        let dark = GrayImage::from_fn(1000, 1000, |x, y| if (x / 30 + y / 30) % 2 == 0 { Luma([10]) } else { Luma([60]) });
        let q = assess(&DynamicImage::ImageLuma8(dark), 1, None, false);
        assert!(q.issues.iter().any(|i| i.contains("dark")), "{q:?}");
        let cropped = GrayImage::from_fn(1000, 1200, |x, y| if y % 24 < 12 && x % 36 < 24 { Luma([20]) } else { Luma([245]) });
        let q = assess(&DynamicImage::ImageLuma8(cropped), 1, None, false);
        assert!(q.issues.iter().any(|i| i.contains("edge")), "{q:?}");
    }

    #[test]
    fn skew_is_estimated_and_corrected() {
        let g = page(1000, 1000);
        assert_eq!(estimate_skew(&g), 0);
        let tilted = rotate_small(&g, 30);
        let est = estimate_skew(&tilted);
        assert!((est - 30).abs() <= 10 || (est + 30).abs() <= 10, "{est}");
    }

    #[test]
    fn duplicate_pages() {
        let a = DynamicImage::ImageLuma8(page(800, 1000));
        let b = DynamicImage::ImageLuma8(GrayImage::from_fn(800, 1000, |x, _| Luma([(x % 255) as u8])));
        let pages = vec![assess(&a, 1, None, false), assess(&b, 2, None, false), assess(&a, 3, None, false)];
        let d = combine(pages, &[dhash(&a), dhash(&b), dhash(&a)]);
        assert!(d.messages.iter().any(|m| m.contains("Page 3") && m.contains("page 1")), "{:?}", d.messages);
    }

    #[test]
    fn contrast_stretch() {
        let faded = GrayImage::from_fn(100, 100, |x, _| Luma([if x < 50 { 150 } else { 200 }]));
        let s = normalize_contrast(&faded);
        assert_eq!((s.get_pixel(0, 0)[0], s.get_pixel(99, 0)[0]), (0, 255));
    }
}
