//! The only network code in gift-check: a polite reader of public Shopify catalogs.
//!
//! Manners, all enforced here:
//! - robots.txt is read first and obeyed (RFC 9309; a 5xx robots.txt means "assume disallowed").
//! - One honest User-Agent, `gift-check/<version> (personal gift price lookup)`.
//! - At least 3 s between requests to one host (more if robots.txt asks with crawl-delay).
//! - A 429 is honoured once: wait for its Retry-After (at most 120 s), try once more. A second
//!   429 (or a longer Retry-After) stops the run; what was fetched so far is kept.
//! - A bot check (Cloudflare/Akamai challenge and the like) stops the run at once with advice to
//!   use browser capture. Nothing here tries to get around one: no header spoofing, no TLS or
//!   fingerprint tricks, no headless browser, no challenge solving, no proxies.

pub mod transport;

pub use transport::{Clock, FakeClock, Http, Response, SystemClock, Transport};

use gift_check_core::db::{Db, Scope};
use gift_check_core::model::Product;
use gift_check_core::robots::Robots;
use gift_check_core::rules::Rules;
use gift_check_core::stores::StoreCfg;
use gift_check_core::{shopify, time};
use std::collections::HashMap;

pub const MIN_GAP_MS: u64 = 3_000;
pub const MAX_CRAWL_DELAY_MS: u64 = 60_000;
pub const RETRY_CAP_S: u64 = 120;
/// Used when a 429 carries no usable Retry-After.
pub const DEFAULT_RETRY_S: u64 = 60;
pub const MAX_PAGES: u32 = 200;

/// Why a run stopped early.
#[derive(Debug, Clone, PartialEq)]
pub enum Stop {
    Challenge { url: String, why: String },
    RateLimited { url: String, why: String },
    Disallowed { url: String },
    RobotsUnreachable { url: String, why: String },
    Status { url: String, status: u16 },
    Redirect { url: String, location: String },
    NotShopify { url: String, why: String },
    Network { url: String, err: String },
    Storage(String),
    PageCap,
}

impl std::fmt::Display for Stop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Stop::Challenge { url, why } => write!(
                f,
                "{url} answered with a bot check ({why}). gift-check does not try to get past bot checks. Use browser capture instead: \
                 open the store from the bridge (gift-check ui, Captures tab, Open the bridge) or import a capture file (see docs/CAPTURE.md)."
            ),
            Stop::RateLimited { url, why } => write!(f, "{url}: the store asked gift-check to slow down ({why}). Stopped; what was fetched so far is kept. Try again later."),
            Stop::Disallowed { url } => write!(f, "robots.txt does not allow gift-check to fetch {url}. Stopped."),
            Stop::RobotsUnreachable { url, why } => {
                write!(f, "{url} could not be read ({why}). Without it everything counts as disallowed (RFC 9309), so nothing was fetched. Try again later.")
            }
            Stop::Status { url, status } => write!(f, "{url} answered {status}. Stopped; what was fetched so far is kept."),
            Stop::Redirect { url, location } => write!(f, "{url} redirected to {location}. gift-check doesn't follow redirects for catalog pages (the store may be password-protected or on another domain)."),
            Stop::NotShopify { url, why } => write!(f, "{url} doesn't look like a Shopify catalog: {why}."),
            Stop::Network { url, err } => write!(f, "couldn't reach {url}: {err}"),
            Stop::Storage(e) => write!(f, "couldn't save the products: {e}"),
            Stop::PageCap => write!(f, "stopped after {MAX_PAGES} pages; the catalog may be larger"),
        }
    }
}

/// A bot check or challenge page, by its headers or its HTML. Returns what gave it away.
pub fn detect_challenge(r: &Response) -> Option<String> {
    if r.header("cf-mitigated").is_some_and(|v| v.to_ascii_lowercase().contains("challenge")) {
        return Some("cf-mitigated: challenge".into());
    }
    if r.header("x-datadome").is_some() || r.header("x-dd-b").is_some() {
        return Some("DataDome".into());
    }
    let lower = r.body.get(..r.body.len().min(64 * 1024)).unwrap_or(&r.body).to_ascii_lowercase();
    let markers: [(&str, &str); 10] = [
        ("<title>just a moment...</title>", "Cloudflare \"Just a moment...\" page"),
        ("cf-browser-verification", "Cloudflare browser verification"),
        ("/cdn-cgi/challenge-platform/", "Cloudflare challenge platform"),
        ("_cf_chl_opt", "Cloudflare challenge"),
        ("attention required! | cloudflare", "Cloudflare block page"),
        ("captcha-delivery.com", "DataDome captcha"),
        ("px-captcha", "PerimeterX captcha"),
        ("/_incapsula_resource", "Imperva/Incapsula check"),
        ("errors.edgesuite.net", "Akamai block page"),
        ("g-recaptcha", "reCAPTCHA"),
    ];
    if let Some((_, what)) = markers.iter().find(|(m, _)| lower.contains(m)) {
        return Some((*what).to_string());
    }
    if r.status == 403 {
        let server = r.header("server").unwrap_or("").to_ascii_lowercase();
        if server.contains("akamaighost") || (lower.contains("access denied") && lower.contains("reference #")) {
            return Some("Akamai 403 \"Access Denied\"".into());
        }
        if server.contains("cloudflare") && lower.contains("<html") {
            return Some("Cloudflare 403 page".into());
        }
    }
    None
}

