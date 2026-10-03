//! PDF supplier documents: the text layer when the PDF has one (typed
//! invoices), and the embedded page image when it is a scan (so it can be
//! read with OCR). Password-protected and damaged files are reported, never
//! guessed at.
//!
//! Limitations: page images are extracted only when stored as JPEG
//! (DCTDecode) or as plain 8-bit gray/RGB pixels (Flate). JBIG2 and CCITT fax
//! images — common in office scanners — are reported so the user can upload a
//! photo or an image export instead. PDF text keeps its reading order but
//! not its positions.

use lopdf::Document;

use crate::error::{AppError, AppResult, ErrorCode};

pub const MAX_PAGES: usize = 20;
/// Decompressed bytes read from one page's content (text drawing commands).
const MAX_CONTENT_BYTES: u64 = 8 * 1024 * 1024;
/// Characters of text kept per page.
const MAX_PAGE_TEXT: usize = 200_000;
/// Pixels of one embedded page image.
const MAX_IMAGE_PIXELS: u64 = super::MAX_PAGE_SIDE as u64 * super::MAX_PAGE_SIDE as u64 / 2;

/// Inflate a Flate stream, refusing anything that grows past `limit`
/// (a decompression bomb) instead of allocating it.
fn inflate_bounded(data: &[u8], limit: u64) -> Option<Vec<u8>> {
    let mut d = std::io::Read::take(flate2::read::ZlibDecoder::new(data), limit + 1);
    let mut buf = vec![];
    std::io::Read::read_to_end(&mut d, &mut buf).ok()?;
    (buf.len() as u64 <= limit).then_some(buf)
}

/// A page's drawing commands, decompressed by us within MAX_CONTENT_BYTES.
fn page_content_bounded(doc: &Document, page_id: lopdf::ObjectId) -> Option<Vec<u8>> {
    let mut out = vec![];
    for id in doc.get_page_contents(page_id) {
        let Ok(obj) = doc.get_object(id) else { continue };
        let Ok(stream) = obj.as_stream() else { continue };
        let filters: Vec<String> = stream
            .dict
            .get(b"Filter")
            .ok()
            .map(|f| match f {
                lopdf::Object::Name(n) => vec![String::from_utf8_lossy(n).into_owned()],
                lopdf::Object::Array(a) => {
                    a.iter().filter_map(|x| x.as_name().ok()).map(|n| String::from_utf8_lossy(n).into_owned()).collect()
                }
                _ => vec![],
            })
            .unwrap_or_default();
        let left = MAX_CONTENT_BYTES.saturating_sub(out.len() as u64);
        let part = match filters.as_slice() {
            [] => (stream.content.len() as u64 <= left).then(|| stream.content.clone()),
            [f] if f == "FlateDecode" => inflate_bounded(&stream.content, left),
            _ => None,
        }?;
        out.extend_from_slice(&part);
        out.push(b'\n');
    }
    Some(out)
}

#[derive(Debug, Clone, Default)]
pub struct PdfPage {
    pub number: u32,
    /// Text from the PDF's own text layer (may be empty).
    pub text: String,
    /// The page's scan as an encoded image (JPEG or PNG), when there is one.
    pub image: Option<Vec<u8>>,
    pub image_is_jpeg: bool,
    /// Why no usable image was found (unsupported encoding, none embedded).
    pub note: Option<String>,
}

impl PdfPage {
    /// A text layer worth using: enough letters and digits, mostly printable.
    pub fn has_text(&self) -> bool {
        let alnum = self.text.chars().filter(|c| c.is_alphanumeric()).count();
        let odd = self.text.chars().filter(|c| c.is_control() && *c != '\n' && *c != '\t' && *c != '\r' || *c == '\u{FFFD}').count();
        alnum >= 20 && odd * 10 < alnum
    }
}

fn is_protected_err(msg: &str) -> bool {
    let m = msg.to_lowercase();
    m.contains("encrypt") || m.contains("password") || m.contains("decrypt")
}

fn encode_png(img: image::DynamicImage) -> Option<Vec<u8>> {
    let mut out = std::io::Cursor::new(vec![]);
    img.write_to(&mut out, image::ImageFormat::Png).ok()?;
    Some(out.into_inner())
}

