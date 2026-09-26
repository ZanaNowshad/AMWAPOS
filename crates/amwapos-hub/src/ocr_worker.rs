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
        Ok(parse_tsv(&String::from_utf8_lossy(&out.stdout)))
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
    blocking(move || c.inv_apply_ai_parse(&id, &v)).await
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
            // Payment screenshots are often Arabic; supplier invoices mostly English.
            let langs: &[&str] = if j.kind == "payment" { &["eng", "ara"] } else { &["eng"] };
            let outcome = match w.recognize(Path::new(&j.path), langs).await {
                Ok(r) => Ok(r),
                Err(e) if e.code == ErrorCode::OcrModelMissing => break,
                Err(e) => Err(e.message),
            };
            let c = w.core.clone();
            let (kind, id) = (j.kind, j.id.clone());
            let _ = blocking(move || c.ocr_result(kind, &id, outcome)).await;
            if kind == "invoice" {
                ai_parse(&w.core, &j.id).await;
            }
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
