//! Product images.
//!
//! A product points at one stored image by content hash (`products.image_hash`).
//! Every image, uploaded or found, is decoded (which proves it is a real,
//! uncorrupted image whatever its name or claimed type), fitted inside
//! 512×512, flattened onto white and re-encoded as JPEG before it is stored in
//! `product_images`. The stored copy is what every screen shows, so nothing
//! depends on an outside URL after it is saved, and the row sync carries it to
//! every till. Images are content-addressed: two products with the same
//! picture share one row, and a row is deleted only when no product uses it.
//!
//! Precedence: manual upload > automatic image > placeholder.
//!
//! Automatic discovery is on by default for new products created without a
//! picture. Two switches turn it off: the "Find pictures automatically"
//! setting (`catalog.image_search.enabled`) and, for administrators, the
//! environment variable `AMWAPOS_IMAGE_SEARCH=off`.
//!
//! Lifecycle (persisted in `products.auto_image_status`, migration 0020):
//!
//! ```text
//! not_attempted ──(create while active / backfill / "Find picture")──► pending
//! pending ──claim──► processing ──► found | not_found | failed      (terminal)
//!                         └──transient error, attempts < 3──► pending (with a delay)
//! manual upload or removal at any point ──► skipped                   (terminal)
//! ```
//!
//! One lifecycle is one logical discovery; `auto_image_attempts` counts the
//! bounded retries inside it. Nothing but an explicit person action ("Find
//! picture automatically", or the backfill for never-searched products)
//! starts a new lifecycle: reads, renders, edits, restarts and sync never do.
//! The network part lives in the hub's image worker, which claims work here
//! and reports the outcome back.

use std::collections::HashMap;
use std::io::Cursor;

use image::{DynamicImage, ImageFormat, ImageReader, Limits};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::audit;
use crate::error::{AppError, AppResult};
use crate::service::AppCore;
use crate::settings;
use crate::time;
use crate::validate;

/// Largest file accepted (upload or download), before decoding.
pub const MAX_SOURCE_BYTES: usize = 8 * 1024 * 1024;
/// Stored images fit inside this square (2× the largest on-screen tile).
pub const MAX_SIDE: u32 = 512;
/// Smallest usable source image.
pub const MIN_SIDE: u32 = 64;
/// Lookups are retried at most this many times after transient errors.
pub const MAX_AUTO_ATTEMPTS: i64 = 3;
/// A claim older than this is treated as abandoned (the worker stopped).
pub const STALE_CLAIM_MINUTES: i64 = 10;
/// Secret-store slot for the optional web image search key.
pub const SECRET_GOOGLE_KEY: &str = "catalog.image_search.google_key";

/// A validated, normalized image ready to store.
#[derive(Debug, Clone)]
pub struct Normalized {
    pub hash: String,
    pub jpeg: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// Size of the source image before fitting.
    pub source_width: u32,
    pub source_height: u32,
    /// Share (0–1) of near-white pixels on the outer frame: a packshot on a
    /// white background scores high, a lifestyle photo low.
    pub white_border: f32,
}

/// Decode, check, fit and re-encode. The format is taken from the bytes
/// (magic numbers), never from a file name or a claimed MIME type.
pub fn normalize(bytes: &[u8]) -> AppResult<Normalized> {
    if bytes.is_empty() {
        return Err(AppError::validation("The image file is empty."));
    }
    if bytes.len() > MAX_SOURCE_BYTES {
        return Err(AppError::validation("The image is larger than 8 MB."));
    }
    let fmt = image::guess_format(bytes).map_err(|_| unsupported())?;
    if !matches!(fmt, ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP | ImageFormat::Gif) {
        return Err(unsupported());
    }
    let mut reader = ImageReader::with_format(Cursor::new(bytes), fmt);
    let mut limits = Limits::default();
    limits.max_image_width = Some(10_000);
    limits.max_image_height = Some(10_000);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    let img = reader.decode().map_err(|_| AppError::validation("The image is damaged or could not be read."))?;
    let (sw, sh) = (img.width(), img.height());
    if sw < MIN_SIDE || sh < MIN_SIDE {
        return Err(AppError::validation(format!("The image is too small (at least {MIN_SIDE}×{MIN_SIDE} pixels).")));
    }
    let fitted = if sw > MAX_SIDE || sh > MAX_SIDE { img.resize(MAX_SIDE, MAX_SIDE, image::imageops::FilterType::CatmullRom) } else { img };
    let rgb = flatten_on_white(&fitted);
    let white_border = white_border_ratio(&rgb);
    let mut jpeg = Vec::new();
    rgb.write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 85))
        .map_err(|e| AppError::internal(format!("image encode failed: {e}")))?;
    let hash = hex::encode(Sha256::digest(&jpeg));
    Ok(Normalized { hash, width: rgb.width(), height: rgb.height(), jpeg, source_width: sw, source_height: sh, white_border })
}

