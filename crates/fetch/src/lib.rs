//! The only network code in gift-check: a polite reader of public Shopify catalogs.
//!
//! Manners, all enforced here:
//! - robots.txt is read first and obeyed (RFC 9309; a 5xx robots.txt means "assume disallowed").
//!   Its redirects are followed (up to 5, same site only); a redirect that can't be followed
//!   means robots.txt is unreachable, so nothing is fetched.
//! - One honest User-Agent, `gift-check/<version> (personal gift price lookup)`.
//! - At least 3 s between requests to one host (more if robots.txt asks with crawl-delay; a
//!   crawl-delay above 60 s stops the run instead). The time of the last request and any
//!   Retry-After are kept in the database, so the next run honours them too.
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
/// robots.txt redirects followed (RFC 9309 asks for at least five).
pub const MAX_ROBOTS_REDIRECTS: usize = 5;
/// A Retry-After kept for the next run is capped at a day.
const MAX_REMEMBERED_WAIT_S: u64 = 86_400;

/// Why a run stopped early.
#[derive(Debug, Clone, PartialEq)]
pub enum Stop {
    Challenge { url: String, why: String },
    RateLimited { url: String, why: String },
    /// An earlier run was told to wait (Retry-After) until this time (Unix ms).
    WaitAsked { host: String, until_ms: u64 },
    Disallowed { url: String },
    RobotsUnreachable { url: String, why: String },
    CrawlDelay { url: String, secs: f64 },
    Status { url: String, status: u16 },
    Redirect { url: String, location: String },
    NotShopify { url: String, why: String },
    /// The page held exactly the products of the page before: the store ignores `page`.
    Repeating { url: String },
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
            Stop::WaitAsked { host, until_ms } => write!(
                f,
                "{host} asked gift-check (Retry-After) not to come back before {} UTC. Nothing was fetched; try again after that.",
                time::format_utc((until_ms / 1000) as i64).replace('T', " ").trim_end_matches('Z')
            ),
            Stop::Disallowed { url } => write!(f, "robots.txt does not allow gift-check to fetch {url}. Stopped."),
            Stop::RobotsUnreachable { url, why } => {
                write!(f, "{url} could not be read ({why}). Without it everything counts as disallowed (RFC 9309), so nothing was fetched. Try again later.")
            }
            Stop::CrawlDelay { url, secs } => write!(
                f,
                "{url} asks for {secs} s between requests, more than the {} s gift-check is willing to space them out. Stopped before fetching the catalog; nothing was fetched.",
                MAX_CRAWL_DELAY_MS / 1000
            ),
            Stop::Status { url, status } => write!(f, "{url} answered {status}. Stopped; what was fetched so far is kept."),
            Stop::Redirect { url, location } => write!(f, "{url} redirected to {location}. gift-check doesn't follow redirects for catalog pages (the store may be password-protected or on another domain)."),
            Stop::NotShopify { url, why } => write!(f, "{url} doesn't look like a Shopify catalog: {why}."),
            Stop::Repeating { url } => write!(f, "{url} returned the same products as the page before, so the store seems to ignore the page number. Stopped; what was fetched so far is kept (not a complete read)."),
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
    /// Where the per-host spacing and Retry-After are kept between runs.
    db: Option<&'a Db>,
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
        Fetcher { t, clock, db: None, last: HashMap::new(), gap: HashMap::new(), had_429: false, requests: 0, log: Vec::new() }
    }

    /// A fetcher that reads and records each host's last request and Retry-After in `db`.
    pub fn with_db(t: &'a dyn Transport, clock: &'a dyn Clock, db: &'a Db) -> Self {
        Fetcher { db: Some(db), ..Fetcher::new(t, clock) }
    }

    fn send(&mut self, url: &str) -> Result<Response, Stop> {
        let host = host_key(url);
        let saved = match self.db {
            Some(db) => db.host_state(&host).map_err(Stop::Storage)?,
            None => Default::default(),
        };
        let now = self.clock.now_ms();
        if let Some(until_ms) = saved.retry_not_before_ms.filter(|t| *t > now) {
            return Err(Stop::WaitAsked { host, until_ms });
        }
        // Until this run's robots.txt says otherwise, the spacing the last run used (never less
        // than 3 s); the last request may have been made by an earlier run or another window.
        let gap = self.gap.get(&host).copied().unwrap_or_else(|| saved.gap_ms.unwrap_or(0).clamp(MIN_GAP_MS, MAX_CRAWL_DELAY_MS));
        let last = self.last.get(&host).copied().into_iter().chain(saved.last_request_ms).max();
        if let Some(last) = last {
            if now < last + gap {
                self.clock.sleep_ms(last + gap - now);
            }
        }
        let at = self.clock.now_ms();
        self.last.insert(host.clone(), at);
        if let Some(db) = self.db {
            db.note_request(&host, at, gap).map_err(Stop::Storage)?;
        }
        self.requests += 1;
        self.t.get(url).map_err(|err| Stop::Network { url: url.to_string(), err })
    }

    /// Keeps a Retry-After for later runs (capped at a day).
    fn remember_wait(&mut self, url: &str, wait_s: u64) -> Result<(), Stop> {
        if let Some(db) = self.db {
            let until = self.clock.now_ms() + wait_s.min(MAX_REMEMBERED_WAIT_S) * 1000;
            db.note_retry_after(&host_key(url), until).map_err(Stop::Storage)?;
        }
        Ok(())
    }

    /// One polite GET: paced, stopped at a bot check, and a 429 waited out once per run.
    pub fn get(&mut self, url: &str) -> Result<Response, Stop> {
        let r = self.send(url)?;
        if let Some(why) = detect_challenge(&r) {
            return Err(Stop::Challenge { url: url.to_string(), why });
        }
        if r.status != 429 {
            return Ok(r);
        }
        let wait = retry_after_secs(r.header("retry-after"), time::now()).unwrap_or(DEFAULT_RETRY_S);
        self.remember_wait(url, wait)?;
        if self.had_429 {
            return Err(Stop::RateLimited { url: url.to_string(), why: "a second 429 Too Many Requests in this run".into() });
        }
        self.had_429 = true;
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
            let wait = retry_after_secs(r.header("retry-after"), time::now()).unwrap_or(DEFAULT_RETRY_S);
            self.remember_wait(url, wait)?;
            return Err(Stop::RateLimited { url: url.to_string(), why: "429 Too Many Requests again after waiting".into() });
        }
        Ok(r)
    }

    /// robots.txt for `base` ("https://host"). Redirects are followed, up to
    /// [`MAX_ROBOTS_REDIRECTS`] and on the same site only; a redirect that can't be followed
    /// (no or a bad Location, https to http, another site, too many) means "unreachable".
    pub fn robots(&mut self, base: &str) -> Result<Robots, Stop> {
        let first = format!("{base}/robots.txt");
        let mut url = first.clone();
        let unreachable = |url: &str, why: String| Stop::RobotsUnreachable { url: url.to_string(), why };
        for hop in 0..=MAX_ROBOTS_REDIRECTS {
            let r = self.get(&url)?;
            match r.status {
                200..=299 => return Ok(Robots::parse(&r.body, gift_check_core::UA_TOKEN)),
                300..=399 => {
                    if hop == MAX_ROBOTS_REDIRECTS {
                        return Err(unreachable(&first, format!("more than {MAX_ROBOTS_REDIRECTS} redirects")));
                    }
                    let location = r.header("location").map(str::trim).filter(|l| !l.is_empty()).ok_or_else(|| unreachable(&url, format!("a {} redirect without a Location", r.status)))?;
                    let next = resolve(&url, location).map_err(|why| unreachable(&url, why))?;
                    if !same_site(&first, &next) {
                        return Err(unreachable(&first, format!("it redirects to another site, {next}")));
                    }
                    self.log.push(format!("robots.txt: {} redirect to {next}", r.status));
                    url = next;
                }
                // "Unavailable": no rules.
                400..=499 => return Ok(Robots::allow_all()),
                s => return Err(unreachable(&url, format!("status {s}"))),
            }
        }
        Err(unreachable(&first, "too many redirects".into()))
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
            if d * 1000.0 > MAX_CRAWL_DELAY_MS as f64 {
                out.stop = Some(Stop::CrawlDelay { url: format!("{base}/robots.txt"), secs: d });
                return out;
            }
            let ms = ((d * 1000.0) as u64).max(MIN_GAP_MS);
            self.gap.insert(host_key(base), ms);
            self.log.push(format!("robots.txt asks for {d} s between requests; using {} s", ms as f64 / 1000.0));
        }
        let path = match collection {
            Some(h) => format!("/collections/{h}/products.json"),
            None => "/products.json".to_string(),
        };
        let mut previous: Option<Vec<String>> = None;
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
            let ids: Vec<String> = products.iter().map(|p| p.id.clone()).collect();
            if previous.as_ref() == Some(&ids) {
                out.stop = Some(Stop::Repeating { url });
                return out;
            }
            previous = Some(ids);
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

