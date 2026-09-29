//! Page layout: OCR words with their page, line and box, in reading order.
//!
//! Tesseract's TSV output (or a PDF's text layer) becomes pages of lines of
//! words. Extraction reads lines in order and uses word positions to keep
//! table columns apart; the review screen uses the boxes to highlight where a
//! value came from. Boxes are in page pixels; `norm_box` scales them to 0..1.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Word {
    pub text: String,
    /// Tesseract word confidence 0..100 (100 for a PDF text layer).
    pub conf: i64,
    /// left, top, width, height in page pixels.
    pub bbox: [u32; 4],
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Line {
    pub block: u32,
    pub words: Vec<Word>,
}

impl Line {
    pub fn text(&self) -> String {
        self.words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ")
    }

    pub fn conf(&self) -> i64 {
        if self.words.is_empty() {
            return 0;
        }
        self.words.iter().map(|w| w.conf).sum::<i64>() / self.words.len() as i64
    }

    pub fn min_conf(&self) -> i64 {
        self.words.iter().map(|w| w.conf).min().unwrap_or(0)
    }

    pub fn bbox(&self) -> Option<[u32; 4]> {
        union(self.words.iter().map(|w| w.bbox))
    }
}

pub fn union(boxes: impl Iterator<Item = [u32; 4]>) -> Option<[u32; 4]> {
    let mut out: Option<(u32, u32, u32, u32)> = None;
    for b in boxes {
        if b[2] == 0 && b[3] == 0 {
            continue;
        }
        let (l, t, r, btm) = (b[0], b[1], b[0] + b[2], b[1] + b[3]);
        out = Some(match out {
            None => (l, t, r, btm),
            Some((a, bb, c, d)) => (a.min(l), bb.min(t), c.max(r), d.max(btm)),
        });
    }
    out.map(|(l, t, r, b)| [l, t, r - l, b - t])
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Page {
    /// 1-based page number in the original document.
    pub number: u32,
    pub width: u32,
    pub height: u32,
    /// `ocr` (Tesseract), `pdf_text` (the PDF's own text layer) or `text`
    /// (plain text without positions).
    pub source: String,
    /// Clockwise rotation applied before OCR (0, 90, 180, 270).
    #[serde(default)]
    pub rotation: u32,
    /// Preprocessing variant that produced the best OCR (e.g. `original`, `gray_contrast`).
    #[serde(default)]
    pub variant: String,
    pub lines: Vec<Line>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Layout {
    pub pages: Vec<Page>,
}

/// A line with its position in the whole document (reading order).
#[derive(Debug, Clone)]
pub struct DocLine<'a> {
    /// Index across all pages, stable for evidence references.
    pub index: usize,
    pub page: u32,
    pub line: &'a Line,
    pub page_w: u32,
    pub page_h: u32,
}

impl Layout {
    /// Plain text (no positions): one line per text line, page 1.
    pub fn from_text(text: &str, conf: i64) -> Layout {
        let lines = text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| Line {
                block: 0,
                words: l.split_whitespace().map(|w| Word { text: w.to_string(), conf, bbox: [0, 0, 0, 0] }).collect(),
            })
            .collect();
        Layout { pages: vec![Page { number: 1, width: 0, height: 0, source: "text".into(), rotation: 0, variant: String::new(), lines }] }
    }

    pub fn lines(&self) -> Vec<DocLine<'_>> {
        let mut out = vec![];
        for p in &self.pages {
            for l in &p.lines {
                out.push(DocLine { index: out.len(), page: p.number, line: l, page_w: p.width, page_h: p.height });
            }
        }
        out
    }

    pub fn text(&self) -> String {
        self.lines().iter().map(|l| l.line.text()).collect::<Vec<_>>().join("\n")
    }

    /// Mean word confidence over the whole document.
    pub fn mean_conf(&self) -> i64 {
        let (mut s, mut n) = (0i64, 0i64);
        for p in &self.pages {
            for l in &p.lines {
                for w in &l.words {
                    s += w.conf;
                    n += 1;
                }
            }
        }
        if n == 0 {
            0
        } else {
            s / n
        }
    }

    pub fn word_count(&self) -> usize {
        self.pages.iter().flat_map(|p| p.lines.iter()).map(|l| l.words.len()).sum()
    }
}

/// Tesseract TSV (level 5 = word rows) → one page. Words keep Tesseract's
/// block/paragraph/line grouping, in the order Tesseract read them.
pub fn page_from_tsv(tsv: &str, number: u32, width: u32, height: u32) -> Page {
    let mut lines: Vec<Line> = vec![];
    let mut key = (String::new(), String::new(), String::new());
    for row in tsv.lines().skip(1) {
        let c: Vec<&str> = row.split('\t').collect();
        if c.len() < 12 || c[0] != "5" {
            continue;
        }
        let word = c[11].trim();
        if word.is_empty() {
            continue;
        }
        let conf = c[10].parse::<f64>().ok().filter(|v| *v >= 0.0).map(|v| v.round() as i64).unwrap_or(0);
        let num = |i: usize| c[i].parse::<u32>().unwrap_or(0);
        let w = Word { text: word.to_string(), conf, bbox: [num(6), num(7), num(8), num(9)] };
        let k = (c[2].to_string(), c[3].to_string(), c[4].to_string());
        if k != key || lines.is_empty() {
            lines.push(Line { block: num(2), words: vec![w] });
            key = k;
        } else if let Some(l) = lines.last_mut() {
            l.words.push(w);
        }
    }
    // Tesseract sometimes splits one visual row (a wide table) into several
    // blocks. Rows whose vertical centres coincide are the same table row:
    // merge them left-to-right so line items keep their columns together.
    let lines = merge_rows(lines);
    Page { number, width, height, source: "ocr".into(), rotation: 0, variant: String::new(), lines }
}

