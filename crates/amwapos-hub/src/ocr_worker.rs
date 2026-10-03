//! OCR worker: a separate task that runs the bundled Tesseract (a child
//! process per image, time-limited, one at a time) for supplier invoices and
//! payment screenshots. It never runs on the WhatsApp task.
//!
//! Models ship with the app as `<lang>.traineddata.gz` plus `models.json`
//! (SHA-256 of each). They are verified and unpacked into
//! `<data>/ocr/tessdata` before use. If the English model is missing or
//! damaged, OCR reports `ocr_model_missing` and stays off. OCR is assistance:
//! nothing it reads posts stock or settles a payment without a person.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use amwapos_core::docintel as dq;
use amwapos_core::docintel::service::DocOcr;
use amwapos_core::{AppCore, AppError, AppResult, ErrorCode};
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

const JOB_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, Default)]
pub struct OcrPaths {
    /// Tesseract executable (bundled on Windows; `tesseract` on PATH in development).
    pub tesseract: Option<PathBuf>,
    /// Folder with `models.json` and `<lang>.traineddata.gz`.
    pub models: Option<PathBuf>,
}

impl OcrPaths {
    /// Installed layout: `<resources>/tesseract/tesseract.exe` and
    /// `<resources>/ocr-models`. Development: `ocr/models` in the repository
    /// and `tesseract` on PATH. `AMWAPOS_TESSERACT` / `AMWAPOS_OCR_MODELS`
    /// override both.
    pub fn discover(resource_dir: Option<&Path>) -> Self {
        let exe = if cfg!(windows) { "tesseract.exe" } else { "tesseract" };
        let tesseract = std::env::var_os("AMWAPOS_TESSERACT")
            .map(PathBuf::from)
            .or_else(|| resource_dir.map(|r| r.join("tesseract").join(exe)).filter(|p| p.exists()))
            .or_else(|| Some(PathBuf::from(exe)));
        let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../ocr/models");
        let models = std::env::var_os("AMWAPOS_OCR_MODELS")
            .map(PathBuf::from)
            .or_else(|| resource_dir.map(|r| r.join("ocr-models")).filter(|p| p.join("models.json").exists()))
            .or_else(|| dev.join("models.json").exists().then_some(dev));
        Self { tesseract, models }
    }
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct OcrStatus {
    /// Models verified and Tesseract runs.
    pub available: bool,
    pub languages: Vec<String>,
    pub engine: Option<String>,
    /// `ocr_model_missing` or `ocr_engine_missing` when unavailable.
    pub error_code: Option<String>,
    pub error: Option<String>,
    pub running: bool,
    pub last_job_at: Option<String>,
}

pub struct OcrWorker {
    core: Arc<AppCore>,
    paths: Mutex<OcrPaths>,
    status: Mutex<OcrStatus>,
    prepared: Mutex<Option<(PathBuf, Vec<String>)>>,
    task: Mutex<Option<JoinHandle<()>>>,
    pub poke: Arc<Notify>,
}

fn model_missing(msg: impl Into<String>) -> AppError {
    AppError::new(ErrorCode::OcrModelMissing, msg).with_details(json!({ "kind": "OCR_MODEL_MISSING" }))
}

impl OcrWorker {
    pub fn new(core: Arc<AppCore>) -> Arc<Self> {
        Arc::new(Self {
            core,
            paths: Mutex::new(OcrPaths::discover(None)),
            status: Mutex::new(OcrStatus::default()),
            prepared: Mutex::new(None),
            task: Mutex::new(None),
            poke: Arc::new(Notify::new()),
        })
    }

    pub fn set_paths(&self, p: OcrPaths) {
        *self.paths.lock().unwrap() = p;
        *self.prepared.lock().unwrap() = None;
    }

    pub fn status(&self) -> OcrStatus {
        let mut s = self.status.lock().unwrap().clone();
        s.running = self.task.lock().unwrap().as_ref().map(|h| !h.is_finished()).unwrap_or(false);
        s
    }

