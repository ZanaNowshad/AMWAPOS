//! Raster rendering of receipt lines for thermal printers.
//!
//! Thermal printers' built-in character sets cannot print Arabic (joined,
//! right-to-left script). Lines that contain anything outside printable ASCII
//! are therefore shaped (rustybuzz, full OpenType Arabic shaping), ordered with
//! the Unicode bidi algorithm and rasterized into a 1-bit image that is sent
//! with the standard ESC/POS raster command `GS v 0`, which every 80 mm and
//! 58 mm ESC/POS printer supports. Pure ASCII lines keep using the printer's
//! fast text mode.
//!
//! Fonts: Noto Sans Arabic + Noto Sans (SIL OFL 1.1), subset and embedded.

use std::sync::OnceLock;

use ab_glyph_rasterizer::{point, Point, Rasterizer};
use rustybuzz::{Direction, Face, UnicodeBuffer};
use unicode_bidi::BidiInfo;

static AR_REGULAR: &[u8] = include_bytes!("../assets/fonts/NotoSansArabic-Regular.ttf");
static AR_BOLD: &[u8] = include_bytes!("../assets/fonts/NotoSansArabic-Bold.ttf");
static LA_REGULAR: &[u8] = include_bytes!("../assets/fonts/NotoSans-Regular.ttf");
static LA_BOLD: &[u8] = include_bytes!("../assets/fonts/NotoSans-Bold.ttf");

/// Pixels per printer character column (ESC/POS Font A is 12×24 dots).
pub const DOTS_PER_CHAR: usize = 12;

struct Fonts {
    arabic: Face<'static>,
    latin: Face<'static>,
}

fn fonts(bold: bool) -> &'static Fonts {
    static REGULAR: OnceLock<Fonts> = OnceLock::new();
    static BOLD: OnceLock<Fonts> = OnceLock::new();
    let (cell, ar, la) = if bold { (&BOLD, AR_BOLD, LA_BOLD) } else { (&REGULAR, AR_REGULAR, LA_REGULAR) };
    cell.get_or_init(|| Fonts {
        arabic: Face::from_slice(ar, 0).expect("embedded Arabic font is valid"),
        latin: Face::from_slice(la, 0).expect("embedded Latin font is valid"),
    })
}

/// True when a line cannot be printed with the printer's built-in ASCII font.
pub fn needs_raster(s: &str) -> bool {
    s.chars().any(|c| !c.is_ascii() || c.is_ascii_control())
}

/// True when the paragraph's base direction is right-to-left (first strong
/// character is Arabic).
pub fn is_rtl(s: &str) -> bool {
    BidiInfo::new(s, None).paragraphs.first().map(|p| p.level.is_rtl()).unwrap_or(false)
}

fn is_arabic(c: char) -> bool {
    matches!(c as u32, 0x0600..=0x06FF | 0x0750..=0x077F | 0x08A0..=0x08FF | 0xFB50..=0xFDFF | 0xFE70..=0xFEFF)
}

/// A 1-bit image, rows packed MSB-first, `width` a multiple of 8.
#[derive(Debug, Clone, PartialEq)]
pub struct Bitmap {
    pub width: usize,
    pub height: usize,
    pub bits: Vec<u8>,
}