fn unsupported() -> AppError {
    AppError::validation("Choose a PNG, JPEG, WEBP or GIF image.")
}

/// Transparent areas become white (packaged goods are shown on white).
fn flatten_on_white(img: &DynamicImage) -> image::RgbImage {
    let rgba = img.to_rgba8();
    let mut out = image::RgbImage::new(rgba.width(), rgba.height());
    for (x, y, p) in rgba.enumerate_pixels() {
        let a = p[3] as u32;
        let mix = |c: u8| ((c as u32 * a + 255 * (255 - a)) / 255) as u8;
        out.put_pixel(x, y, image::Rgb([mix(p[0]), mix(p[1]), mix(p[2])]));
    }
    out
}

fn white_border_ratio(img: &image::RgbImage) -> f32 {
    let (w, h) = (img.width(), img.height());
    let band = ((w.min(h) as f32) * 0.06).ceil().max(1.0) as u32;
    let (mut white, mut total) = (0u64, 0u64);
    for y in 0..h {
        for x in 0..w {
            if x >= band && x < w - band && y >= band && y < h - band {
                continue;
            }
            let p = img.get_pixel(x, y);
            let (lo, hi) = (p.0.iter().min().copied().unwrap_or(0), p.0.iter().max().copied().unwrap_or(0));
            total += 1;
            if lo >= 232 && hi - lo <= 14 {
                white += 1;
            }
        }
    }
    if total == 0 {
        0.0
    } else {
        white as f32 / total as f32
    }
}

/// Store a normalized image (idempotent by hash).
pub(crate) fn store(c: &Connection, n: &Normalized) -> AppResult<()> {
    c.execute(
        "INSERT OR IGNORE INTO product_images(image_hash, mime, width, height, bytes, data_b64, created_at)
         VALUES (?1,'image/jpeg',?2,?3,?4,?5,?6)",
        params![n.hash, n.width, n.height, n.jpeg.len() as i64, crate::ids::b64(&n.jpeg), time::now_str()],
    )?;
    Ok(())
}

/// Delete a stored image once no product refers to it.
pub(crate) fn collect(c: &Connection, hash: Option<&str>) -> AppResult<()> {
    if let Some(h) = hash {
        c.execute("DELETE FROM product_images WHERE image_hash=?1 AND NOT EXISTS (SELECT 1 FROM products WHERE image_hash=?1)", [h])?;
    }
    Ok(())
}

/// Where the image search looks and which sources it may use.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ImageSearchSettings {
    /// Look for a picture automatically for new products created without
    /// one. On by default; turning it off stops all outside searches.
    pub enabled: bool,
    /// The default method: barcode + product name sent as the query of
    /// Bing's image thumbnail address (`https://tse1.mm.bing.net/th?q=…`),
    /// which answers with one picture for that search. No key needed.
    /// Asked first; a picture it returns is used.
    pub bing_thumbnail: bool,
    /// Open Food Facts: exact barcode lookup, no key needed.
    pub open_food_facts: bool,
    /// Bing image results (the public results page, no key; the approach of
    /// the `bing-image-urls` package). Unofficial: may change or be limited.
    pub bing: bool,
    /// Google Programmable Search (image search); needs a key and an engine id.
    pub google: bool,
    /// Programmable Search Engine id (cx). The API key is in the secret store.
    pub google_cx: String,
    /// Two-letter market for relevance (gl / cr), e.g. "bh".
    pub region: String,
    /// Interface language hint ("en" or "ar").
    pub language: String,
}
impl Default for ImageSearchSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            bing_thumbnail: true,
            open_food_facts: true,
            bing: true,
            google: false,
            google_cx: String::new(),
            region: "bh".into(),
            language: "en".into(),
        }
    }
}
pub const KEY_IMAGE_SEARCH: &str = "catalog.image_search";