/// Seconds to wait from a Retry-After value: a number of seconds or an HTTP date.
pub fn retry_after_secs(value: Option<&str>, now_epoch: i64) -> Option<u64> {
    let v = value?.trim();
    if let Ok(n) = v.parse::<u64>() {
        return Some(n);
    }
    time::parse_http_date(v).map(|t| (t - now_epoch).max(0) as u64)
}

/// The host of `http(s)://host[:port]/...`, for pacing.
fn host_key(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.split(['/', '?', '#']).next().unwrap_or("").to_ascii_lowercase()
}

pub struct Fetcher<'a> {
    t: &'a dyn Transport,
    clock: &'a dyn Clock,
    last: HashMap<String, u64>,
    gap: HashMap<String, u64>,
    had_429: bool,
    /// Requests sent, for the report.
    pub requests: u32,
    /// What happened, line by line.
    pub log: Vec<String>,
}

impl<'a> Fetcher<'a> {
    pub fn new(t: &'a dyn Transport, clock: &'a dyn Clock) -> Self {
        Fetcher { t, clock, last: HashMap::new(), gap: HashMap::new(), had_429: false, requests: 0, log: Vec::new() }
    }

    fn send(&mut self, url: &str) -> Result<Response, Stop> {
        let host = host_key(url);
        let gap = self.gap.get(&host).copied().unwrap_or(MIN_GAP_MS);
        if let Some(&last) = self.last.get(&host) {
            let now = self.clock.now_ms();
            if now < last + gap {
                self.clock.sleep_ms(last + gap - now);
            }
        }
        self.last.insert(host, self.clock.now_ms());
        self.requests += 1;
        self.t.get(url).map_err(|err| Stop::Network { url: url.to_string(), err })
    }

    /// One polite GET: paced, stopped at a bot check, and a 429 waited out once.
    pub fn get(&mut self, url: &str) -> Result<Response, Stop> {
        let r = self.send(url)?;
        if let Some(why) = detect_challenge(&r) {
            return Err(Stop::Challenge { url: url.to_string(), why });
        }
        if r.status != 429 {
            return Ok(r);
        }
        if self.had_429 {
            return Err(Stop::RateLimited { url: url.to_string(), why: "a second 429 Too Many Requests in this run".into() });
        }
        self.had_429 = true;
        let wait = retry_after_secs(r.header("retry-after"), time::now()).unwrap_or(DEFAULT_RETRY_S);
        if wait > RETRY_CAP_S {
            return Err(Stop::RateLimited { url: url.to_string(), why: format!("429, and Retry-After asks for {wait} s, more than the {RETRY_CAP_S} s gift-check will wait") });
        }
        self.log.push(format!("429 from {}: waiting {wait} s as asked, then trying once more", host_key(url)));
        self.clock.sleep_ms(wait * 1000);
        let r = self.send(url)?;
        if let Some(why) = detect_challenge(&r) {
            return Err(Stop::Challenge { url: url.to_string(), why });
        }
        if r.status == 429 {
            return Err(Stop::RateLimited { url: url.to_string(), why: "429 Too Many Requests again after waiting".into() });
        }
        Ok(r)
    }

    /// robots.txt for `base` ("https://host"). Redirects are followed (up to 5, as RFC 9309 asks).
    pub fn robots(&mut self, base: &str) -> Result<Robots, Stop> {
        let mut url = format!("{base}/robots.txt");
        for _ in 0..=5 {
            let r = self.get(&url)?;
            match r.status {
                200..=299 => return Ok(Robots::parse(&r.body, gift_check_core::UA_TOKEN)),
                300..=399 => {
                    let Some(next) = r.header("location").and_then(|l| resolve(&url, l)) else {
                        return Ok(Robots::allow_all());
                    };
                    url = next;
                }
                // "Unavailable": no rules.
                400..=499 => return Ok(Robots::allow_all()),
                s => return Err(Stop::RobotsUnreachable { url, why: format!("status {s}") }),
            }
        }
        Ok(Robots::allow_all())
    }