impl Bitmap {
    pub fn new(width: usize, height: usize) -> Self {
        let width = width.div_ceil(8) * 8;
        Self { width, height, bits: vec![0; width / 8 * height] }
    }
    pub fn get(&self, x: usize, y: usize) -> bool {
        self.bits[y * self.width / 8 + x / 8] & (0x80 >> (x % 8)) != 0
    }
    pub fn set(&mut self, x: usize, y: usize) {
        self.bits[y * self.width / 8 + x / 8] |= 0x80 >> (x % 8);
    }
    /// Number of black dots (used by tests).
    pub fn ink(&self) -> usize {
        self.bits.iter().map(|b| b.count_ones() as usize).sum()
    }
    /// Leftmost and rightmost inked columns.
    pub fn ink_span(&self) -> Option<(usize, usize)> {
        let cols: Vec<usize> = (0..self.width).filter(|&x| (0..self.height).any(|y| self.get(x, y))).collect();
        Some((*cols.first()?, *cols.last()?))
    }
    /// Portable bitmap (P4) — for previews and debugging.
    pub fn to_pbm(&self) -> Vec<u8> {
        let mut out = format!("P4\n{} {}\n", self.width, self.height).into_bytes();
        out.extend_from_slice(&self.bits);
        out
    }
    /// Stack bitmaps of equal width vertically.
    pub fn stack(parts: &[Bitmap]) -> Bitmap {
        let width = parts.iter().map(|b| b.width).max().unwrap_or(8);
        let mut out = Bitmap::new(width, parts.iter().map(|b| b.height).sum());
        let mut y0 = 0;
        for b in parts {
            for y in 0..b.height {
                for x in 0..b.width {
                    if b.get(x, y) {
                        out.set(x, y0 + y);
                    }
                }
            }
            y0 += b.height;
        }
        out
    }
    /// ESC/POS `GS v 0` raster bit image (normal density).
    pub fn to_escpos(&self) -> Vec<u8> {
        let bx = self.width / 8;
        let mut out =
            vec![0x1D, 0x76, 0x30, 0x00, (bx & 0xFF) as u8, (bx >> 8) as u8, (self.height & 0xFF) as u8, (self.height >> 8) as u8];
        out.extend_from_slice(&self.bits);
        out
    }
}

/// Glyphs of one shaped run, positioned in pixels relative to the run start.
struct ShapedRun {
    face: &'static Face<'static>,
    scale: f32,
    glyphs: Vec<(u16, f32, f32)>, // glyph id, x, y offset (y up)
    advance: f32,
}

/// Split `text` (one visual-order bidi run) into font segments and shape them.
fn shape_run(text: &str, rtl: bool, size_px: f32, f: &'static Fonts) -> Vec<ShapedRun> {
    // Segment by font: Arabic letters use the Arabic face; neutrals (spaces,
    // digits, punctuation) stay with the neighbouring segment when it has them.
    let mut segments: Vec<(bool, String)> = vec![];
    for c in text.chars() {
        let want_ar = if is_arabic(c) {
            true
        } else if c.is_alphabetic() {
            false
        } else {
            match segments.last() {
                Some((ar, _)) if *ar => f.arabic.glyph_index(c).is_some(),
                Some((ar, _)) => *ar,
                None => rtl && f.arabic.glyph_index(c).is_some(),
            }
        };
        match segments.last_mut() {
            Some((ar, s)) if *ar == want_ar => s.push(c),
            _ => segments.push((want_ar, c.to_string())),
        }
    }
    if rtl {
        segments.reverse();
    }
    segments
        .into_iter()
        .map(|(ar, s)| {
            let face: &'static Face<'static> = if ar { &f.arabic } else { &f.latin };
            let scale = size_px / face.units_per_em() as f32;
            let mut buf = UnicodeBuffer::new();
            buf.push_str(&s);
            buf.set_direction(if rtl { Direction::RightToLeft } else { Direction::LeftToRight });
            buf.guess_segment_properties();
            let out = rustybuzz::shape(face, &[], buf);
            let mut x = 0.0f32;
            let mut glyphs = vec![];
            for (info, pos) in out.glyph_infos().iter().zip(out.glyph_positions()) {
                glyphs.push((info.glyph_id as u16, x + pos.x_offset as f32 * scale, pos.y_offset as f32 * scale));
                x += pos.x_advance as f32 * scale;
            }
            ShapedRun { face, scale, glyphs, advance: x }
        })
        .collect()
}

