//! Product image worker (on by default; see `AppCore::auto_image_availability`).
//!
//! Runs on the hub (or a standalone till), never on a terminal: terminals get
//! the stored image with the product rows. One product at a time, a few
//! seconds apart: claim it (the core's conditional update), search, download
//! the best candidates through the SSRF-guarded fetcher, validate and
//! normalize them in the core, keep the best one or none, and report the
//! outcome. A lookup runs once per product; failures never touch selling.
//!
//! Sources, asked in this priority order (see `ImageSearchSettings`). Each is
//! isolated: its own timeout, a crash or bad reply is that source's error
//! only, and the next source is still asked. As soon as a source yields an
//! accepted picture tied to the exact barcode, lower-priority sources are not
//! asked at all; otherwise the best accepted picture across sources wins.
//! * Default: barcode + product name as the query of Bing's image thumbnail
//!   address (`https://tse1.mm.bing.net/th?q=<barcode>+<name>`). When it
//!   returns a usable picture, that picture is used and nothing else is asked.
//! * Open Food Facts: exact barcode lookup (no key). The strongest evidence.
//! * Bing image results page (no key), the approach of `bing-image-urls`:
//!   barcode + name, large white product photographs, configured market.
//! * Google Programmable Search, image search: optional, needs a key (secret
//!   store) and an engine id; asked for white-background product photos in the
//!   configured market (`gl` / `cr`), barcode + name first.
//!
//! Nothing here runs on a product read: screens only ever see stored images.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use amwapos_core::product_images::{normalize, AutoImageJob, AutoOutcome, ImageSearchSettings, Normalized, MAX_SOURCE_BYTES};
use amwapos_core::{AppCore, AppError, AppResult};
use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

/// Pause between two lookups (provider rate limits; never a burst).
const BETWEEN_JOBS: Duration = Duration::from_secs(3);
/// Idle poll when nothing is queued.
const IDLE: Duration = Duration::from_secs(30);
/// Whole lookup budget for one product.
const JOB_TIMEOUT: Duration = Duration::from_secs(90);
/// Candidates downloaded and checked per product at most.
const MAX_DOWNLOADS: usize = 6;
/// … and per source.
const MAX_DOWNLOADS_PER_SOURCE: usize = 3;
/// One source's search budget.
const PROVIDER_TIMEOUT: Duration = Duration::from_secs(25);

/// A search hit before download.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub url: String,
    pub title: String,
    pub page_url: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// The source ties this image to the exact barcode.
    pub barcode_match: bool,
    pub provider: &'static str,
}

#[derive(Debug, Clone)]
pub struct SearchError {
    /// Worth retrying later (network, 5xx, quota).
    pub transient: bool,
    pub message: String,
}

impl SearchError {
    fn transient(m: impl Into<String>) -> Self {
        Self { transient: true, message: m.into() }
    }
    fn permanent(m: impl Into<String>) -> Self {
        Self { transient: false, message: m.into() }
    }
}

#[derive(Debug, Clone)]
pub struct SearchQuery {
    pub barcode: Option<String>,
    pub name: String,
    pub name_ar: Option<String>,
    pub category: Option<String>,
    /// The words sent to a web search: "<barcode> <name>", or name + category.
    pub text: String,
    pub region: String,
    pub language: String,
}

/// A source of candidate product images. Provider code stays behind this.
#[async_trait]
pub trait ImageSearchProvider: Send + Sync {
    fn name(&self) -> &'static str;
    /// A picture this source returns is the answer: stop asking the others.
    fn decisive(&self) -> bool {
        false
    }
    async fn search(&self, q: &SearchQuery) -> Result<Vec<Candidate>, SearchError>;
}

/// A barcode worth looking up: 8–14 digits (EAN-8, UPC-A, EAN-13, GTIN-14).
pub fn usable_barcode(barcodes: &[String]) -> Option<String> {
    barcodes.iter().find(|b| (8..=14).contains(&b.len()) && b.chars().all(|c| c.is_ascii_digit())).cloned()
}

pub fn build_query(job: &AutoImageJob, cfg: &ImageSearchSettings) -> SearchQuery {
    let barcode = usable_barcode(&job.barcodes);
    let text = match &barcode {
        Some(b) => format!("{b} {}", job.name),
        None => match &job.category {
            Some(c) => format!("{} {c}", job.name),
            None => job.name.clone(),
        },
    };
    SearchQuery {
        barcode,
        name: job.name.clone(),
        name_ar: job.name_ar.clone(),
        category: job.category.clone(),
        text,
        region: cfg.region.clone(),
        language: cfg.language.clone(),
    }
}

// ---------------------------------------------------------------- providers

pub struct OpenFoodFacts {
    pub base: String,
    http: reqwest::Client,
}

impl OpenFoodFacts {
    pub fn new(base: &str) -> Self {
        Self { base: base.trim_end_matches('/').to_string(), http: api_client() }
    }
}

#[async_trait]
impl ImageSearchProvider for OpenFoodFacts {
    fn name(&self) -> &'static str {
        "open_food_facts"
    }
    async fn search(&self, q: &SearchQuery) -> Result<Vec<Candidate>, SearchError> {
        let Some(code) = &q.barcode else { return Ok(vec![]) };
        let url = format!("{}/api/v2/product/{code}?fields=code,product_name,brands,image_front_url,image_url", self.base);
        let resp = self.http.get(&url).send().await.map_err(|e| SearchError::transient(net(&e)))?;
        let status = resp.status();
        if status.as_u16() == 404 {
            return Ok(vec![]);
        }
        if status.as_u16() == 429 || status.is_server_error() {
            return Err(SearchError::transient(format!("Open Food Facts answered {status}")));
        }
        if !status.is_success() {
            return Err(SearchError::permanent(format!("Open Food Facts answered {status}")));
        }
        let v: Value = resp.json().await.map_err(|_| SearchError::transient("Open Food Facts sent an unreadable reply"))?;
        if v.get("status").and_then(|s| s.as_i64()) != Some(1) {
            return Ok(vec![]);
        }
        let p = &v["product"];
        let same_code = v.get("code").and_then(|c| c.as_str()) == Some(code.as_str());
        let title = [p["product_name"].as_str(), p["brands"].as_str()].into_iter().flatten().collect::<Vec<_>>().join(" · ");
        let mut out = vec![];
        for key in ["image_front_url", "image_url"] {
            if let Some(u) = p[key].as_str().filter(|u| !u.is_empty()) {
                if out.iter().any(|c: &Candidate| c.url == u) {
                    continue;
                }
                out.push(Candidate {
                    url: u.to_string(),
                    title: title.clone(),
                    page_url: Some(format!("https://world.openfoodfacts.org/product/{code}")),
                    width: None,
                    height: None,
                    barcode_match: same_code,
                    provider: "open_food_facts",
                });
            }
        }
        Ok(out)
    }
}