/// Administrator kill switch: `AMWAPOS_IMAGE_SEARCH=off` (or 0 / false / no)
/// stops every outside image search on this computer, whatever the settings.
pub fn search_disabled_by_environment() -> bool {
    std::env::var("AMWAPOS_IMAGE_SEARCH")
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "off" | "0" | "false" | "no"))
        .unwrap_or(false)
}

impl ImageSearchSettings {
    /// At least one source can run (Google needs its engine id and key).
    pub fn has_source(&self, google_key_set: bool) -> bool {
        self.bing_thumbnail || self.open_food_facts || self.bing || self.google_ready(google_key_set)
    }
    pub fn google_ready(&self, google_key_set: bool) -> bool {
        self.google && !self.google_cx.trim().is_empty() && google_key_set
    }
}

/// Whether automatic discovery can run, and if not, why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    Active,
    /// The "Find pictures automatically" setting is off.
    SwitchedOff,
    /// `AMWAPOS_IMAGE_SEARCH=off` on this computer.
    DisabledByAdministrator,
    /// Every source is switched off (or Google alone, without its key / id).
    NoSources,
}

impl Availability {
    pub fn as_str(self) -> &'static str {
        match self {
            Availability::Active => "active",
            Availability::SwitchedOff => "switched_off",
            Availability::DisabledByAdministrator => "disabled_by_administrator",
            Availability::NoSources => "no_sources",
        }
    }
}

/// Terminal states: a lifecycle that reached one of these never restarts on
/// its own.
pub const TERMINAL_STATES: &[&str] = &["found", "not_found", "failed", "skipped"];

/// One product the worker should look up (claimed; status = processing).
#[derive(Debug, Clone, Serialize)]
pub struct AutoImageJob {
    pub product_id: String,
    pub name: String,
    pub name_ar: Option<String>,
    pub sku: String,
    pub barcodes: Vec<String>,
    pub category: Option<String>,
    pub attempt: i64,
}

/// What the worker found.
pub enum AutoOutcome {
    Found {
        image: Normalized,
        note: Value,
    },
    NotFound {
        note: Value,
    },
    /// A network, quota or provider error: retried later (bounded).
    Transient {
        note: Value,
    },
    /// Nothing can be done (for example no source is configured).
    Failed {
        note: Value,
    },
}

/// The image state of one product, for the editor.
#[derive(Debug, Clone, Serialize)]
pub struct ProductImageState {
    pub product_id: String,
    pub image_hash: Option<String>,
    pub image_source: Option<String>,
    pub auto_image_status: String,
    pub auto_image_attempted_at: Option<String>,
    pub auto_image_note: Option<Value>,
    /// Whether automatic discovery can run right now ("active",
    /// "switched_off", "disabled_by_administrator", "no_sources").
    pub discovery: &'static str,
}

pub(crate) fn image_state(c: &Connection, product_id: &str) -> AppResult<ProductImageState> {
    c.query_row(
        "SELECT image_hash, image_source, auto_image_status, auto_image_attempted_at, auto_image_note FROM products WHERE product_id=?1",
        [product_id],
        |r| {
            let note: Option<String> = r.get(4)?;
            Ok(ProductImageState {
                product_id: product_id.to_string(),
                image_hash: r.get(0)?,
                image_source: r.get(1)?,
                auto_image_status: r.get(2)?,
                auto_image_attempted_at: r.get(3)?,
                auto_image_note: note.and_then(|n| serde_json::from_str(&n).ok()),
                discovery: "active",
            })
        },
    )
    .optional()?
    .ok_or_else(|| AppError::not_found("Product"))
}