/// Shape a single logical line into visual-order runs.
fn shape_line(text: &str, size_px: f32, bold: bool) -> Vec<ShapedRun> {
    let f = fonts(bold);
    if text.is_empty() {
        return vec![];
    }
    let bidi = BidiInfo::new(text, None);
    let mut runs = vec![];
    for para in &bidi.paragraphs {
        let line = para.range.clone();
        let (levels, visual) = bidi.visual_runs(para, line);
        for r in visual {
            let rtl = levels[r.start].is_rtl();
            runs.extend(shape_run(&text[r], rtl, size_px, f));
        }
    }
    runs
}

fn width_of(runs: &[ShapedRun]) -> f32 {
    runs.iter().map(|r| r.advance).sum()
}

/// Width in pixels of `text` at the given size.
pub fn measure(text: &str, size_px: f32, bold: bool) -> f32 {
    width_of(&shape_line(text, size_px, bold))
}

struct Outline<'a> {
    r: &'a mut Rasterizer,
    x: f32,
    y: f32,
    scale: f32,
    last: Point,
    start: Point,
}

impl Outline<'_> {
    fn p(&self, x: f32, y: f32) -> Point {
        point(self.x + x * self.scale, self.y - y * self.scale)
    }
}

impl rustybuzz::ttf_parser::OutlineBuilder for Outline<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.last = self.p(x, y);
        self.start = self.last;
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.p(x, y);
        self.r.draw_line(self.last, p);
        self.last = p;
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let (c, p) = (self.p(x1, y1), self.p(x, y));
        self.r.draw_quad(self.last, c, p);
        self.last = p;
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let (c1, c2, p) = (self.p(x1, y1), self.p(x2, y2), self.p(x, y));
        self.r.draw_cubic(self.last, c1, c2, p);
        self.last = p;
    }
    fn close(&mut self) {
        if self.last != self.start {
            self.r.draw_line(self.last, self.start);
        }
        self.last = self.start;
    }
}

/// Text placed on one raster line: (text, x offset in px).
fn draw(r: &mut Rasterizer, runs: &[ShapedRun], x0: f32, baseline: f32) {
    let mut x = x0;
    for run in runs {
        for &(gid, gx, gy) in &run.glyphs {
            let mut o = Outline { r, x: x + gx, y: baseline - gy, scale: run.scale, last: point(0.0, 0.0), start: point(0.0, 0.0) };
            run.face.outline_glyph(rustybuzz::ttf_parser::GlyphId(gid), &mut o);
        }
        x += run.advance;
    }
}

/// Horizontal placement of a text span on a raster line.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Place {
    Left,
    Center,
    Right,
}

/// Line metrics for normal / large text.
pub fn metrics(large: bool) -> (f32, usize) {
    // (font size px, line height px). Noto's Arabic needs room above and below
    // the Latin x-height, so lines are taller than the 24-dot text mode.
    if large {
        (42.0, 60)
    } else {
        (22.0, 34)
    }
}

/// Render one receipt line containing one or more spans.
pub fn render(width_px: usize, spans: &[(&str, Place)], bold: bool, large: bool) -> Bitmap {
    let (size, height) = metrics(large);
    let baseline = height as f32 * 0.70;
    let mut bm = Bitmap::new(width_px, height);
    let mut r = Rasterizer::new(bm.width, height);
    for (text, place) in spans {
        let runs = shape_line(text, size, bold);
        let w = width_of(&runs);
        let x = match place {
            Place::Left => 0.0,
            Place::Right => (bm.width as f32 - w).max(0.0),
            Place::Center => ((bm.width as f32 - w) / 2.0).max(0.0),
        };
        draw(&mut r, &runs, x, baseline);
    }
    r.for_each_pixel_2d(|x, y, a| {
        if a >= 0.5 && (x as usize) < bm.width && (y as usize) < height {
            bm.set(x as usize, y as usize);
        }
    });
    bm
}