/// Provider name of the default method (see `BingThumbnail`).
pub const BING_THUMBNAIL: &str = "bing_thumbnail";

/// The default method: barcode + product name as the query of Bing's image
/// thumbnail address. Nothing is searched or parsed here: the address itself
/// is the candidate, e.g. "6767647641365 10 Colour Flame Candles" becomes
/// `https://tse1.mm.bing.net/th?q=6767647641365+10+Colour+Flame+Candles`, and
/// Bing serves the picture it associates with that search when it is
/// fetched (through the same SSRF-guarded fetcher and image checks as every
/// other source). Unofficial: Bing may change or limit it, so a failed
/// download is transient and the other sources are still asked.
pub struct BingThumbnail {
    pub base: String,
}

impl BingThumbnail {
    pub fn new(base: &str) -> Self {
        Self { base: base.trim_end_matches('/').to_string() }
    }
}

/// `<base>/th?q=<query>`, the query form-encoded (spaces become `+`).
pub fn bing_thumbnail_url(base: &str, query: &str) -> String {
    let words = query.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut u = reqwest::Url::parse(&format!("{}/th", base.trim_end_matches('/'))).expect("thumbnail base is a URL");
    u.query_pairs_mut().append_pair("q", &words);
    u.to_string()
}

#[async_trait]
impl ImageSearchProvider for BingThumbnail {
    fn name(&self) -> &'static str {
        BING_THUMBNAIL
    }
    fn decisive(&self) -> bool {
        true
    }
    async fn search(&self, q: &SearchQuery) -> Result<Vec<Candidate>, SearchError> {
        if q.text.trim().is_empty() {
            return Ok(vec![]);
        }
        Ok(vec![Candidate {
            url: bing_thumbnail_url(&self.base, &q.text),
            // The picture answers exactly this query (barcode + name).
            title: q.text.clone(),
            page_url: None,
            width: None,
            height: None,
            barcode_match: false,
            provider: BING_THUMBNAIL,
        }])
    }
}

/// Bing image results, no key: the public results fragment
/// (`/images/async`) that the `bing-image-urls` package reads. Each result
/// carries an HTML-escaped JSON `m` attribute with `murl` (full image),
/// `purl` (page) and `t` (title). Filtered to large photographs, white,
/// in the configured market. Unofficial: the page may change or be limited,
/// so errors are transient and other sources still run.
pub struct BingImages {
    pub base: String,
    http: reqwest::Client,
}

impl BingImages {
    pub fn new(base: &str) -> Self {
        Self { base: base.trim_end_matches('/').to_string(), http: api_client() }
    }
}

fn html_unescape(s: &str) -> String {
    s.replace("&quot;", "\"").replace("&#39;", "'").replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

/// An absolute http(s) URL, or None (data:, javascript:, relative, junk).
fn web_url(u: &str) -> Option<String> {
    let url = reqwest::Url::parse(u.trim()).ok()?;
    (matches!(url.scheme(), "http" | "https") && url.host_str().is_some()).then(|| url.to_string())
}

/// Parse the results fragment: every `m="{...}"` attribute with a `murl`.
///
/// A page with result markup from which nothing can be read is an error (the
/// unofficial format probably changed), never "no results": that keeps a
/// parser break from marking every product `not_found`. Entries without a
/// usable http(s) image address are skipped, duplicates are dropped, and a
/// page without any result markup is a clean "no results".
pub fn parse_bing(html: &str, barcode: Option<&str>) -> Result<Vec<Candidate>, SearchError> {
    let mut out: Vec<Candidate> = vec![];
    let mut markers = 0usize;
    for part in html.split(" m=\"").skip(1) {
        markers += 1;
        let Some(end) = part.find('"') else { continue };
        let Ok(v) = serde_json::from_str::<Value>(&html_unescape(&part[..end])) else { continue };
        let Some(murl) = v.get("murl").and_then(|u| u.as_str()).and_then(web_url) else { continue };
        if out.iter().any(|c| c.url == murl) {
            continue;
        }
        let title = v.get("t").and_then(|t| t.as_str()).unwrap_or("");
        let desc = v.get("desc").and_then(|t| t.as_str()).unwrap_or("");
        let page = v.get("purl").and_then(|t| t.as_str()).and_then(web_url);
        let text = format!("{title} {desc} {murl}");
        out.push(Candidate {
            barcode_match: barcode.is_some_and(|b| text.contains(b) || page.as_deref().is_some_and(|p| p.contains(b))),
            title: text,
            page_url: page,
            width: None,
            height: None,
            url: murl,
            provider: "bing_images",
        });
        if out.len() >= 30 {
            break;
        }
    }
    let result_markup = markers > 0 || html.contains("iusc") || html.contains("imgpt");
    if out.is_empty() && result_markup {
        return Err(SearchError::transient("Bing results could not be read (the page format may have changed)"));
    }
    Ok(out)
}

#[async_trait]
impl ImageSearchProvider for BingImages {
    fn name(&self) -> &'static str {
        "bing_images"
    }
    async fn search(&self, q: &SearchQuery) -> Result<Vec<Candidate>, SearchError> {
        let cc = q.region.to_ascii_uppercase();
        let mkt = format!("{}-{cc}", q.language);
        let params = [
            ("q", q.text.as_str()),
            ("first", "0"),
            ("count", "35"),
            ("adlt", "strict"),
            ("qft", "+filterui:photo-photo+filterui:imagesize-large+filterui:color2-FGcls_WHITE"),
            ("cc", cc.as_str()),
            ("setmkt", mkt.as_str()),
            ("setlang", q.language.as_str()),
        ];
        let resp = self
            .http
            .get(format!("{}/images/async", self.base))
            .query(&params)
            .header(reqwest::header::ACCEPT_LANGUAGE, format!("{mkt},{};q=0.8,ar;q=0.5", q.language))
            .send()
            .await
            .map_err(|e| SearchError::transient(net(&e)))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(SearchError::transient(format!("Bing images answered {status}")));
        }
        let body = resp.text().await.map_err(|_| SearchError::transient("Bing images sent an unreadable reply"))?;
        parse_bing(&body, q.barcode.as_deref())
    }
}