/// Text of a page with its line breaks: a new line starts whenever the text
/// position moves vertically (Td/TD with a y offset, T*, ', ", Tm with a new
/// y) or a text object ends. Font encodings are decoded by lopdf.
fn page_text(doc: &Document, page_id: lopdf::ObjectId) -> String {
    use lopdf::Object;
    let fonts = match doc.get_page_fonts(page_id) {
        Ok(f) => f,
        Err(_) => return String::new(),
    };
    let encodings: std::collections::BTreeMap<Vec<u8>, lopdf::Encoding> =
        fonts.into_iter().filter_map(|(name, font)| font.get_font_encoding(doc).ok().map(|e| (name, e))).collect();
    let Some(data) = page_content_bounded(doc, page_id) else { return String::new() };
    let Ok(content) = lopdf::content::Content::decode(&data) else { return String::new() };
    let mut out = String::new();
    let mut enc: Option<&lopdf::Encoding> = None;
    let mut y: Option<f32> = None;
    let newline = |out: &mut String| {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
    };
    let num = |o: &Object| o.as_float().ok().or_else(|| o.as_i64().ok().map(|v| v as f32));
    for op in &content.operations {
        let a = &op.operands;
        match op.operator.as_str() {
            "Tf" => enc = a.first().and_then(|n| n.as_name().ok()).and_then(|n| encodings.get(n)),
            "Td" | "TD" => {
                let (tx, ty) = (a.first().and_then(num).unwrap_or(0.0), a.get(1).and_then(num).unwrap_or(0.0));
                if ty.abs() > 0.5 {
                    newline(&mut out);
                } else if tx > 0.5 && !out.ends_with([' ', '\n']) && !out.is_empty() {
                    out.push(' ');
                }
            }
            "T*" | "'" | "\"" => newline(&mut out),
            "Tm" => {
                let ny = a.get(5).and_then(num);
                if let (Some(prev), Some(n)) = (y, ny) {
                    if (prev - n).abs() > 0.5 {
                        newline(&mut out);
                    } else if !out.ends_with([' ', '\n']) {
                        out.push(' ');
                    }
                }
                y = ny;
            }
            "ET" => newline(&mut out),
            _ => {}
        }
        if out.len() > MAX_PAGE_TEXT {
            break;
        }
        if matches!(op.operator.as_str(), "Tj" | "'" | "\"" | "TJ") {
            let Some(e) = enc else { continue };
            let items: Vec<&Object> = match op.operator.as_str() {
                "TJ" => a.first().and_then(|x| x.as_array().ok()).map(|v| v.iter().collect()).unwrap_or_default(),
                _ => a.last().into_iter().collect(),
            };
            for it in items {
                match it {
                    Object::String(bytes, _) => {
                        if let Ok(t) = Document::decode_text(e, bytes) {
                            out.push_str(&t);
                        }
                    }
                    other => {
                        // Large kerning back-steps are word gaps.
                        if num(other).is_some_and(|v| v < -200.0) && !out.ends_with([' ', '\n']) {
                            out.push(' ');
                        }
                    }
                }
            }
        }
    }
    out
}

fn page_image(doc: &Document, page_id: lopdf::ObjectId) -> (Option<(Vec<u8>, bool)>, Option<String>) {
    let images = match doc.get_page_images(page_id) {
        Ok(i) => i,
        Err(_) => return (None, None),
    };
    // The largest image on the page is the scan.
    let Some(img) = images.iter().max_by_key(|i| i.width * i.height) else { return (None, None) };
    let filters = img.filters.clone().unwrap_or_default();
    if img.width < 50 || img.height < 50 {
        return (None, None);
    }
    let pixels = (img.width.max(0) as u64).saturating_mul(img.height.max(0) as u64);
    if pixels > MAX_IMAGE_PIXELS || img.width > super::MAX_PAGE_SIDE as i64 || img.height > super::MAX_PAGE_SIDE as i64 {
        return (
            None,
            Some(format!("The page image is too large to read ({} × {} pixels); upload a smaller scan or a photo.", img.width, img.height)),
        );
    }
    if filters.iter().any(|f| f == "DCTDecode") {
        return (Some((img.content.to_vec(), true)), None);
    }
    if filters.iter().any(|f| f == "JBIG2Decode" || f == "CCITTFaxDecode" || f == "JPXDecode") {
        return (
            None,
            Some(format!(
                "The page image uses {} encoding, which cannot be read here; upload a photo or image export of the page.",
                filters.join("+")
            )),
        );
    }
    // Flate / raw 8-bit gray or RGB pixels.
    // Never inflate more than the pixels the image declares (3 bytes each at most).
    let expected = pixels.saturating_mul(3);
    let raw = if filters.iter().any(|f| f == "FlateDecode") {
        match inflate_bounded(img.content, expected) {
            Some(b) => b,
            None => return (None, Some("The page image could not be decompressed (damaged or larger than it says).".into())),
        }
    } else if filters.is_empty() {
        img.content.to_vec()
    } else {
        return (None, Some(format!("The page image encoding {} is not supported.", filters.join("+"))));
    };
    let (w, h) = (img.width as u32, img.height as u32);
    let (gray, rgb) = (pixels as usize, pixels as usize * 3);
    let bpc = img.bits_per_component.unwrap_or(8);
    let cs = img.color_space.clone().unwrap_or_default();
    let out = match (cs.as_str(), bpc) {
        ("DeviceGray", 8) if raw.len() >= gray => {
            image::GrayImage::from_raw(w, h, raw[..gray].to_vec()).map(image::DynamicImage::ImageLuma8)
        }
        ("DeviceRGB", 8) if raw.len() >= rgb => image::RgbImage::from_raw(w, h, raw[..rgb].to_vec()).map(image::DynamicImage::ImageRgb8),
        _ => None,
    };
    match out.and_then(encode_png) {
        Some(png) => (Some((png, false)), None),
        None => (None, Some(format!("The page image ({cs}, {bpc}-bit) is in a format that cannot be read here."))),
    }
}

