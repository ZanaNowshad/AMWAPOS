//! Minimal PDF writer for receipt copies: one page holding the receipt as a
//! 1-bit image at printer resolution (203 dpi), so the PDF shows exactly what
//! the thermal printer prints, Arabic included. No external dependencies.

use std::io::Write;

use flate2::write::ZlibEncoder;
use flate2::Compression;

use crate::raster::Bitmap;

const PRINTER_DPI: f64 = 203.0;

/// Encode a bitmap (1 = black) as a single-page PDF.
pub fn bitmap_pdf(bm: &Bitmap, title: &str) -> Vec<u8> {
    let w_pt = bm.width as f64 / PRINTER_DPI * 72.0;
    let h_pt = bm.height as f64 / PRINTER_DPI * 72.0;
    let mut z = ZlibEncoder::new(Vec::new(), Compression::default());
    let _ = z.write_all(&bm.bits);
    let image = z.finish().unwrap_or_default();
    let content = format!("q {w_pt:.2} 0 0 {h_pt:.2} 0 0 cm /Im0 Do Q");
    let title: String = title.chars().filter(|c| c.is_ascii_graphic() || *c == ' ').filter(|c| !matches!(c, '(' | ')' | '\\')).collect();

    let mut out: Vec<u8> = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets: Vec<usize> = Vec::new();
    let mut obj = |out: &mut Vec<u8>, body: &[u8]| {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", offsets.len()).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    };
    obj(&mut out, b"<< /Type /Catalog /Pages 2 0 R >>");
    obj(&mut out, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    obj(
        &mut out,
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w_pt:.2} {h_pt:.2}] /Resources << /XObject << /Im0 4 0 R >> >> /Contents 5 0 R >>")
            .as_bytes(),
    );
    let mut img = format!(
        "<< /Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace /DeviceGray /BitsPerComponent 1 /Decode [1 0] /Filter /FlateDecode /Length {} >>\nstream\n",
        bm.width,
        bm.height,
        image.len()
    )
    .into_bytes();
    img.extend_from_slice(&image);
    img.extend_from_slice(b"\nendstream");
    obj(&mut out, &img);
    let mut c = format!("<< /Length {} >>\nstream\n", content.len()).into_bytes();
    c.extend_from_slice(content.as_bytes());
    c.extend_from_slice(b"\nendstream");
    obj(&mut out, &c);
    obj(&mut out, format!("<< /Title ({title}) /Producer (AMWAPOS) >>").as_bytes());
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len() + 1).as_bytes());
    for o in &offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {} /Root 1 0 R /Info {} 0 R >>\nstartxref\n{xref}\n%%EOF\n", offsets.len() + 1, offsets.len())
            .as_bytes(),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pdf_is_well_formed() {
        let mut bm = Bitmap::new(64, 10);
        bm.set(3, 3);
        let pdf = bitmap_pdf(&bm, "Receipt T01-0000001");
        let s = String::from_utf8_lossy(&pdf);
        assert!(s.starts_with("%PDF-1.4"));
        assert!(s.contains("/Width 64 /Height 10"));
        assert!(s.trim_end().ends_with("%%EOF"));
        // xref offsets point at "N 0 obj"
        let xref_at: usize = s.rsplit("startxref\n").next().unwrap().lines().next().unwrap().parse().unwrap();
        assert!(pdf[xref_at..].starts_with(b"xref"));
        let tail = String::from_utf8(pdf[xref_at..].to_vec()).unwrap();
        for (i, line) in tail.lines().skip(3).take(6).enumerate() {
            let off: usize = line[..10].parse().unwrap();
            assert!(pdf[off..].starts_with(format!("{} 0 obj", i + 1).as_bytes()));
        }
    }
}