pub struct GoogleImages {
    pub base: String,
    key: String,
    cx: String,
    http: reqwest::Client,
}

impl GoogleImages {
    pub fn new(base: &str, key: String, cx: String) -> Self {
        Self { base: base.trim_end_matches('/').to_string(), key, cx, http: api_client() }
    }
}

#[async_trait]
impl ImageSearchProvider for GoogleImages {
    fn name(&self) -> &'static str {
        "google_images"
    }
    async fn search(&self, q: &SearchQuery) -> Result<Vec<Candidate>, SearchError> {
        // `gl` boosts results for the market; no `cr` (that would restrict
        // results to documents from that country and hide an exact match
        // published elsewhere).
        let region = q.region.to_ascii_lowercase();
        let params = [
            ("key", self.key.as_str()),
            ("cx", self.cx.as_str()),
            ("q", q.text.as_str()),
            ("searchType", "image"),
            ("num", "10"),
            ("imgSize", "large"),
            ("imgType", "photo"),
            ("imgDominantColor", "white"),
            ("safe", "active"),
            ("gl", region.as_str()),
            ("hl", q.language.as_str()),
        ];
        let resp = self
            .http
            .get(format!("{}/customsearch/v1", self.base))
            .query(&params)
            .send()
            .await
            .map_err(|e| SearchError::transient(net(&e)))?;
        let status = resp.status();
        if !status.is_success() {
            // 429 / 403 quota and 5xx are retried; the key never appears in messages.
            return Err(SearchError::transient(format!("web image search answered {status}")));
        }
        let v: Value = resp.json().await.map_err(|_| SearchError::transient("web image search sent an unreadable reply"))?;
        let items = v.get("items").and_then(|i| i.as_array()).cloned().unwrap_or_default();
        Ok(items
            .iter()
            .filter_map(|it| {
                let url = it.get("link")?.as_str()?.to_string();
                let img = it.get("image");
                let text = format!(
                    "{} {} {}",
                    it.get("title").and_then(|t| t.as_str()).unwrap_or(""),
                    it.get("snippet").and_then(|t| t.as_str()).unwrap_or(""),
                    url
                );
                Some(Candidate {
                    barcode_match: q.barcode.as_deref().is_some_and(|b| text.contains(b)),
                    title: text,
                    page_url: img.and_then(|i| i.get("contextLink")).and_then(|x| x.as_str()).map(String::from),
                    width: img.and_then(|i| i.get("width")).and_then(|x| x.as_u64()).map(|x| x as u32),
                    height: img.and_then(|i| i.get("height")).and_then(|x| x.as_u64()).map(|x| x as u32),
                    url,
                    provider: "google_images",
                })
            })
            .collect())
    }
}

/// Client for the search sources (fixed, configured hosts). Redirects are
/// followed only to https on the same host, at most 3, so a source can never
/// bounce a request somewhere else. The system proxy, if any, is used.
fn api_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::custom(|a| {
            let same = a.previous().first().and_then(|u| u.host_str()) == a.url().host_str();
            if a.previous().len() > 3 || !same || a.url().scheme() != "https" {
                a.stop()
            } else {
                a.follow()
            }
        }))
        .timeout(Duration::from_secs(15))
        .connect_timeout(Duration::from_secs(6))
        .user_agent("AMWAPOS/0.1 (product image lookup)")
        .build()
        .unwrap_or_default()
}

/// A network error without the request URL (which may carry an API key).
fn net(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "the request timed out".into()
    } else if e.is_connect() {
        "could not connect".into()
    } else {
        "network error".into()
    }
}

// ---------------------------------------------------------------- scoring

const REJECT_WORDS: &[&str] =
    &["logo", "banner", "icon", "vector", "clipart", "clip art", "wallpaper", "illustration", "cartoon", "recipe", "poster", "advert"];
const GCC_TLDS: &[&str] = &[".bh", ".ae", ".sa", ".kw", ".qa", ".om"];

fn tokens(s: &str) -> Vec<String> {
    s.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|t| t.chars().count() >= 3).map(String::from).collect()
}

/// Ranking weights, in strict priority: identity first (barcode, then name /
/// brand words), then picture quality (size, white packshot background, at
/// most 3 points together), then the GCC market (a small tie-breaker). With
/// these weights a better name match always beats a better picture or a
/// regional site, and an exact barcode beats everything.
pub const W_BARCODE: f32 = 100.0;
pub const W_NAME: f32 = 40.0;
pub const W_GCC: f32 = 0.3;