/// Read a PDF into pages. Errors: `validation` for protected or damaged files.
pub fn read_pdf(bytes: &[u8]) -> AppResult<Vec<PdfPage>> {
    let doc = Document::load_mem(bytes).map_err(|e| {
        let msg = e.to_string();
        if is_protected_err(&msg) {
            AppError::new(
                ErrorCode::Validation,
                "This PDF is password-protected. Remove the password (or print it to a new PDF) and upload it again.",
            )
        } else {
            AppError::new(ErrorCode::Validation, "The PDF is damaged or not a PDF, so it could not be read.")
        }
    })?;
    let pages = doc.get_pages();
    if pages.is_empty() {
        return Err(AppError::validation("The PDF has no pages."));
    }
    let mut out = vec![];
    for (n, id) in pages.iter().take(MAX_PAGES) {
        let text = page_text(&doc, *id);
        let (img, note) = page_image(&doc, *id);
        let (image, image_is_jpeg) = match img {
            Some((b, j)) => (Some(b), j),
            None => (None, false),
        };
        out.push(PdfPage { number: *n, text: text.replace('\u{0}', ""), image, image_is_jpeg, note });
    }
    if doc.is_encrypted() && out.iter().all(|p| !p.has_text() && p.image.is_none()) {
        return Err(AppError::validation(
            "This PDF is password-protected. Remove the password (or print it to a new PDF) and upload it again.",
        ));
    }
    Ok(out)
}