/// A Location header against the URL that sent it (RFC 3986 reference resolution: absolute,
/// protocol-relative, absolute-path and relative references). Only http(s), and never from https
/// down to http.
fn resolve(from: &str, location: &str) -> Result<String, String> {
    let base = url::Url::parse(from).map_err(|e| format!("can't read the URL {from}: {e}"))?;
    let next = base.join(location).map_err(|e| format!("a redirect to {location:?}, which isn't a usable URL ({e})"))?;
    match (base.scheme(), next.scheme()) {
        ("https", "http") => Err(format!("a redirect from https down to http ({next})")),
        (_, "http" | "https") => Ok(next.to_string()),
        (_, other) => Err(format!("a redirect to a {other}: address")),
    }
}

/// The same site: the same host and port, or the same host with or without a leading "www.".
fn same_site(a: &str, b: &str) -> bool {
    let key = |u: &str| {
        let u = url::Url::parse(u).ok()?;
        let host = u.host_str()?.to_ascii_lowercase();
        let host = host.strip_prefix("www.").map(str::to_string).unwrap_or(host);
        Some((host, u.port_or_known_default()))
    };
    matches!((key(a), key(b)), (Some(x), Some(y)) if x == y)
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
    let mut f = Fetcher::with_db(t, clock, db);
    let outcome = f.fetch_shopify(store, base, collection, &mut |page, products| {
        let n = db.store_products(cap, &time::now_utc(), &products, rules)?;
        progress(&format!("page {page}: {n} products saved"));
        Ok(())
    });
    // Only a whole-catalog read that reached the empty page can say what has gone.
    let scope = if outcome.complete && collection.is_none() { Scope::All } else { Scope::None };
    let note = outcome.stop.as_ref().map(ToString::to_string);
    let fin = db.finish_capture(cap, outcome.complete, &scope, note.as_deref())?;
    Ok(RunReport { capture_id: cap, store: store.id.clone(), pages: outcome.pages, products: fin.products, complete: outcome.complete, discontinued: fin.discontinued, requests: f.requests, stop: note, log: f.log })
}

#[cfg(test)]
mod tests;