/// Text relevance of a hit before download, or None to skip it. Without the
/// exact barcode, at least half of the product-name words must appear.
pub fn text_score(c: &Candidate, q: &SearchQuery) -> Option<f32> {
    let hay = format!("{} {}", c.title, c.page_url.as_deref().unwrap_or("")).to_lowercase();
    if REJECT_WORDS.iter().any(|w| hay.contains(w)) {
        return None;
    }
    if let (Some(w), Some(h)) = (c.width, c.height) {
        if w < 200 || h < 200 {
            return None;
        }
    }
    let name = tokens(&q.name);
    let overlap = if name.is_empty() { 0.0 } else { name.iter().filter(|t| hay.contains(t.as_str())).count() as f32 / name.len() as f32 };
    if !c.barcode_match && overlap < 0.5 {
        return None;
    }
    let mut s = if c.barcode_match { W_BARCODE } else { 0.0 } + W_NAME * overlap;
    if let Some(page) = &c.page_url {
        if let Ok(u) = reqwest::Url::parse(page) {
            if u.host_str().is_some_and(|h| GCC_TLDS.iter().any(|t| h.ends_with(t))) {
                s += W_GCC;
            }
        }
    }
    Some(s)
}

/// Final score after the image was downloaded and decoded, or None when it
/// is not a usable product picture. Web hits must look like a packshot (a
/// mostly white frame); an exact-barcode source is trusted on identity.
pub fn image_score(c: &Candidate, text: f32, n: &Normalized) -> Option<f32> {
    let side = n.source_width.min(n.source_height);
    if side < 200 {
        return None;
    }
    let aspect = n.source_width.max(n.source_height) as f32 / side as f32;
    if aspect > 3.0 {
        return None; // banners and strips
    }
    if !c.barcode_match && c.provider != BING_THUMBNAIL && n.white_border < 0.45 {
        return None;
    }
    Some(text + 2.0 * n.white_border + (side.min(1000) as f32 / 1000.0))
}

// ---------------------------------------------------------------- fetching

#[derive(Debug, Clone)]
pub struct FetchError {
    pub transient: bool,
    pub message: String,
}

fn fe(transient: bool, m: impl Into<String>) -> FetchError {
    FetchError { transient, message: m.into() }
}

/// Downloads an untrusted image URL safely: http(s) only on the standard
/// ports, no credentials in the URL, every address the name resolves to must
/// be public (no loopback, private, link-local, metadata, CGNAT, multicast,
/// unique-local or mapped-private addresses), the connection is pinned to the
/// checked address, redirects are followed by hand (at most 3, each checked
/// again), with timeouts, an image content type and a hard size cap.
///
/// Proxies: image downloads never use the system proxy (`HTTP(S)_PROXY`),
/// because behind a proxy the proxy resolves the host name again and the
/// address check above would no longer bind the real destination. Where the
/// network allows only proxied traffic, an administrator can name a proxy
/// they trust with `AMWAPOS_IMAGE_FETCH_PROXY`; destination filtering then
/// also depends on that proxy (the local check still runs first).
pub struct SafeFetcher {
    allow_private: bool,
    max_bytes: usize,
    proxy: ProxyPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyPolicy {
    /// Connect straight to the checked address (default).
    Direct,
    /// Send downloads through this explicitly trusted proxy.
    Trusted(String),
}

impl ProxyPolicy {
    /// `AMWAPOS_IMAGE_FETCH_PROXY=<url>` → trusted proxy; otherwise direct.
    pub fn from_environment() -> Self {
        match std::env::var("AMWAPOS_IMAGE_FETCH_PROXY") {
            Ok(u) if !u.trim().is_empty() => ProxyPolicy::Trusted(u.trim().to_string()),
            _ => ProxyPolicy::Direct,
        }
    }
}

impl Default for SafeFetcher {
    fn default() -> Self {
        let proxy = ProxyPolicy::from_environment();
        if proxy == ProxyPolicy::Direct && system_proxy_configured() {
            static ONCE: std::sync::Once = std::sync::Once::new();
            ONCE.call_once(|| {
                tracing::warn!(
                    "a system proxy is configured but product image downloads connect directly; \
                     set AMWAPOS_IMAGE_FETCH_PROXY to a trusted proxy if direct connections are blocked"
                )
            });
        }
        Self { allow_private: false, max_bytes: MAX_SOURCE_BYTES, proxy }
    }
}

fn system_proxy_configured() -> bool {
    ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"]
        .iter()
        .any(|k| std::env::var(k).is_ok_and(|v| !v.trim().is_empty()))
}

pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public_v4(v4);
            }
            let s = v6.segments();
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00 // unique local fc00::/7
                || (s[0] & 0xffc0) == 0xfe80 // link local fe80::/10
                || (s[0] == 0x2001 && s[1] == 0x0db8) // documentation
                || (s[0] == 0x0064 && s[1] == 0xff9b)) // NAT64 (may reach private v4)
        }
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    !(ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local() // 169.254/16, incl. 169.254.169.254 metadata
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        || o[0] == 0
        || (o[0] == 100 && (o[1] & 0xc0) == 64) // CGNAT 100.64/10
        || (o[0] == 192 && o[1] == 0 && o[2] == 0) // IETF 192.0.0/24
        || (o[0] == 198 && (o[1] & 0xfe) == 18) // benchmarking 198.18/15
        || o[0] >= 240) // reserved
}

impl SafeFetcher {
    /// Tests only: allow a local fixture server.
    pub fn allowing_private_for_tests() -> Self {
        Self { allow_private: true, max_bytes: MAX_SOURCE_BYTES, proxy: ProxyPolicy::Direct }
    }

    pub fn with_proxy(mut self, proxy: ProxyPolicy) -> Self {
        self.proxy = proxy;
        self
    }

    pub fn proxy_policy(&self) -> &ProxyPolicy {
        &self.proxy
    }