pub fn page_count_note(total: usize) -> Option<String> {
    (total > MAX_PAGES).then(|| format!("Only the first {MAX_PAGES} pages are read."))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use lopdf::content::{Content, Operation};
    use lopdf::{dictionary, Object, Stream};

    /// A small PDF: page 1 with a text layer, page 2 with an embedded JPEG.
    pub fn sample_pdf(text_lines: &[&str], jpeg: Option<(Vec<u8>, u32, u32)>) -> Vec<u8> {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let font_id = doc.add_object(dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica" });
        let resources_id = doc.add_object(dictionary! { "Font" => dictionary! { "F1" => font_id } });
        let mut ops = vec![Operation::new("BT", vec![]), Operation::new("Tf", vec!["F1".into(), 12.into()])];
        for (i, l) in text_lines.iter().enumerate() {
            ops.push(Operation::new("Td", vec![50.into(), (if i == 0 { 780 } else { -16 }).into()]));
            ops.push(Operation::new("Tj", vec![Object::string_literal(*l)]));
        }
        ops.push(Operation::new("ET", vec![]));
        let content = Content { operations: ops };
        let c1 = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
        let p1 = doc.add_object(dictionary! { "Type" => "Page", "Parent" => pages_id, "Contents" => c1, "Resources" => resources_id, "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()] });
        let mut kids: Vec<Object> = vec![p1.into()];
        if let Some((bytes, w, h)) = jpeg {
            let img = doc.add_object(Stream::new(
                dictionary! { "Type" => "XObject", "Subtype" => "Image", "Width" => w as i64, "Height" => h as i64, "ColorSpace" => "DeviceGray", "BitsPerComponent" => 8, "Filter" => "DCTDecode" },
                bytes,
            ));
            let res2 = doc.add_object(dictionary! { "XObject" => dictionary! { "Im1" => img } });
            let draw = Content {
                operations: vec![Operation::new("q", vec![]), Operation::new("Do", vec!["Im1".into()]), Operation::new("Q", vec![])],
            };
            let c2 = doc.add_object(Stream::new(dictionary! {}, draw.encode().unwrap()));
            let p2 = doc.add_object(dictionary! { "Type" => "Page", "Parent" => pages_id, "Contents" => c2, "Resources" => res2, "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()] });
            kids.push(p2.into());
        }
        let count = kids.len() as i64;
        doc.objects.insert(pages_id, Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => count }));
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog);
        let mut out = vec![];
        doc.save_to(&mut out).unwrap();
        out
    }

    #[test]
    fn text_layer_and_scanned_page() {
        let g = image::GrayImage::from_fn(200, 100, |x, _| image::Luma([(x % 250) as u8]));
        let mut jpg = std::io::Cursor::new(vec![]);
        image::DynamicImage::ImageLuma8(g).write_to(&mut jpg, image::ImageFormat::Jpeg).unwrap();
        let pdf = sample_pdf(
            &["TAX INVOICE", "Invoice No: INV-7 Date: 28/09/2026", "Milk 10 0.450 4.500", "Total 4.500"],
            Some((jpg.into_inner(), 200, 100)),
        );
        let pages = read_pdf(&pdf).unwrap();
        assert_eq!(pages.len(), 2);
        assert!(pages[0].has_text(), "{:?}", pages[0].text);
        assert!(pages[0].text.contains("INV-7"));
        assert!(pages[0].text.lines().any(|l| l.trim() == "Milk 10 0.450 4.500"), "{:?}", pages[0].text);
        assert!(pages[1].image.is_some() && pages[1].image_is_jpeg);
        assert!(!pages[1].has_text());
    }

    #[test]
    fn damaged_file() {
        let e = read_pdf(b"%PDF-1.4 not really").unwrap_err();
        assert_eq!(e.code, ErrorCode::Validation);
        assert!(e.message.contains("damaged"));
    }

    fn zlib(data: &[u8]) -> Vec<u8> {
        let mut e = flate2::write::ZlibEncoder::new(vec![], flate2::Compression::best());
        std::io::Write::write_all(&mut e, data).unwrap();
        e.finish().unwrap()
    }

    /// One page whose only content is an image XObject with this dictionary and data.
    fn image_pdf(dict: lopdf::Dictionary, data: Vec<u8>, pages: usize, content: Option<Vec<u8>>) -> Vec<u8> {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let img = doc.add_object(Stream::new(dict, data));
        let res = doc.add_object(dictionary! { "XObject" => dictionary! { "Im1" => img } });
        let draw = content.unwrap_or_else(|| {
            Content { operations: vec![Operation::new("q", vec![]), Operation::new("Do", vec!["Im1".into()]), Operation::new("Q", vec![])] }
                .encode()
                .unwrap()
        });
        let mut kids: Vec<Object> = vec![];
        for _ in 0..pages {
            let c = doc.add_object(Stream::new(dictionary! { "Filter" => "FlateDecode" }, zlib(&draw)));
            let p = doc.add_object(dictionary! { "Type" => "Page", "Parent" => pages_id, "Contents" => c, "Resources" => res, "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()] });
            kids.push(p.into());
        }
        let count = kids.len() as i64;
        doc.objects.insert(pages_id, Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => count }));
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog);
        let mut out = vec![];
        doc.save_to(&mut out).unwrap();
        out
    }

    #[test]
    fn a_decompression_bomb_in_a_page_image_is_refused() {
        // Declares 100 × 100 gray (10 000 bytes) but inflates to 64 MB.
        let bomb = zlib(&vec![0u8; 64 * 1024 * 1024]);
        assert!(bomb.len() < 200_000, "the bomb is small on disk");
        let pdf = image_pdf(
            dictionary! { "Type" => "XObject", "Subtype" => "Image", "Width" => 100, "Height" => 100, "ColorSpace" => "DeviceGray", "BitsPerComponent" => 8, "Filter" => "FlateDecode" },
            bomb,
            1,
            None,
        );
        let pages = read_pdf(&pdf).unwrap();
        assert!(pages[0].image.is_none());
        assert!(pages[0].note.as_deref().unwrap_or("").contains("decompressed"), "{:?}", pages[0].note);
    }

    #[test]
    fn a_decompression_bomb_in_page_text_is_cut_off() {
        // 64 MB of drawing commands compressed into a small stream: read up to the limit only.
        let mut content = b"BT /F1 12 Tf ".to_vec();
        content.extend(std::iter::repeat_n(b'0', 64 * 1024 * 1024));
        let pdf =
            image_pdf(dictionary! { "Type" => "XObject", "Subtype" => "Image", "Width" => 1, "Height" => 1 }, vec![0], 1, Some(content));
        let pages = read_pdf(&pdf).unwrap();
        assert!(pages[0].text.len() <= MAX_PAGE_TEXT + 1_000);
    }

    #[test]
    fn enormous_images_and_unsupported_codecs_are_reported_not_decoded() {
        let huge = image_pdf(
            dictionary! { "Type" => "XObject", "Subtype" => "Image", "Width" => 100_000, "Height" => 100_000, "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8, "Filter" => "FlateDecode" },
            zlib(&[0u8; 10]),
            1,
            None,
        );
        let p = read_pdf(&huge).unwrap();
        assert!(p[0].image.is_none() && p[0].note.as_deref().unwrap_or("").contains("too large"), "{:?}", p[0].note);
        let jbig = image_pdf(
            dictionary! { "Type" => "XObject", "Subtype" => "Image", "Width" => 800, "Height" => 1000, "ColorSpace" => "DeviceGray", "BitsPerComponent" => 1, "Filter" => "JBIG2Decode" },
            vec![0; 100],
            1,
            None,
        );
        let p = read_pdf(&jbig).unwrap();
        assert!(p[0].note.as_deref().unwrap_or("").contains("JBIG2"), "{:?}", p[0].note);
        // A corrupt image claiming to be JPEG is handed on as bytes; decoding it later fails safely.
        assert!(super::super::decode_page(b"\xff\xd8\xff\xe0 not a jpeg").is_none());
    }

    #[test]
    fn truncated_and_huge_page_count_pdfs_are_bounded() {
        let g = image::GrayImage::from_fn(200, 100, |x, _| image::Luma([(x % 250) as u8]));
        let mut jpg = std::io::Cursor::new(vec![]);
        image::DynamicImage::ImageLuma8(g).write_to(&mut jpg, image::ImageFormat::Jpeg).unwrap();
        let pdf = sample_pdf(&["TAX INVOICE", "Total 4.500"], Some((jpg.into_inner(), 200, 100)));
        // Cut at every tenth of the file: never a panic, only pages or a refusal.
        for cut in (1..10).map(|k| pdf.len() * k / 10) {
            let _ = read_pdf(&pdf[..cut]);
        }
        let many = image_pdf(
            dictionary! { "Type" => "XObject", "Subtype" => "Image", "Width" => 60, "Height" => 60, "ColorSpace" => "DeviceGray", "BitsPerComponent" => 8, "Filter" => "FlateDecode" },
            zlib(&[128u8; 3600]),
            MAX_PAGES + 15,
            None,
        );
        let pages = read_pdf(&many).unwrap();
        assert_eq!(pages.len(), MAX_PAGES);
        assert!(page_count_note(MAX_PAGES + 15).is_some());
    }

    #[test]
    fn an_image_header_claiming_a_giant_page_is_refused_before_decoding() {
        // A valid PNG header for a 60 000 × 60 000 image (a few bytes on disk).
        let mut png = std::io::Cursor::new(vec![]);
        image::DynamicImage::ImageLuma8(image::GrayImage::new(2, 2)).write_to(&mut png, image::ImageFormat::Png).unwrap();
        let mut bytes = png.into_inner();
        bytes[16..20].copy_from_slice(&60_000u32.to_be_bytes());
        bytes[20..24].copy_from_slice(&60_000u32.to_be_bytes());
        // Recompute the IHDR checksum so the header itself is valid.
        let crc = {
            let mut c = 0xFFFF_FFFFu32;
            for b in &bytes[12..29] {
                c ^= *b as u32;
                for _ in 0..8 {
                    c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
                }
            }
            !c
        };
        bytes[29..33].copy_from_slice(&crc.to_be_bytes());
        assert_eq!(super::super::page_oversized(&bytes), Some((60_000, 60_000)));
        assert!(super::super::decode_page(&bytes).is_none());
    }

    #[test]
    fn deeply_nested_objects_are_refused_not_a_crash() {
        // RUSTSEC-2026-0187: nesting used to overflow the parser's stack.
        let depth = 200_000;
        let mut body = String::from("%PDF-1.4\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R /X ");
        body.push_str(&"[".repeat(depth));
        body.push_str(&"]".repeat(depth));
        body.push_str(" >>\nendobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF\n");
        let r = std::thread::Builder::new().stack_size(2 << 20).spawn(move || read_pdf(body.as_bytes()).map(|p| p.len())).unwrap().join();
        let r = r.expect("the parser must not crash on nested objects");
        assert!(r.is_err() || r.unwrap() == 0);
    }
}