fn merge_rows(lines: Vec<Line>) -> Vec<Line> {
    let mut out: Vec<Line> = vec![];
    for l in lines {
        let Some(b) = l.bbox() else {
            out.push(l);
            continue;
        };
        // Sideways text (tall line boxes) is never merged: its lines share a
        // vertical centre by nature.
        if b[3] > b[2] {
            out.push(l);
            continue;
        }
        let centre = b[1] + b[3] / 2;
        // Search the whole page: a right-hand column block (totals) usually
        // comes after every line of the left-hand block in Tesseract's order.
        let hit = out.iter_mut().rev().find(|o| {
            o.bbox().filter(|ob| ob[2] >= ob[3]).is_some_and(|ob| {
                let oc = ob[1] + ob[3] / 2;
                let tol = ob[3].min(b[3]) / 2;
                oc.abs_diff(centre) <= tol.max(2) && (ob[0] + ob[2] <= b[0] || b[0] + b[2] <= ob[0])
            })
        });
        match hit {
            Some(o) => {
                o.words.extend(l.words);
                o.words.sort_by_key(|w| w.bbox[0]);
            }
            None => out.push(l),
        }
    }
    out
}

/// A box scaled to the page (0..1, rounded to 4 places) for the review screen.
pub fn norm_box(b: [u32; 4], w: u32, h: u32) -> Option<[f32; 4]> {
    if w == 0 || h == 0 || (b[2] == 0 && b[3] == 0) {
        return None;
    }
    let r = |v: f32| (v * 10_000.0).round() / 10_000.0;
    Some([r(b[0] as f32 / w as f32), r(b[1] as f32 / h as f32), r(b[2] as f32 / w as f32), r(b[3] as f32 / h as f32)])
}

#[cfg(test)]
mod tests {
    use super::*;

    const HDR: &str = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n";

    #[test]
    fn tsv_words_lines_and_boxes() {
        let tsv = format!(
            "{HDR}5\t1\t1\t1\t1\t1\t10\t10\t50\t12\t96\tTax\n5\t1\t1\t1\t1\t2\t64\t10\t70\t12\t91\tInvoice\n\
             5\t1\t2\t1\t1\t1\t10\t40\t40\t12\t88\tMilk\n5\t1\t3\t1\t1\t1\t300\t41\t40\t12\t90\t0.450\n"
        );
        let p = page_from_tsv(&tsv, 1, 600, 800);
        assert_eq!(p.lines.len(), 2, "the split table row is merged");
        assert_eq!(p.lines[0].text(), "Tax Invoice");
        assert_eq!(p.lines[1].text(), "Milk 0.450");
        assert_eq!(p.lines[0].bbox(), Some([10, 10, 124, 12]));
        assert_eq!(norm_box([60, 80, 120, 40], 600, 800), Some([0.1, 0.1, 0.2, 0.05]));
        let l = Layout { pages: vec![p] };
        assert_eq!(l.word_count(), 4);
        assert_eq!(l.mean_conf(), (96 + 91 + 88 + 90) / 4);
    }

    #[test]
    fn right_column_block_joins_its_rows_after_many_lines() {
        // Block 1: ten description rows; block 2 (read afterwards): the totals column.
        let mut tsv = HDR.to_string();
        for i in 0..10u32 {
            tsv.push_str(&format!("5\t1\t1\t1\t{}\t1\t70\t{}\t300\t30\t95\tItem{i}\n", i + 1, 100 + i * 60));
        }
        for i in 0..10u32 {
            tsv.push_str(&format!("5\t1\t2\t1\t{}\t1\t1050\t{}\t90\t30\t95\t{}.000\n", i + 1, 101 + i * 60, i + 1));
        }
        let p = page_from_tsv(&tsv, 1, 1240, 1400);
        assert_eq!(p.lines.len(), 10, "{:?}", p.lines.iter().map(|l| l.text()).collect::<Vec<_>>());
        assert_eq!(p.lines[0].text(), "Item0 1.000");
        assert_eq!(p.lines[9].text(), "Item9 10.000");
    }

    #[test]
    fn plain_text_layout() {
        let l = Layout::from_text("A b\n\nC", 70);
        assert_eq!(l.lines().len(), 2);
        assert_eq!(l.text(), "A b\nC");
        assert_eq!(l.lines()[1].index, 1);
    }
}
