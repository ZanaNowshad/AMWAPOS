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
    let raw = if filters.iter().any(|f| f == "FlateDecode") {
        let mut d = flate2::read::ZlibDecoder::new(img.content);
        let mut buf = vec![];
        if std::io::Read::read_to_end(&mut d, &mut buf).is_err() {
            return (None, Some("The page image could not be decompressed.".into()));
        }
        buf
    } else if filters.is_empty() {
        img.content.to_vec()
    } else {
        return (None, Some(format!("The page image encoding {} is not supported.", filters.join("+"))));
    };
    let (w, h) = (img.width as u32, img.height as u32);
    let bpc = img.bits_per_component.unwrap_or(8);
    let cs = img.color_space.clone().unwrap_or_default();
    let out = match (cs.as_str(), bpc) {
        ("DeviceGray", 8) if raw.len() >= (w * h) as usize => {
            image::GrayImage::from_raw(w, h, raw[..(w * h) as usize].to_vec()).map(image::DynamicImage::ImageLuma8)
        }
        ("DeviceRGB", 8) if raw.len() >= (w * h * 3) as usize => {
            image::RgbImage::from_raw(w, h, raw[..(w * h * 3) as usize].to_vec()).map(image::DynamicImage::ImageRgb8)
        }
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
        let text = doc.extract_text(&[*n]).unwrap_or_default();
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
        assert!(pages[1].image.is_some() && pages[1].image_is_jpeg);
        assert!(!pages[1].has_text());
    }

    #[test]
    fn damaged_file() {
        let e = read_pdf(b"%PDF-1.4 not really").unwrap_err();
        assert_eq!(e.code, ErrorCode::Validation);
        assert!(e.message.contains("damaged"));
    }
}