    /// Verify and unpack the models (once), and check the engine runs.
    /// Blocking: call from a blocking thread.
    pub fn prepare(&self) -> AppResult<(PathBuf, Vec<String>)> {
        if let Some(p) = self.prepared.lock().unwrap().clone() {
            return Ok(p);
        }
        let paths = self.paths.lock().unwrap().clone();
        let r = prepare_models(paths.models.as_deref(), &self.core.data_dir.join("ocr").join("tessdata")).and_then(|(dir, langs)| {
            let exe = paths.tesseract.clone().ok_or_else(|| engine_missing("No OCR engine is installed."))?;
            let v = std::process::Command::new(&exe)
                .arg("--version")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .map_err(|e| engine_missing(format!("The OCR engine could not be started ({}): {e}", exe.display())))?;
            let text = String::from_utf8_lossy(if v.stdout.is_empty() { &v.stderr } else { &v.stdout }).to_string();
            let engine = text.lines().next().unwrap_or("tesseract").trim().to_string();
            let mut st = self.status.lock().unwrap();
            st.engine = Some(engine);
            Ok((dir, langs))
        });
        let mut st = self.status.lock().unwrap();
        match &r {
            Ok((_, langs)) => {
                st.available = true;
                st.languages = langs.clone();
                st.error = None;
                st.error_code = None;
                *self.prepared.lock().unwrap() = r.as_ref().ok().cloned();
            }
            Err(e) => {
                st.available = false;
                st.error = Some(e.message.clone());
                st.error_code =
                    Some(if e.code == ErrorCode::OcrModelMissing { "ocr_model_missing".into() } else { "ocr_engine_missing".into() });
            }
        }
        r
    }

    /// Start the worker when OCR is on. Idempotent; restarts a dead task.
    pub fn ensure(self: &Arc<Self>) {
        let on = self.core.features().map(|f| f.is_on("ocr.enabled")).unwrap_or(false);
        let mut g = self.task.lock().unwrap();
        let alive = g.as_ref().map(|h| !h.is_finished()).unwrap_or(false);
        if on && !alive {
            *g = Some(tokio::spawn(run(self.clone())));
        }
    }

    /// Recognise one image: text and mean word confidence (0–100).
    pub async fn recognize(&self, image: &Path, langs: &[&str]) -> AppResult<(String, i64)> {
        Ok(parse_tsv(&self.recognize_tsv(image, langs).await?))
    }