    /// Check a URL and resolve it to one allowed address.
    pub async fn check(&self, raw: &str) -> Result<(reqwest::Url, SocketAddr), FetchError> {
        let url = reqwest::Url::parse(raw).map_err(|_| fe(false, "not a valid address"))?;
        if url.scheme() != "https" && url.scheme() != "http" {
            return Err(fe(false, "only http and https addresses are allowed"));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(fe(false, "addresses with credentials are not allowed"));
        }
        let host = url.host_str().ok_or_else(|| fe(false, "the address has no host"))?.to_string();
        let port = url.port_or_known_default().ok_or_else(|| fe(false, "no port"))?;
        if !self.allow_private && url.port().is_some_and(|p| p != 80 && p != 443) {
            return Err(fe(false, "only the standard web ports are allowed"));
        }
        let lower = host.to_ascii_lowercase();
        if !self.allow_private
            && (lower == "localhost" || lower.ends_with(".localhost") || lower.ends_with(".local") || lower.ends_with(".internal"))
        {
            return Err(fe(false, "internal host names are not allowed"));
        }
        let bare = lower.trim_start_matches('[').trim_end_matches(']').to_string();
        let addrs: Vec<SocketAddr> = match bare.parse::<IpAddr>() {
            Ok(ip) => vec![SocketAddr::new(ip, port)],
            Err(_) => tokio::time::timeout(Duration::from_secs(5), tokio::net::lookup_host((bare.as_str(), port)))
                .await
                .map_err(|_| fe(true, "name lookup timed out"))?
                .map_err(|_| fe(true, "name lookup failed"))?
                .collect(),
        };
        if addrs.is_empty() {
            return Err(fe(true, "the name did not resolve"));
        }
        if !self.allow_private && addrs.iter().any(|a| !is_public_ip(a.ip())) {
            return Err(fe(false, "the address points to a private or internal network"));
        }
        Ok((url, addrs[0]))
    }

    pub async fn fetch(&self, raw: &str) -> Result<Vec<u8>, FetchError> {
        let mut next = raw.to_string();
        for _ in 0..=3 {
            let (url, addr) = self.check(&next).await?;
            let host = url.host_str().unwrap_or_default().to_string();
            let mut b = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(15))
                .connect_timeout(Duration::from_secs(6))
                .user_agent("AMWAPOS/0.1 (product image lookup)");
            match &self.proxy {
                ProxyPolicy::Direct => {
                    // Never the system proxy; pinned to the address checked above,
                    // so a second DNS answer (rebinding) cannot change the target.
                    b = b.no_proxy();
                    if host.parse::<IpAddr>().is_err() {
                        b = b.resolve(&host, addr);
                    }
                }
                ProxyPolicy::Trusted(p) => {
                    let proxy = reqwest::Proxy::all(p).map_err(|_| fe(false, "the configured image proxy is not a valid address"))?;
                    b = b.proxy(proxy);
                }
            }
            let client = b.build().map_err(|_| fe(true, "could not prepare the download"))?;
            let resp = client.get(url.clone()).header(reqwest::header::ACCEPT, "image/*").send().await.map_err(|e| fe(true, net(&e)))?;
            let status = resp.status();
            if status.is_redirection() {
                let loc = resp
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| fe(false, "a redirect without a destination"))?;
                next = url.join(loc).map_err(|_| fe(false, "a bad redirect"))?.to_string();
                continue;
            }
            if status.as_u16() == 429 || status.is_server_error() {
                return Err(fe(true, format!("the image server answered {status}")));
            }
            if !status.is_success() {
                return Err(fe(false, format!("the image server answered {status}")));
            }
            let ctype = resp.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("").to_ascii_lowercase();
            if !ctype.starts_with("image/") {
                return Err(fe(false, "the address is not an image"));
            }
            if resp.content_length().is_some_and(|n| n as usize > self.max_bytes) {
                return Err(fe(false, "the image is too large"));
            }
            let mut resp = resp;
            let mut body: Vec<u8> = Vec::new();
            while let Some(chunk) = resp.chunk().await.map_err(|e| fe(true, net(&e)))? {
                if body.len() + chunk.len() > self.max_bytes {
                    return Err(fe(false, "the image is too large"));
                }
                body.extend_from_slice(&chunk);
            }
            return Ok(body);
        }
        Err(fe(false, "too many redirects"))
    }
}

// ---------------------------------------------------------------- lookup

/// One source's search, isolated: its own time budget, and a panic inside
/// the source becomes that source's error instead of stopping the worker.
async fn search_isolated(p: Arc<dyn ImageSearchProvider>, q: SearchQuery) -> Result<Vec<Candidate>, SearchError> {
    let name = p.name();
    let task = tokio::spawn(async move { p.search(&q).await });
    let abort = task.abort_handle();
    match tokio::time::timeout(PROVIDER_TIMEOUT, task).await {
        Ok(Ok(r)) => r,
        Ok(Err(_)) => Err(SearchError::transient(format!("{name} stopped unexpectedly"))),
        Err(_) => {
            abort.abort();
            Err(SearchError::transient(format!("{name} took too long")))
        }
    }
}