/// Set a manual image on a product inside an open transaction. The new image
/// is stored before the reference moves; the old one is collected after.
pub(crate) fn set_manual(c: &Connection, product_id: &str, n: &Normalized) -> AppResult<Option<String>> {
    let old: Option<String> = c
        .query_row("SELECT image_hash FROM products WHERE product_id=?1", [product_id], |r| r.get(0))
        .optional()?
        .ok_or_else(|| AppError::not_found("Product"))?;
    store(c, n)?;
    c.execute(
        "UPDATE products SET image_hash=?2, image_source='manual',
            auto_image_status=CASE WHEN auto_image_status IN ('found','not_found','failed') THEN auto_image_status ELSE 'skipped' END,
            auto_image_next_at=NULL, updated_at=?3, version=version+1
         WHERE product_id=?1",
        params![product_id, n.hash, time::now_str()],
    )?;
    if old.as_deref() != Some(n.hash.as_str()) {
        collect(c, old.as_deref())?;
    }
    Ok(old)
}

pub(crate) fn decode_b64_image(data_b64: &str) -> AppResult<Normalized> {
    let data = data_b64.split_once("base64,").map(|x| x.1).unwrap_or(data_b64).trim();
    if data.len() > MAX_SOURCE_BYTES * 4 / 3 + 16 {
        return Err(AppError::validation("The image is larger than 8 MB."));
    }
    let bytes = crate::ids::b64_decode(data).ok_or_else(|| AppError::validation("The image could not be read."))?;
    normalize(&bytes)
}

fn backoff_minutes(attempt: i64) -> i64 {
    match attempt {
        1 => 5,
        2 => 60,
        _ => 360,
    }
}

fn discovery_off() -> AppError {
    AppError::conflict("Automatic product pictures are switched off. An owner can turn them on in Settings → Product images.")
        .with_details(json!({ "kind": "auto_images_off" }))
}

impl AppCore {
    /// Whether automatic discovery can run now (settings, environment and
    /// configured sources). Cheap: one settings read and a secret-store probe
    /// only when Google is the sole source.
    pub fn auto_image_availability(&self) -> AppResult<Availability> {
        if search_disabled_by_environment() {
            return Ok(Availability::DisabledByAdministrator);
        }
        let cfg: ImageSearchSettings = self.db.read(|c| settings::get(c, KEY_IMAGE_SEARCH))?;
        if !cfg.enabled {
            return Ok(Availability::SwitchedOff);
        }
        let key_set = if cfg.bing_thumbnail || cfg.open_food_facts || cfg.bing { true } else { self.google_key_set()? };
        Ok(if cfg.has_source(key_set) { Availability::Active } else { Availability::NoSources })
    }

    /// Discovery can run: new products are queued and the worker may claim.
    pub fn auto_images_active(&self) -> bool {
        matches!(self.auto_image_availability(), Ok(Availability::Active))
    }

    fn google_key_set(&self) -> AppResult<bool> {
        Ok(self.secrets.get(SECRET_GOOGLE_KEY)?.is_some_and(|k| !k.trim().is_empty()))
    }

    fn image_state_view(&self, product_id: &str) -> AppResult<ProductImageState> {
        let mut st = self.db.read(|c| image_state(c, product_id))?;
        st.discovery = self.auto_image_availability()?.as_str();
        Ok(st)
    }