    /// Reads a Shopify catalog page by page (250 per page) until an empty page, handing each
    /// page to `sink` as it arrives so a stopped run keeps what it got.
    pub fn fetch_shopify(&mut self, store: &StoreCfg, base: &str, collection: Option<&str>, sink: &mut dyn FnMut(u32, Vec<Product>) -> Result<(), String>) -> Outcome {
        let mut out = Outcome::default();
        let robots = match self.robots(base) {
            Ok(r) => r,
            Err(s) => {
                out.stop = Some(s);
                return out;
            }
        };
        out.robots_group = robots.matched.clone();
        if let Some(d) = robots.crawl_delay() {
            let ms = ((d * 1000.0) as u64).clamp(MIN_GAP_MS, MAX_CRAWL_DELAY_MS);
            self.gap.insert(host_key(base), ms);
            self.log.push(format!("robots.txt asks for {d} s between requests; using {} s", ms / 1000));
        }
        let path = match collection {
            Some(h) => format!("/collections/{h}/products.json"),
            None => "/products.json".to_string(),
        };
        for page in 1..=MAX_PAGES {
            let pq = format!("{path}?limit=250&page={page}");
            let url = format!("{base}{pq}");
            if !robots.allowed(&pq) {
                out.stop = Some(Stop::Disallowed { url });
                return out;
            }
            let r = match self.get(&url) {
                Ok(r) => r,
                Err(s) => {
                    out.stop = Some(s);
                    return out;
                }
            };
            match r.status {
                200 => {}
                300..=399 => {
                    out.stop = Some(Stop::Redirect { url, location: r.header("location").unwrap_or("?").to_string() });
                    return out;
                }
                404 if page == 1 => {
                    out.stop = Some(Stop::NotShopify { url, why: "there is no products.json here (404)".into() });
                    return out;
                }
                status => {
                    out.stop = Some(Stop::Status { url, status });
                    return out;
                }
            }
            let products = match shopify::map_page(&r.body, store) {
                Ok(p) => p,
                Err(why) => {
                    out.stop = Some(Stop::NotShopify { url, why });
                    return out;
                }
            };
            if products.is_empty() {
                out.complete = true;
                return out;
            }
            out.pages = page;
            out.products += products.len();
            if let Err(e) = sink(page, products) {
                out.stop = Some(Stop::Storage(e));
                return out;
            }
        }
        out.stop = Some(Stop::PageCap);
        out
    }
}

/// A Location header against the URL that sent it: absolute http(s), or a path on the same host.
fn resolve(from: &str, location: &str) -> Option<String> {
    let l = location.trim();
    if l.starts_with("https://") || l.starts_with("http://") {
        // Never step down from https to http.
        return (!(from.starts_with("https://") && l.starts_with("http://"))).then(|| l.to_string());
    }
    let (scheme, rest) = from.split_once("://")?;
    let host = rest.split(['/', '?', '#']).next()?;
    l.starts_with('/').then(|| format!("{scheme}://{host}{l}"))
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Outcome {
    /// Pages that held products.
    pub pages: u32,
    pub products: usize,
    /// The empty page was reached: the whole catalog (or collection) was read.
    pub complete: bool,
    pub stop: Option<Stop>,
    pub robots_group: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RunReport {
    pub capture_id: i64,
    pub store: String,
    pub pages: u32,
    pub products: i64,
    pub complete: bool,
    pub discontinued: i64,
    pub requests: u32,
    pub stop: Option<String>,
    pub log: Vec<String>,
}

/// Fetches a Shopify store into the database as one capture. `base` is where requests go
/// ("https://<domain>"; tests pass a local server); product links always use the store's origin.
#[allow(clippy::too_many_arguments)]
pub fn run_shopify(db: &Db, rules: &Rules, store: &StoreCfg, base: &str, collection: Option<&str>, t: &dyn Transport, clock: &dyn Clock, progress: &mut dyn FnMut(&str)) -> Result<RunReport, String> {
    if let Some(h) = collection {
        if h.is_empty() || h.len() > 100 || !h.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_') {
            return Err(format!("collection handle {h:?} must be lower-case letters, digits, - and _"));
        }
    }
    db.upsert_store(store)?;
    let source = match collection {
        Some(h) => format!("shopify:{h}"),
        None => "shopify".to_string(),
    };
    let cap = db.begin_capture(&store.id, &source, None, None, &[])?;
    progress(&format!("Reading {base}/robots.txt"));
    let mut f = Fetcher::new(t, clock);
    let outcome = f.fetch_shopify(store, base, collection, &mut |page, products| {
        let n = db.store_products(cap, &time::now_utc(), &products, rules)?;
        progress(&format!("page {page}: {n} products saved"));
        Ok(())
    });
    // Only a whole-catalog read that reached the empty page can say what has gone.
    let scope = if outcome.complete && collection.is_none() { Scope::All } else { Scope::None };
    let note = outcome.stop.as_ref().map(ToString::to_string);
    let (count, gone) = db.finish_capture(cap, outcome.complete, &scope, note.as_deref())?;
    Ok(RunReport { capture_id: cap, store: store.id.clone(), pages: outcome.pages, products: count, complete: outcome.complete, discontinued: gone, requests: f.requests, stop: note, log: f.log })
}

#[cfg(test)]
mod tests;