/// Ask the sources in priority order, download and check the best candidates
/// of each, keep the best accepted picture. Stops early once a picture tied
/// to the exact barcode is accepted. Only a confident answer from every
/// source ends in `NotFound`; a source or download failure means `Transient`
/// (retried, bounded) so an outage never marks a product as having no picture.
pub async fn lookup(providers: &[Arc<dyn ImageSearchProvider>], fetcher: &SafeFetcher, q: &SearchQuery) -> AutoOutcome {
    let mut errors: Vec<String> = vec![];
    let mut transient = false;
    let mut best: Option<(f32, Normalized, Candidate)> = None;
    let (mut rejected, mut downloads) = (0usize, 0usize);
    let mut asked: Vec<&'static str> = vec![];
    for p in providers {
        let name = p.name();
        asked.push(name);
        tracing::debug!(provider = name, query = %q.text, "image source asked");
        let hits = match search_isolated(p.clone(), q.clone()).await {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!(provider = name, transient = e.transient, error = %e.message, "image source failed");
                transient |= e.transient;
                errors.push(format!("{name}: {}", e.message));
                continue;
            }
        };
        let mut scored: Vec<(f32, Candidate)> = hits.into_iter().filter_map(|c| text_score(&c, q).map(|s| (s, c))).collect();
        if scored.is_empty() {
            tracing::debug!(provider = name, "image source: no usable candidate");
            continue;
        }
        scored.sort_by(|a, b| b.0.total_cmp(&a.0));
        for (ts, c) in scored.into_iter().take(MAX_DOWNLOADS_PER_SOURCE) {
            if downloads >= MAX_DOWNLOADS {
                break;
            }
            downloads += 1;
            let bytes = match fetcher.fetch(&c.url).await {
                Ok(b) => b,
                Err(e) => {
                    rejected += 1;
                    transient |= e.transient;
                    tracing::debug!(provider = c.provider, transient = e.transient, error = %e.message, "candidate rejected: download");
                    continue;
                }
            };
            let n = match tokio::task::spawn_blocking(move || normalize(&bytes)).await {
                Ok(Ok(n)) => n,
                _ => {
                    rejected += 1;
                    tracing::debug!(provider = c.provider, "candidate rejected: not a readable image");
                    continue;
                }
            };
            match image_score(&c, ts, &n) {
                Some(s) if best.as_ref().is_none_or(|b| s > b.0) => best = Some((s, n, c)),
                Some(_) => {}
                None => {
                    rejected += 1;
                    tracing::debug!(provider = c.provider, white = n.white_border, "candidate rejected: not a product picture");
                }
            }
        }
        if best.as_ref().is_some_and(|b| b.2.barcode_match) {
            break; // exact identity found: lower-priority sources are not asked
        }
        if p.decisive() && best.as_ref().is_some_and(|b| b.2.provider == name) {
            break; // the default method answered: use its picture
        }
    }
    match best {
        Some((score, image, c)) => {
            tracing::info!(provider = c.provider, score, barcode_match = c.barcode_match, "image candidate selected");
            AutoOutcome::Found {
                note: json!({
                    "provider": c.provider, "source_url": c.url, "source_page": c.page_url, "barcode_match": c.barcode_match,
                    "score": (score * 100.0).round() / 100.0, "white_background": (image.white_border * 100.0).round() / 100.0,
                    "query": q.text, "sources_asked": asked, "found_at": amwapos_core::time::now_str(),
                }),
                image,
            }
        }
        None if transient => {
            AutoOutcome::Transient { note: json!({ "errors": errors, "rejected": rejected, "query": q.text, "sources_asked": asked }) }
        }
        None => AutoOutcome::NotFound {
            note: json!({ "reason": "no confident match", "rejected": rejected, "errors": errors, "query": q.text, "sources_asked": asked }),
        },
    }
}

/// Which sources can run with the current settings (none: the worker idles
/// without claiming anything, so nothing is marked failed for lack of setup).
pub fn providers_for(cfg: &ImageSearchSettings, google_key: Option<String>) -> Vec<Arc<dyn ImageSearchProvider>> {
    if amwapos_core::product_images::search_disabled_by_environment() {
        return vec![];
    }
    configured_providers(cfg, google_key)
}

/// The sources the settings switch on, in the order they are asked.
pub fn configured_providers(cfg: &ImageSearchSettings, google_key: Option<String>) -> Vec<Arc<dyn ImageSearchProvider>> {
    let mut v: Vec<Arc<dyn ImageSearchProvider>> = vec![];
    let off_base = std::env::var("AMWAPOS_OFF_BASE").unwrap_or_else(|_| "https://world.openfoodfacts.org".into());
    let google_base = std::env::var("AMWAPOS_GOOGLE_SEARCH_BASE").unwrap_or_else(|_| "https://www.googleapis.com".into());
    if !cfg.enabled {
        return v;
    }
    if cfg.bing_thumbnail {
        let base = std::env::var("AMWAPOS_BING_THUMB_BASE").unwrap_or_else(|_| "https://tse1.mm.bing.net".into());
        v.push(Arc::new(BingThumbnail::new(&base)));
    }
    if cfg.open_food_facts {
        v.push(Arc::new(OpenFoodFacts::new(&off_base)));
    }
    if cfg.bing {
        let bing_base = std::env::var("AMWAPOS_BING_BASE").unwrap_or_else(|_| "https://www.bing.com".into());
        v.push(Arc::new(BingImages::new(&bing_base)));
    }
    if cfg.google && !cfg.google_cx.is_empty() {
        if let Some(k) = google_key {
            v.push(Arc::new(GoogleImages::new(&google_base, k, cfg.google_cx.clone())));
        }
    }
    v
}

pub struct ImageWorker {
    core: Arc<AppCore>,
    task: Mutex<Option<JoinHandle<()>>>,
    pub poke: Arc<Notify>,
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> AppResult<T> + Send + 'static) -> AppResult<T> {
    tokio::task::spawn_blocking(f).await.map_err(|e| AppError::internal(format!("worker failed: {e}")))?
}

fn may_run(core: &AppCore) -> bool {
    // Terminals receive products (and their stored images) from the hub and
    // never search themselves.
    core.device().is_some_and(|d| d.mode != "terminal") && core.auto_images_active()
}

impl ImageWorker {
    pub fn new(core: Arc<AppCore>) -> Arc<Self> {
        Arc::new(Self { core, task: Mutex::new(None), poke: Arc::new(Notify::new()) })
    }

    /// Start the worker when discovery can run. Idempotent; restarts a dead task.
    pub fn ensure(self: &Arc<Self>) {
        let mut g = self.task.lock().unwrap();
        let alive = g.as_ref().map(|h| !h.is_finished()).unwrap_or(false);
        if may_run(&self.core) && !alive {
            *g = Some(tokio::spawn(run(self.clone())));
        }
    }

    pub fn running(&self) -> bool {
        self.task.lock().unwrap().as_ref().map(|h| !h.is_finished()).unwrap_or(false)
    }
}