    /// Upload (or replace) a product's image. The image is validated and
    /// stored before the product's reference moves to it.
    pub fn product_image_upload(&self, token: &str, product_id: &str, data_b64: &str) -> AppResult<ProductImageState> {
        let s = self.session(token)?;
        s.require("products.manage")?;
        self.require_back_office_writable()?;
        let pid = validate::id(product_id, "Product")?;
        let n = decode_b64_image(data_b64)?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let old = set_manual(tx, &pid, &n)?;
            audit::record(
                tx,
                &actor,
                "product.image_set",
                "product",
                Some(&pid),
                Some(&json!({ "image_hash": old })),
                Some(&json!({ "image_hash": n.hash, "source": "manual", "width": n.width, "height": n.height })),
            )?;
            Ok(())
        })?;
        self.image_state_view(&pid)
    }

    /// Remove a product's image. The automatic lookup is not re-queued: the
    /// state becomes `skipped` (a person decided); "Find image" re-queues it
    /// explicitly.
    pub fn product_image_remove(&self, token: &str, product_id: &str) -> AppResult<ProductImageState> {
        let s = self.session(token)?;
        s.require("products.manage")?;
        self.require_back_office_writable()?;
        let pid = validate::id(product_id, "Product")?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let st = image_state(tx, &pid)?;
            tx.execute(
                "UPDATE products SET image_hash=NULL, image_source=NULL, auto_image_status='skipped', auto_image_next_at=NULL,
                    updated_at=?2, version=version+1 WHERE product_id=?1",
                params![pid, time::now_str()],
            )?;
            collect(tx, st.image_hash.as_deref())?;
            audit::record(
                tx,
                &actor,
                "product.image_removed",
                "product",
                Some(&pid),
                Some(&json!({ "image_hash": st.image_hash, "source": st.image_source })),
                None,
            )?;
            Ok(())
        })?;
        self.image_state_view(&pid)
    }

    /// A person asks for the automatic lookup (again) for a product with no
    /// image. Never replaces a manual image.
    pub fn product_image_find(&self, token: &str, product_id: &str) -> AppResult<ProductImageState> {
        let s = self.session(token)?;
        s.require("products.manage")?;
        self.require_back_office_writable()?;
        if !self.auto_images_active() {
            return Err(discovery_off());
        }
        let pid = validate::id(product_id, "Product")?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let st = image_state(tx, &pid)?;
            if st.image_hash.is_some() {
                return Err(AppError::conflict("This product already has an image. Remove it first to search again."));
            }
            if st.auto_image_status == "processing" || st.auto_image_status == "pending" {
                return Ok(());
            }
            tx.execute(
                "UPDATE products SET auto_image_status='pending', auto_image_attempts=0, auto_image_next_at=NULL WHERE product_id=?1",
                [&pid],
            )?;
            audit::record(tx, &actor, "product.image_search_requested", "product", Some(&pid), None, None)?;
            tracing::info!(product_id = %pid, previous = %st.auto_image_status, "product image discovery queued (requested)");
            Ok(())
        })?;
        self.image_state_view(&pid)
    }

    pub fn product_image_state(&self, token: &str, product_id: &str) -> AppResult<ProductImageState> {
        let s = self.session(token)?;
        s.require("products.view")?;
        let pid = validate::id(product_id, "Product")?;
        self.image_state_view(&pid)
    }

    /// Stored images by hash, as data URLs, for any signed-in screen (the
    /// till shows product pictures too). At most 60 per call; unknown hashes
    /// are left out. Read-only; never touches the network.
    pub fn product_images_get(&self, token: &str, hashes: Vec<String>) -> AppResult<HashMap<String, String>> {
        self.session(token)?;
        if hashes.len() > 60 {
            return Err(AppError::validation("Ask for at most 60 images at a time."));
        }
        let hashes: Vec<String> = hashes.into_iter().filter(|h| h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit())).collect();
        self.db.read(|c| {
            let mut out = HashMap::new();
            let mut st = c.prepare_cached("SELECT data_b64 FROM product_images WHERE image_hash=?1")?;
            for h in hashes {
                if let Some(d) = st.query_row([&h], |r| r.get::<_, String>(0)).optional()? {
                    out.insert(h, format!("data:image/jpeg;base64,{d}"));
                }
            }
            Ok(out)
        })
    }

    /// Controlled backfill: queue up to `limit` active products that have no
    /// image and were never looked up. The worker then processes the queue
    /// one product at a time at its own pace. Idempotent.
    pub fn product_image_backfill(&self, token: &str, limit: Option<i64>) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("products.manage")?;
        self.require_back_office_writable()?;
        if !self.auto_images_active() {
            return Err(discovery_off());
        }
        let limit = validate::limit(limit, 200, 1000);
        let actor = self.actor(&s, None);
        let n = self.db.write(|tx| {
            let n = tx.execute(
                &format!(
                    "UPDATE products SET auto_image_status='pending', auto_image_next_at=NULL
                     WHERE product_id IN (SELECT product_id FROM products WHERE active=1 AND image_hash IS NULL
                        AND auto_image_status='not_attempted' ORDER BY created_at LIMIT {limit})"
                ),
                [],
            )?;
            audit::record(tx, &actor, "product.image_backfill", "product", None, None, Some(&json!({ "queued": n })))?;
            tracing::info!(queued = n, limit, "product image discovery queued (backfill)");
            Ok(n)
        })?;
        Ok(json!({ "queued": n }))
    }

    /// Counts per state and the search configuration (never the key itself).
    pub fn product_image_overview(&self, token: &str) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("products.view")?;
        let cfg: ImageSearchSettings = self.db.read(|c| settings::get(c, KEY_IMAGE_SEARCH))?;
        let key_set = self.google_key_set()?;
        let availability = self.auto_image_availability()?;
        self.db.read(|c| {
            let mut st = c.prepare("SELECT auto_image_status, COUNT(*) FROM products WHERE active=1 GROUP BY auto_image_status")?;
            let counts: HashMap<String, i64> = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
            let with_image: i64 =
                c.query_row("SELECT COUNT(*) FROM products WHERE active=1 AND image_hash IS NOT NULL", [], |r| r.get(0))?;
            let without: i64 = c.query_row(
                "SELECT COUNT(*) FROM products WHERE active=1 AND image_hash IS NULL AND auto_image_status='not_attempted'",
                [],
                |r| r.get(0),
            )?;
            Ok(json!({
                "enabled": cfg.enabled,
                "availability": availability.as_str(),
                "environment_disabled": search_disabled_by_environment(),
                "google_ready": cfg.google_ready(key_set),
                "sources": {
                    "bing_thumbnail": cfg.bing_thumbnail,
                    "open_food_facts": cfg.open_food_facts,
                    "bing": cfg.bing,
                    "google": cfg.google_ready(key_set),
                },
                "counts": counts,
                "with_image": with_image,
                "never_searched": without,
                "settings": cfg,
                "google_key_set": key_set,
            }))
        })
    }

    /// Save the image-search settings and, when given, the web search key
    /// (kept in the operating system's secret store; "" removes it).
    pub fn product_image_configure(&self, token: &str, cfg: ImageSearchSettings, google_key: Option<String>) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("settings.manage")?;
        let region = cfg.region.trim().to_ascii_lowercase();
        if region.len() != 2 || !region.chars().all(|c| c.is_ascii_lowercase()) {
            return Err(AppError::validation("The market must be a two-letter country code, for example bh."));
        }
        if !["en", "ar"].contains(&cfg.language.as_str()) {
            return Err(AppError::validation("The search language must be English or Arabic."));
        }
        let cx = cfg.google_cx.trim().to_string();
        if cx.len() > 100 || !cx.chars().all(|c| c.is_ascii_alphanumeric() || c == ':' || c == '-' || c == '_') {
            return Err(AppError::validation("The search engine id has characters it cannot contain."));
        }
        let cfg = ImageSearchSettings { region, google_cx: cx, ..cfg };
        if let Some(k) = google_key.map(|k| k.trim().to_string()) {
            if k.is_empty() {
                self.secrets.delete(SECRET_GOOGLE_KEY)?;
            } else {
                if k.len() > 200 || k.chars().any(|c| c.is_whitespace()) {
                    return Err(AppError::validation("That key does not look right."));
                }
                self.secrets.set(SECRET_GOOGLE_KEY, &k)?;
            }
        }
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            settings::put(tx, KEY_IMAGE_SEARCH, &cfg, Some(&s.user_id))?;
            audit::record(tx, &actor, "settings.image_search", "settings", None, None, Some(&json!({ "settings": cfg })))?;
            Ok(())
        })?;
        self.product_image_overview(token)
    }

    /// Worker: the search settings and key (the key never leaves the backend).
    pub fn image_search_config(&self) -> AppResult<(ImageSearchSettings, Option<String>)> {
        let cfg: ImageSearchSettings = self.db.read(|c| settings::get(c, KEY_IMAGE_SEARCH))?;
        let key = self.secrets.get(SECRET_GOOGLE_KEY)?.filter(|k| !k.trim().is_empty());
        Ok((cfg, key))
    }

    /// Worker: claim the next product due for its automatic lookup. The claim
    /// is a single conditional update, so two workers never take the same
    /// product; an abandoned claim is taken back after STALE_CLAIM_MINUTES.
    pub fn auto_image_claim(&self) -> AppResult<Option<AutoImageJob>> {
        if !self.auto_images_active() {
            return Ok(None);
        }
        let now = time::now_str();
        let stale = time::fmt(time::now() - chrono::Duration::minutes(STALE_CLAIM_MINUTES));
        // Idle check with a read first: an empty queue costs no write.
        let due: bool = self.db.read(|c| {
            Ok(c.query_row(
                "SELECT EXISTS(SELECT 1 FROM products WHERE (auto_image_status='pending' AND image_hash IS NULL AND active=1
                    AND (auto_image_next_at IS NULL OR auto_image_next_at <= ?1))
                   OR (auto_image_status='processing' AND auto_image_attempted_at < ?2))",
                [&now, &stale],
                |r| r.get(0),
            )?)
        })?;
        if !due {
            return Ok(None);
        }
        self.db.write(|tx| {
            // Abandoned claims (the worker stopped mid-lookup) count as a
            // transient failure: retried after a delay, failed at the cap, so
            // a lookup that keeps crashing cannot loop.
            let stale_note = json!({ "errors": ["the lookup did not finish (worker stopped)"] }).to_string();
            let failed = tx.execute(
                "UPDATE products SET auto_image_status='failed', auto_image_next_at=NULL, auto_image_note=?3
                 WHERE auto_image_status='processing' AND auto_image_attempted_at < ?1 AND auto_image_attempts >= ?2",
                params![stale, MAX_AUTO_ATTEMPTS, stale_note],
            )?;
            let requeued = tx.execute(
                "UPDATE products SET auto_image_status='pending', auto_image_next_at=?2, auto_image_note=?3
                 WHERE auto_image_status='processing' AND auto_image_attempted_at < ?1",
                params![stale, time::fmt(time::now() + chrono::Duration::minutes(backoff_minutes(1))), stale_note],
            )?;
            if failed + requeued > 0 {
                tracing::warn!(failed, requeued, "product image discovery: abandoned claims recovered");
            }
            let pid: Option<String> = tx
                .query_row(
                    "SELECT product_id FROM products WHERE auto_image_status='pending' AND image_hash IS NULL AND active=1
                       AND (auto_image_next_at IS NULL OR auto_image_next_at <= ?1)
                     ORDER BY auto_image_next_at IS NOT NULL, created_at LIMIT 1",
                    [&now],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(pid) = pid else { return Ok(None) };
            let n = tx.execute(
                "UPDATE products SET auto_image_status='processing', auto_image_attempted_at=?2, auto_image_attempts=auto_image_attempts+1
                 WHERE product_id=?1 AND auto_image_status='pending' AND image_hash IS NULL",
                params![pid, now],
            )?;
            if n != 1 {
                return Ok(None);
            }
            let (name, name_ar, sku, category, attempt): (String, Option<String>, String, Option<String>, i64) = tx.query_row(
                "SELECT p.name, p.name_ar, p.sku, c.name, p.auto_image_attempts FROM products p
                 LEFT JOIN categories c ON c.category_id=p.category_id WHERE p.product_id=?1",
                [&pid],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )?;
            let mut st = tx.prepare("SELECT barcode FROM product_barcodes WHERE product_id=?1 ORDER BY is_primary DESC, created_at")?;
            let barcodes: Vec<String> = st.query_map([&pid], |r| r.get(0))?.collect::<Result<_, _>>()?;
            Ok(Some(AutoImageJob { product_id: pid, name, name_ar, sku, barcodes, category, attempt }))
        })
    }

    /// Worker: record the outcome of a claimed lookup. Writes only while the
    /// product is still claimed and has no image, so a manual upload made
    /// meanwhile always wins (the found image is then discarded).
    pub fn auto_image_complete(&self, product_id: &str, outcome: AutoOutcome) -> AppResult<String> {
        let now = time::now_str();
        self.db.write(|tx| {
            let attempts: Option<i64> = tx
                .query_row(
                    "SELECT auto_image_attempts FROM products WHERE product_id=?1 AND auto_image_status='processing' AND image_hash IS NULL",
                    [product_id],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(attempts) = attempts else {
                // A manual upload (or removal) happened meanwhile: it wins and
                // the automatic result is dropped before anything is stored.
                tracing::info!(product_id, "product image discovery superseded (manual image won)");
                return Ok("superseded".to_string());
            };
            let state = match outcome {
                AutoOutcome::Found { image, note } => {
                    store(tx, &image)?;
                    tx.execute(
                        "UPDATE products SET image_hash=?2, image_source='automatic', auto_image_status='found', auto_image_next_at=NULL,
                            auto_image_note=?3, updated_at=?4, version=version+1
                         WHERE product_id=?1 AND auto_image_status='processing' AND image_hash IS NULL",
                        params![product_id, image.hash, note.to_string(), now],
                    )?;
                    "found"
                }
                AutoOutcome::NotFound { note } => {
                    tx.execute(
                        "UPDATE products SET auto_image_status='not_found', auto_image_next_at=NULL, auto_image_note=?2 WHERE product_id=?1",
                        params![product_id, note.to_string()],
                    )?;
                    "not_found"
                }
                AutoOutcome::Failed { note } => {
                    tx.execute(
                        "UPDATE products SET auto_image_status='failed', auto_image_next_at=NULL, auto_image_note=?2 WHERE product_id=?1",
                        params![product_id, note.to_string()],
                    )?;
                    "failed"
                }
                AutoOutcome::Transient { note } => {
                    if attempts >= MAX_AUTO_ATTEMPTS {
                        tx.execute(
                            "UPDATE products SET auto_image_status='failed', auto_image_next_at=NULL, auto_image_note=?2 WHERE product_id=?1",
                            params![product_id, note.to_string()],
                        )?;
                        "failed"
                    } else {
                        let next = time::fmt(time::now() + chrono::Duration::minutes(backoff_minutes(attempts)));
                        tx.execute(
                            "UPDATE products SET auto_image_status='pending', auto_image_next_at=?2, auto_image_note=?3 WHERE product_id=?1",
                            params![product_id, next, note.to_string()],
                        )?;
                        "retry"
                    }
                }
            };
            if state == "found" {
                tracing::info!(product_id, attempts, "product image persisted (automatic)");
            }
            tracing::info!(product_id, attempts, state, terminal = state != "retry", "product image discovery outcome");
            Ok(state.to_string())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn png(w: u32, h: u32, bg: [u8; 4], fg: [u8; 4]) -> Vec<u8> {
        let mut img = image::RgbaImage::from_pixel(w, h, image::Rgba(bg));
        for y in h / 4..h * 3 / 4 {
            for x in w / 4..w * 3 / 4 {
                img.put_pixel(x, y, image::Rgba(fg));
            }
        }
        let mut out = Vec::new();
        DynamicImage::ImageRgba8(img).write_to(&mut Cursor::new(&mut out), ImageFormat::Png).unwrap();
        out
    }

    #[test]
    fn normalizes_to_a_bounded_white_jpeg_and_scores_the_background() {
        let n = normalize(&png(1200, 800, [255, 255, 255, 255], [200, 20, 20, 255])).unwrap();
        assert_eq!((n.width, n.height), (512, 341));
        assert_eq!((n.source_width, n.source_height), (1200, 800));
        assert!(n.jpeg.starts_with(&[0xFF, 0xD8]));
        assert!(n.white_border > 0.95, "{}", n.white_border);
        // Transparent becomes white; a dark full-bleed photo scores low.
        let t = normalize(&png(300, 300, [0, 0, 0, 0], [10, 10, 200, 255])).unwrap();
        assert!(t.white_border > 0.95);
        let dark = normalize(&png(300, 300, [30, 60, 20, 255], [10, 10, 200, 255])).unwrap();
        assert!(dark.white_border < 0.05);
        // Same picture, same hash.
        assert_eq!(normalize(&png(300, 300, [0, 0, 0, 0], [10, 10, 200, 255])).unwrap().hash, t.hash);
    }

    #[test]
    fn rejects_non_images_damaged_files_and_tiny_images() {
        assert!(normalize(b"").is_err());
        assert!(normalize(b"<html>not an image</html>").is_err());
        assert!(normalize(b"%PDF-1.7 ...").is_err());
        let mut broken = png(200, 200, [255; 4], [0, 0, 0, 255]);
        broken.truncate(60);
        assert!(normalize(&broken).is_err(), "a truncated PNG is refused");
        assert!(normalize(&png(20, 20, [255; 4], [0, 0, 0, 255])).is_err(), "too small");
        assert!(normalize(&vec![0u8; MAX_SOURCE_BYTES + 1]).is_err(), "too large");
    }
}