/// Greedy word wrap by rendered width.
pub fn wrap(text: &str, width_px: usize, bold: bool, large: bool) -> Vec<String> {
    let (size, _) = metrics(large);
    let max = width_px as f32;
    let mut lines = vec![];
    for para in text.split('\n') {
        let mut cur = String::new();
        for word in para.split_whitespace() {
            let candidate = if cur.is_empty() { word.to_string() } else { format!("{cur} {word}") };
            if cur.is_empty() || measure(&candidate, size, bold) <= max {
                cur = candidate;
            } else {
                lines.push(std::mem::replace(&mut cur, word.to_string()));
            }
        }
        lines.push(cur);
    }
    lines
}

/// Truncate `text` (by characters from the logical end) until it fits `max_px`.
pub fn fit(text: &str, max_px: f32, bold: bool, large: bool) -> String {
    let (size, _) = metrics(large);
    let mut s: Vec<char> = text.chars().collect();
    while !s.is_empty() && measure(&s.iter().collect::<String>(), size, bold) > max_px {
        s.pop();
    }
    s.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_needs_no_raster_but_arabic_and_symbols_do() {
        assert!(!needs_raster("TOTAL  BHD 1.650"));
        assert!(needs_raster("حليب"));
        assert!(needs_raster("Café"));
    }

    #[test]
    fn arabic_is_shaped_into_joined_forms() {
        // "بحرين": joined letters are narrower than the same letters isolated,
        // proving contextual shaping (not isolated forms, not '?').
        let joined = measure("بحرين", 22.0, false);
        let isolated: f32 = "بحرين".chars().map(|c| measure(&c.to_string(), 22.0, false)).sum();
        assert!(joined > 10.0);
        assert!(joined < isolated * 0.9, "joined {joined} vs isolated {isolated}");
        // Every Arabic glyph resolved (no .notdef = glyph 0).
        let runs = shape_line("فاتورة ضريبية", 22.0, false);
        assert!(runs.iter().all(|r| r.glyphs.iter().all(|g| g.0 != 0)));
    }

    #[test]
    fn rtl_text_is_placed_right_to_left() {
        // In visual order the first logical letter of an RTL word is rightmost.
        let runs = shape_line("ab بت", 22.0, false);
        assert_eq!(runs.len(), 2, "one Latin and one Arabic segment");
        assert!(runs[0].face.glyph_index('a').is_some(), "Latin run first (left)");
        // Lam-alef ligature: two characters become one glyph.
        let la = shape_line("لا", 22.0, false);
        assert_eq!(la.iter().map(|r| r.glyphs.len()).sum::<usize>(), 1);
    }

    #[test]
    fn render_places_spans_and_inks_pixels() {
        let bm = render(576, &[("حليب المراعي 1L", Place::Left), ("0.850", Place::Right)], false, false);
        assert_eq!(bm.width, 576);
        assert!(bm.ink() > 200);
        let (l, r) = bm.ink_span().unwrap();
        assert!(l < 20, "left span starts at the left edge ({l})");
        assert!(r > 550, "right span ends at the right edge ({r})");
        let empty = render(576, &[("", Place::Left)], false, false);
        assert_eq!(empty.ink(), 0);
        let esc = bm.to_escpos();
        assert_eq!(&esc[..4], &[0x1D, 0x76, 0x30, 0x00]);
        assert_eq!(esc[4] as usize + ((esc[5] as usize) << 8), 72);
        assert_eq!(esc.len(), 8 + 72 * bm.height);
    }

    #[test]
    fn wrap_by_pixel_width() {
        let long = "شكراً لتسوقكم معنا نتمنى لكم يوماً سعيداً وزيارة قريبة إن شاء الله";
        let lines = wrap(long, 300, false, false);
        assert!(lines.len() >= 2);
        for l in &lines {
            assert!(measure(l, 22.0, false) <= 300.0 || !l.contains(' '));
        }
        assert_eq!(lines.join(" "), long);
    }
}