/// One claimed product, start to finish. Returns false when nothing was due.
pub async fn process_one(core: &Arc<AppCore>, providers: &[Arc<dyn ImageSearchProvider>], fetcher: &SafeFetcher) -> AppResult<bool> {
    let (c, cfg) = (core.clone(), core.image_search_config()?.0);
    let Some(job) = blocking(move || c.auto_image_claim()).await? else { return Ok(false) };
    let q = build_query(&job, &cfg);
    tracing::info!(product_id = %job.product_id, attempt = job.attempt, sources = providers.len(), "product image lookup started");
    let outcome = match tokio::time::timeout(JOB_TIMEOUT, lookup(providers, fetcher, &q)).await {
        Ok(o) => o,
        Err(_) => AutoOutcome::Transient { note: json!({ "errors": ["the lookup took too long"], "query": q.text }) },
    };
    let (c, pid) = (core.clone(), job.product_id.clone());
    let state = blocking(move || c.auto_image_complete(&pid, outcome)).await?;
    tracing::info!(product_id = %job.product_id, attempt = job.attempt, state = %state, "product image lookup finished");
    Ok(true)
}

async fn run(w: Arc<ImageWorker>) {
    let fetcher = SafeFetcher::default();
    loop {
        let c = w.core.clone();
        if !blocking(move || Ok(may_run(&c))).await.unwrap_or(false) {
            return;
        }
        let c = w.core.clone();
        let providers = match blocking(move || c.image_search_config()).await {
            Ok((cfg, key)) => providers_for(&cfg, key),
            Err(_) => vec![],
        };
        let worked = if providers.is_empty() {
            false
        } else {
            match process_one(&w.core, &providers, &fetcher).await {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!(error = %e.message, "product image worker");
                    false
                }
            }
        };
        let pause = if worked { BETWEEN_JOBS } else { IDLE };
        tokio::select! {
            _ = tokio::time::sleep(pause) => {}
            _ = w.poke.notified() => { tokio::time::sleep(Duration::from_millis(300)).await; }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_and_internal_addresses_are_not_public() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.9",
            "192.168.1.1",
            "169.254.169.254",
            "0.0.0.0",
            "100.64.0.1",
            "224.0.0.1",
            "255.255.255.255",
            "::1",
            "fc00::1",
            "fd12::1",
            "fe80::1",
            "::ffff:192.168.0.1",
            "::ffff:127.0.0.1",
            "64:ff9b::a00:1",
        ] {
            assert!(!is_public_ip(ip.parse().unwrap()), "{ip} must be refused");
        }
        for ip in ["8.8.8.8", "1.1.1.1", "2a00:1450:4001::200e", "151.101.1.140"] {
            assert!(is_public_ip(ip.parse().unwrap()), "{ip} is public");
        }
    }

    #[tokio::test]
    async fn the_fetcher_refuses_unsafe_urls_before_connecting() {
        let f = SafeFetcher::default();
        for u in [
            "http://127.0.0.1/a.png",
            "http://localhost/a.png",
            "http://169.254.169.254/latest/meta-data/",
            "http://[::1]/a.png",
            "http://10.0.0.5/a.png",
            "http://192.168.1.10:8080/a.png",
            "file:///etc/passwd",
            "ftp://example.com/a.png",
            "gopher://example.com/",
            "https://user:pw@example.com/a.png",
            "http://metadata.google.internal/",
            "http://printer.local/a.png",
            "http://example.com:22/a.png",
            "not a url",
        ] {
            let e = f.check(u).await.unwrap_err();
            assert!(!e.transient, "{u}: {}", e.message);
        }
    }

    fn cand(title: &str, barcode_match: bool, w: Option<u32>) -> Candidate {
        Candidate {
            url: "https://cdn.example.com/p.jpg".into(),
            title: title.into(),
            page_url: Some("https://shop.example.bh/p".into()),
            width: w,
            height: w,
            barcode_match,
            provider: "test",
        }
    }

    fn q() -> SearchQuery {
        SearchQuery {
            barcode: Some("6281007031126".into()),
            name: "Almarai Fresh Milk 1L".into(),
            name_ar: None,
            category: None,
            text: "6281007031126 Almarai Fresh Milk 1L".into(),
            region: "bh".into(),
            language: "en".into(),
        }
    }

    #[test]
    fn the_default_method_puts_barcode_and_name_in_the_bing_thumbnail_address() {
        assert_eq!(
            bing_thumbnail_url("https://tse1.mm.bing.net", "6767647641365 10 Colour Flame Candles"),
            "https://tse1.mm.bing.net/th?q=6767647641365+10+Colour+Flame+Candles"
        );
        // Extra spaces collapse; symbols and Arabic are encoded, never break the address.
        assert_eq!(
            bing_thumbnail_url("https://tse1.mm.bing.net/", "  6281007031126   Milk & Co 1L  "),
            "https://tse1.mm.bing.net/th?q=6281007031126+Milk+%26+Co+1L"
        );
        assert!(bing_thumbnail_url("https://tse1.mm.bing.net", "حليب").starts_with("https://tse1.mm.bing.net/th?q=%D8%AD"));
        let cfg = ImageSearchSettings::default();
        assert!(cfg.bing_thumbnail, "the default method is on by default");
        let names: Vec<_> = configured_providers(&cfg, None).iter().map(|p| p.name()).collect();
        assert_eq!(names.first(), Some(&BING_THUMBNAIL), "and asked first: {names:?}");
    }

    /// One Bing result entry as the page encodes it (HTML-escaped JSON in `m`).
    fn bing_entry(json: &str) -> String {
        let esc = json.replace('&', "&amp;").replace('"', "&quot;");
        format!(r##"<div class="imgpt"><a class="iusc" m="{esc}" href="#">x</a></div>"##)
    }

    #[test]
    fn bing_fixtures_valid_duplicate_logo_missing_metadata_malformed_and_empty() {
        let page = [
            // Valid product result, barcode on the page address.
            bing_entry(r#"{"purl":"https://shop.example.bh/p/6281007031126","murl":"https://cdn.example.com/milk.jpg?a=1&b=2","t":"Almarai Fresh Milk 1L"}"#),
            // Duplicate image address.
            bing_entry(r#"{"murl":"https://cdn.example.com/milk.jpg?a=1&b=2","t":"dup"}"#),
            // Logo result.
            bing_entry(r#"{"purl":"https://blog.example.com","murl":"https://img.example.com/logo.png","t":"Almarai logo"}"#),
            // Missing metadata: only the image address.
            bing_entry(r#"{"murl":"https://img.example.com/plain.jpg"}"#),
            // Malformed entries: broken JSON, no murl, a data: URL, a relative path.
            r#"<a class="iusc" m="{broken json}">z</a>"#.to_string(),
            bing_entry(r#"{"t":"no image address"}"#),
            bing_entry(r#"{"murl":"data:image/png;base64,AAAA"}"#),
            bing_entry(r#"{"murl":"/relative.jpg","purl":"javascript:alert(1)"}"#),
        ]
        .join("\n");
        let c = parse_bing(&page, Some("6281007031126")).unwrap();
        let urls: Vec<&str> = c.iter().map(|c| c.url.as_str()).collect();
        assert_eq!(
            urls,
            ["https://cdn.example.com/milk.jpg?a=1&b=2", "https://img.example.com/logo.png", "https://img.example.com/plain.jpg"],
            "duplicates, broken entries and non-web addresses are skipped"
        );
        assert!(c[0].barcode_match, "barcode found on the page address");
        assert_eq!(c[0].page_url.as_deref(), Some("https://shop.example.bh/p/6281007031126"));
        assert!(text_score(&c[1], &q()).is_none(), "logo refused");
        assert!(text_score(&c[2], &q()).is_none(), "no title, no identity: refused");
        // Zero results: a clean empty answer.
        assert!(parse_bing("<html><body>There are no results for this search</body></html>", None).unwrap().is_empty());
        assert!(parse_bing("", None).unwrap().is_empty());
        // Result markup that cannot be read (format changed): an error, never "no results".
        for broken in [
            r#"<div class="imgpt"><a class="iusc" data-m="{}">x</a></div>"#.to_string(),
            r#"<a class="iusc" m="{&quot;image&quot;:&quot;https://x.example/a.jpg&quot;}">x</a>"#.to_string(),
            r#"<a m="{not json at all">x</a>"#.to_string(),
        ] {
            let e = parse_bing(&broken, None).unwrap_err();
            assert!(e.transient && e.message.contains("could not be read"), "{broken}: {}", e.message);
        }
    }

    #[test]
    fn identity_outranks_picture_quality_and_region() {
        let q = q();
        // An exact barcode beats any name match.
        let exact = cand("unrelated title", true, Some(800));
        let name = cand("Almarai Fresh Milk 1L", false, Some(800));
        assert!(text_score(&exact, &q).unwrap() > text_score(&name, &q).unwrap());
        // A full name match on a global site beats a half match on a GCC site,
        // even when the half match has the perfect white packshot.
        let mut global_full = cand("Almarai Fresh Milk 1L", false, Some(800));
        global_full.page_url = Some("https://shop.example.co.uk/p".into());
        let gcc_half = cand("Almarai Fresh", false, Some(800));
        let (tg, th) = (text_score(&global_full, &q).unwrap(), text_score(&gcc_half, &q).unwrap());
        assert!(tg > th);
        let img = |white: f32, side: u32| Normalized {
            hash: String::new(),
            jpeg: vec![],
            width: side.min(512),
            height: side.min(512),
            source_width: side,
            source_height: side,
            white_border: white,
        };
        let full_plain = image_score(&global_full, tg, &img(0.5, 300)).unwrap();
        let half_perfect = image_score(&gcc_half, th, &img(1.0, 1000)).unwrap();
        assert!(full_plain > half_perfect, "identity first: {full_plain} vs {half_perfect}");
        // Among equal identity, the white packshot and then the region decide.
        let a = image_score(&name, text_score(&name, &q).unwrap(), &img(0.95, 800)).unwrap();
        let b = image_score(&name, text_score(&name, &q).unwrap(), &img(0.5, 800)).unwrap();
        assert!(a > b);
        // Web hits that do not look like a packshot are refused outright.
        assert!(image_score(&name, 1.0, &img(0.2, 800)).is_none(), "dark / lifestyle photo");
        assert!(image_score(&name, 1.0, &img(0.9, 150)).is_none(), "too small");
    }

    #[test]
    fn text_scoring_needs_identity_and_skips_logos_and_thumbnails() {
        assert!(text_score(&cand("Almarai Fresh Milk 1 litre", false, Some(800)), &q()).is_some());
        assert!(text_score(&cand("Almarai logo vector", false, Some(800)), &q()).is_none(), "logo");
        assert!(text_score(&cand("Something unrelated", false, Some(800)), &q()).is_none(), "no identity");
        assert!(text_score(&cand("Something unrelated", true, Some(800)), &q()).is_some(), "barcode evidence");
        assert!(text_score(&cand("Almarai Fresh Milk", false, Some(120)), &q()).is_none(), "thumbnail");
        let gcc = text_score(&cand("Almarai Fresh Milk", false, Some(800)), &q()).unwrap();
        let mut other = cand("Almarai Fresh Milk", false, Some(800));
        other.page_url = Some("https://shop.example.com/p".into());
        assert!(gcc > text_score(&other, &q()).unwrap(), "GCC market is a small boost");
        let job = AutoImageJob {
            product_id: "p".into(),
            name: "Laban".into(),
            name_ar: None,
            sku: "100001".into(),
            barcodes: vec!["12".into(), "6281007031126".into()],
            category: Some("Dairy".into()),
            attempt: 1,
        };
        assert_eq!(build_query(&job, &ImageSearchSettings::default()).text, "6281007031126 Laban");
        let job = AutoImageJob { barcodes: vec![], ..job };
        assert_eq!(build_query(&job, &ImageSearchSettings::default()).text, "Laban Dairy", "no barcode is invented");
    }
}