    /// Recognise one image: Tesseract's TSV (words with boxes and confidence).
    pub async fn recognize_tsv(&self, image: &Path, langs: &[&str]) -> AppResult<String> {
        let (dir, have) = {
            let me = self.prepared.lock().unwrap().clone();
            match me {
                Some(p) => p,
                None => return Err(model_missing("OCR models are not ready.")),
            }
        };
        let use_langs: Vec<&str> = langs.iter().copied().filter(|l| have.iter().any(|h| h == l)).collect();
        if use_langs.is_empty() {
            return Err(model_missing("The OCR model for this language is not installed."));
        }
        let exe = self.paths.lock().unwrap().tesseract.clone().ok_or_else(|| engine_missing("No OCR engine is installed."))?;
        let mut cmd = tokio::process::Command::new(&exe);
        cmd.arg(image)
            .arg("stdout")
            .arg("--tessdata-dir")
            .arg(&dir)
            .arg("-l")
            .arg(use_langs.join("+"))
            // TSV via options, not the `tsv` config file (the unpacked
            // tessdata folder has no `configs/`).
            .args(["-c", "tessedit_create_tsv=1", "-c", "tessedit_create_txt=0"])
            .env("OMP_THREAD_LIMIT", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        {
            // CREATE_NO_WINDOW: no console flash on the till.
            cmd.creation_flags(0x0800_0000);
        }
        let out = tokio::time::timeout(JOB_TIMEOUT, cmd.output())
            .await
            .map_err(|_| AppError::internal("OCR took longer than two minutes and was stopped."))?
            .map_err(|e| engine_missing(format!("The OCR engine could not be started: {e}")))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            return Err(AppError::validation(format!("The image could not be read: {}", err.lines().last().unwrap_or("unknown error"))));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

fn engine_missing(msg: impl Into<String>) -> AppError {
    AppError::new(ErrorCode::Conflict, msg).with_details(json!({ "kind": "ocr_engine_missing" }))
}

/// Verify `models.json` hashes and unpack `<lang>.traineddata.gz` files.
fn prepare_models(src: Option<&Path>, dest: &Path) -> AppResult<(PathBuf, Vec<String>)> {
    let src = src.ok_or_else(|| model_missing("The OCR models are not installed. Reinstall AMWAPOS to restore them."))?;
    let manifest: serde_json::Value = std::fs::read(src.join("models.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .ok_or_else(|| model_missing("The OCR model list (models.json) is missing or damaged."))?;
    let models = manifest.get("models").and_then(|m| m.as_object()).ok_or_else(|| model_missing("The OCR model list is empty."))?;
    std::fs::create_dir_all(dest)?;
    let mut langs = vec![];
    for (lang, meta) in models {
        let want = meta.get("sha256").and_then(|v| v.as_str()).unwrap_or_default();
        let gz = src.join(format!("{lang}.traineddata.gz"));
        let Ok(bytes) = std::fs::read(&gz) else {
            tracing::warn!(lang, "OCR model file missing");
            continue;
        };
        if hex::encode(Sha256::digest(&bytes)) != want {
            tracing::warn!(lang, "OCR model checksum mismatch; not used");
            continue;
        }
        let out = dest.join(format!("{lang}.traineddata"));
        let stamp = dest.join(format!("{lang}.sha256"));
        if std::fs::read_to_string(&stamp).ok().as_deref() != Some(want) || !out.exists() {
            let mut d = flate2::read::GzDecoder::new(&bytes[..]);
            let mut buf = Vec::with_capacity(bytes.len() * 2);
            std::io::Read::read_to_end(&mut d, &mut buf).map_err(|_| model_missing(format!("The {lang} OCR model is damaged.")))?;
            let tmp = out.with_extension("part");
            std::fs::write(&tmp, &buf)?;
            std::fs::rename(&tmp, &out)?;
            std::fs::write(&stamp, want)?;
        }
        langs.push(lang.clone());
    }
    if !langs.iter().any(|l| l == "eng") {
        return Err(model_missing("The English OCR model is missing or damaged. OCR stays off until AMWAPOS is reinstalled."));
    }
    langs.sort();
    Ok((dest.to_path_buf(), langs))
}

/// Tesseract TSV → text (one line per OCR line) and mean word confidence.
fn parse_tsv(tsv: &str) -> (String, i64) {
    let mut lines: Vec<String> = vec![];
    let mut key = (String::new(), String::new(), String::new(), String::new());
    let (mut sum, mut n) = (0f64, 0f64);
    for row in tsv.lines().skip(1) {
        let c: Vec<&str> = row.split('\t').collect();
        if c.len() < 12 || c[0] != "5" {
            continue;
        }
        let word = c[11].trim();
        if word.is_empty() {
            continue;
        }
        if let Ok(conf) = c[10].parse::<f64>() {
            if conf >= 0.0 {
                sum += conf;
                n += 1.0;
            }
        }
        let k = (c[1].to_string(), c[2].to_string(), c[3].to_string(), c[4].to_string());
        if k != key || lines.is_empty() {
            lines.push(word.to_string());
            key = k;
        } else if let Some(l) = lines.last_mut() {
            l.push(' ');
            l.push_str(word);
        }
    }
    (lines.join("\n"), if n > 0.0 { (sum / n).round() as i64 } else { 0 })
}

/// Languages for supplier documents: English and Arabic (mixed invoices).
const DOC_LANGS: &[&str] = &["eng", "ara"];

/// OCR one page image: every preprocessing variant is read, and the variant
/// (and, when the upright read is poor, the quarter-turn) with the best
/// measured OCR score is kept. Returns the page layout and, when the kept
/// image differs from the original file, that image for the review screen.
#[allow(clippy::too_many_arguments)]
async fn read_page(
    w: &Arc<OcrWorker>,
    img: Option<dq::image::DynamicImage>,
    original: &Path,
    number: u32,
    work: &Path,
    file_bytes: Option<u64>,
    is_jpeg: bool,
    derived_always: bool,
) -> AppResult<(dq::layout::Page, Option<dq::quality::PageQuality>, Option<u64>, Option<String>)> {
    let Some(img) = img else {
        // Not decodable here (e.g. an unusual TIFF): let Tesseract try the file as is.
        let tsv = w.recognize_tsv(original, DOC_LANGS).await?;
        let mut p = dq::layout::page_from_tsv(&tsv, number, 0, 0);
        p.variant = "original_undecoded".into();
        return Ok((p, None, None, None));
    };
    let (q, hash, variants) = {
        let img = img.clone();
        blocking(move || {
            let q = dq::quality::assess(&img, number, file_bytes, is_jpeg);
            let h = dq::quality::dhash(&img);
            let v: Vec<(String, dq::image::DynamicImage)> =
                dq::quality::variants(&img, &q).into_iter().map(|(n, i)| (n.to_string(), i)).collect();
            Ok((q, h, v))
        })
        .await?
    };
    std::fs::create_dir_all(work)?;
    let mut best: Option<(i64, dq::layout::Page, dq::image::DynamicImage, String, u32)> = None;
    for (name, v) in variants {
        let file = if name == "original" && !derived_always { original.to_path_buf() } else { work.join(format!("p{number}-{name}.png")) };
        if file != original {
            let (v2, f2) = (v.clone(), file.clone());
            blocking(move || v2.save(&f2).map_err(|e| AppError::internal(format!("could not write page image: {e}")))).await?;
        }
        let tsv = w.recognize_tsv(&file, DOC_LANGS).await?;
        let (pw, ph) = (v.width(), v.height());
        let mut page = dq::layout::page_from_tsv(&tsv, number, pw, ph);
        page.variant = name.clone();
        let score = dq::quality::ocr_score(&page);
        if best.as_ref().is_none_or(|b| score > b.0) {
            best = Some((score, page, v, name, 0));
        }
    }
    let (mut score, mut page, mut chosen, mut name, mut rot) = best.ok_or_else(|| AppError::internal("no page variant"))?;
    // Poor upright read: try quarter turns (orientation by OCR signal).
    let words = page.lines.iter().map(|l| l.words.len()).sum::<usize>();
    if words < 12 || (dq::layout::Layout { pages: vec![page.clone()] }).mean_conf() < 55 || dq::quality::looks_rotated(&page) {
        let base = chosen.clone();
        for (deg, r) in blocking(move || Ok(dq::quality::rotations(&base))).await? {
            let file = work.join(format!("p{number}-rot{deg}.png"));
            let (r2, f2) = (r.clone(), file.clone());
            blocking(move || r2.save(&f2).map_err(|e| AppError::internal(format!("could not write page image: {e}")))).await?;
            let tsv = w.recognize_tsv(&file, DOC_LANGS).await?;
            let mut p = dq::layout::page_from_tsv(&tsv, number, r.width(), r.height());
            let s2 = dq::quality::ocr_score(&p);
            // A clearly better read (20%), or an upright read nearly as good
            // as a sideways one (upright text keeps table columns usable).
            let upright_fix = dq::quality::looks_rotated(&page) && !dq::quality::looks_rotated(&p) && s2 * 10 >= score * 9;
            if s2 > score + score / 5 + 50 || upright_fix {
                p.variant = name.clone();
                (score, page, chosen, rot) = (s2, p, r, deg);
            }
        }
    }
    page.rotation = rot;
    if rot != 0 {
        name = format!("{name}+rot{rot}");
        page.variant = name.clone();
    }
    // The review screen must show the image the boxes refer to.
    let derived = if name != "original" || derived_always {
        let f = work.join(format!("page-{number}.png"));
        let (c2, f2) = (chosen.clone(), f.clone());
        blocking(move || c2.save(&f2).map_err(|e| AppError::internal(format!("could not write page image: {e}")))).await?;
        Some(f.to_string_lossy().into_owned())
    } else {
        None
    };
    let mut q = q;
    if rot != 0 {
        q.issues.push(format!("was turned {rot}° to read it"));
    }
    Ok((page, Some(q), Some(hash), derived))
}

/// Read a supplier document (PDF or image) into pages with layout and quality.
pub async fn read_document(w: &Arc<OcrWorker>, id: &str, path: &Path) -> AppResult<DocOcr> {
    let core = w.core.clone();
    let stage = |s: &'static str| {
        let (c, id) = (core.clone(), id.to_string());
        async move {
            let _ = blocking(move || c.doc_stage(&id, s)).await;
        }
    };
    stage("preprocessing").await;
    let bytes = tokio::fs::read(path).await.map_err(|e| AppError::validation(format!("The uploaded file could not be opened: {e}")))?;
    let work = w.core.data_dir.join("invoice-scans").join(format!("{id}-pages"));
    let mut out = DocOcr::default();
    let mut qualities = vec![];
    let mut hashes = vec![];
    if bytes.starts_with(b"%PDF") {
        let b = bytes.clone();
        let pages = blocking(move || dq::pdfdoc::read_pdf(&b)).await?;
        out.page_count = pages.len() as u32;
        stage("ocr").await;
        for p in pages {
            if p.has_text() {
                let mut l = dq::layout::Layout::from_text(&p.text, 100).pages.remove(0);
                l.number = p.number;
                l.source = "pdf_text".into();
                out.layout.pages.push(l);
                continue;
            }
            match p.image {
                Some(img_bytes) => {
                    let ext = if p.image_is_jpeg { "jpg" } else { "png" };
                    std::fs::create_dir_all(&work)?;
                    let f = work.join(format!("src-{}.{ext}", p.number));
                    std::fs::write(&f, &img_bytes)?;
                    let n = img_bytes.len() as u64;
                    let dec = blocking(move || Ok(dq::decode_page(&img_bytes))).await?;
                    let (page, q, h, derived) = read_page(w, dec, &f, p.number, &work, Some(n), p.image_is_jpeg, true).await?;
                    out.layout.pages.push(page);
                    qualities.extend(q);
                    hashes.extend(h);
                    if let Some(d) = derived {
                        out.page_images.push((p.number, d));
                    }
                }
                None => out.notes.push(format!(
                    "Page {}: {}",
                    p.number,
                    p.note.unwrap_or_else(|| "has no text layer and no page image to read.".into())
                )),
            }
        }
    } else {
        out.page_count = 1;
        let n = bytes.len() as u64;
        let is_jpeg = bytes.starts_with(&[0xFF, 0xD8]);
        // A picture larger than any page is refused before OCR, which would
        // otherwise try to hold it all in memory.
        if let Some((w, h)) = dq::page_oversized(&bytes) {
            return Err(AppError::validation(format!(
                "The image is too large to read ({w} × {h} pixels). Upload a photo or scan under {} pixels per side.",
                dq::MAX_PAGE_SIDE
            )));
        }
        let dec = blocking(move || Ok(dq::decode_page(&bytes))).await?;
        if dec.is_none() {
            out.notes.push("Page 1: the image could not be decoded for the quality check; it was read as uploaded.".into());
        }
        stage("ocr").await;
        let (page, q, h, derived) = read_page(w, dec, path, 1, &work, Some(n), is_jpeg, false).await?;
        out.layout.pages.push(page);
        qualities.extend(q);
        hashes.extend(h);
        if let Some(d) = derived {
            out.page_images.push((1, d));
        }
    }
    stage("extracting").await;
    if !qualities.is_empty() {
        out.quality = Some(dq::quality::combine(qualities, &hashes));
    }
    Ok(out)
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> AppResult<T> + Send + 'static) -> AppResult<T> {
    tokio::task::spawn_blocking(f).await.map_err(|e| AppError::internal(format!("worker failed: {e}")))?
}

/// `ocr.ai_parse`: ask the configured AI provider to extract the invoice
/// lines from the OCR text. Any failure keeps the rules parser's result.
async fn ai_parse(core: &Arc<AppCore>, scan_id: &str) {
    if let Err(e) = ai_parse_now(core, scan_id).await {
        tracing::info!(error = %e.message, "AI invoice parse not applied; rules parse kept");
    }
}

/// One AI parse of a scan in review. Ok(true) when the lines were replaced.
/// Also behind the "Improve parse" button (`invoicescan.ai_parse`).
pub async fn ai_parse_now(core: &Arc<AppCore>, scan_id: &str) -> AppResult<bool> {
    // The revision the AI starts from: a person's edit meanwhile makes the
    // result stale, and a stale result is dropped (never merged over the edit).
    let (c, id) = (core.clone(), scan_id.to_string());
    let revision = blocking(move || c.doc_revision(&id)).await?;
    let (c, id) = (core.clone(), scan_id.to_string());
    let Some(turn) = blocking(move || c.ocr_ai_parse_turn(&id)).await? else {
        return Err(AppError::conflict(
            "AI parsing needs the 'AI reads invoices' switch, a real AI provider with consent, and a scan waiting for review.",
        ));
    };
    let reply = crate::ai_client::complete_once(&turn).await?;
    let v = crate::ai_client::json_object(&reply)
        .ok_or_else(|| AppError::new(ErrorCode::AiProviderError, "The AI reply was not a JSON object; the rules parse was kept."))?;
    let (c, id) = (core.clone(), scan_id.to_string());
    blocking(move || c.inv_apply_ai_parse_at(&id, &v, Some(revision))).await
}

async fn run(w: Arc<OcrWorker>) {
    loop {
        let c = w.core.clone();
        let on = blocking(move || c.features().map(|f| f.is_on("ocr.enabled"))).await.unwrap_or(false);
        if !on {
            return;
        }
        let w2 = w.clone();
        if blocking(move || w2.prepare()).await.is_err() {
            // Stay off; check again later (a reinstall may restore the models).
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(300)) => {}
                _ = w.poke.notified() => {}
            }
            continue;
        }
        let c = w.core.clone();
        let jobs = blocking(move || c.ocr_pending(2)).await.unwrap_or_default();
        for j in jobs {
            if j.kind == "invoice" {
                // Supplier documents: the Document Intelligence reader.
                match read_document(&w, &j.id, Path::new(&j.path)).await {
                    Ok(doc) => {
                        let (c, id) = (w.core.clone(), j.id.clone());
                        if let Err(e) = blocking(move || c.doc_ocr_done(&id, doc)).await {
                            tracing::warn!(error = %e.message, "document analysis failed");
                            let (c, id, m) = (w.core.clone(), j.id.clone(), e.message.clone());
                            let _ = blocking(move || c.doc_failed(&id, &m)).await;
                        }
                        ai_parse(&w.core, &j.id).await;
                    }
                    Err(e) if e.code == ErrorCode::OcrModelMissing => break,
                    Err(e) => {
                        let (c, id, m) = (w.core.clone(), j.id.clone(), e.message.clone());
                        let _ = blocking(move || c.doc_failed(&id, &m)).await;
                    }
                }
                w.status.lock().unwrap().last_job_at = Some(amwapos_core::time::now_str());
                continue;
            }
            // Payment screenshots are often Arabic.
            let langs: &[&str] = &["eng", "ara"];
            let outcome = match w.recognize(Path::new(&j.path), langs).await {
                Ok(r) => Ok(r),
                Err(e) if e.code == ErrorCode::OcrModelMissing => break,
                Err(e) => Err(e.message),
            };
            let c = w.core.clone();
            let (kind, id) = (j.kind, j.id.clone());
            let _ = blocking(move || c.ocr_result(kind, &id, outcome)).await;
            w.status.lock().unwrap().last_job_at = Some(amwapos_core::time::now_str());
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(4)) => {}
            _ = w.poke.notified() => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tsv_lines_and_confidence() {
        let tsv = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n\
                   5\t1\t1\t1\t1\t1\t0\t0\t1\t1\t90\tAmount\n5\t1\t1\t1\t1\t2\t0\t0\t1\t1\t80\tBHD\n\
                   5\t1\t1\t1\t1\t3\t0\t0\t1\t1\t70\t12.500\n5\t1\t1\t1\t2\t1\t0\t0\t1\t1\t60\tRef\n4\t1\t1\t1\t2\t0\t0\t0\t1\t1\t-1\t\n";
        let (text, conf) = parse_tsv(tsv);
        assert_eq!(text, "Amount BHD 12.500\nRef");
        assert_eq!(conf, 75);
    }

    #[test]
    fn missing_models_are_reported_as_ocr_model_missing() {
        let d = tempfile::tempdir().unwrap();
        let e = prepare_models(None, d.path()).unwrap_err();
        assert_eq!(e.code, ErrorCode::OcrModelMissing);
        std::fs::write(d.path().join("models.json"), r#"{"models":{"eng":{"sha256":"00"}}}"#).unwrap();
        std::fs::write(d.path().join("eng.traineddata.gz"), b"not the model").unwrap();
        let e = prepare_models(Some(d.path()), &d.path().join("out")).unwrap_err();
        assert_eq!(e.code, ErrorCode::OcrModelMissing, "a checksum mismatch counts as missing");
    }
}
