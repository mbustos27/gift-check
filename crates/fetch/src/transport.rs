//! HTTP behind a trait (a real client, and whatever the tests stand up), and the clock, so the
//! pacing and waiting can be tested without waiting.

use std::cell::{Cell, RefCell};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

pub trait Transport {
    /// `Err` means no HTTP answer at all (DNS, TLS, timeout). Any status code is `Ok`.
    fn get(&self, url: &str) -> Result<Response, String>;
}

pub trait Clock {
    /// Unix milliseconds.
    fn now_ms(&self) -> u64;
    fn sleep_ms(&self, ms: u64);
}

/// Wall-clock time, so the spacing between requests to a host holds across runs too.
#[derive(Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
    }
    fn sleep_ms(&self, ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }
}

/// A clock that only moves when told to sleep, and remembers each sleep.
#[derive(Default)]
pub struct FakeClock {
    now: Cell<u64>,
    pub sleeps: RefCell<Vec<u64>>,
}

impl FakeClock {
    /// A fake clock that starts at `ms`.
    pub fn at(ms: u64) -> FakeClock {
        FakeClock { now: Cell::new(ms), sleeps: RefCell::new(Vec::new()) }
    }
}

impl Clock for FakeClock {
    fn now_ms(&self) -> u64 {
        self.now.get()
    }
    fn sleep_ms(&self, ms: u64) {
        self.sleeps.borrow_mut().push(ms);
        self.now.set(self.now.get() + ms);
    }
}

/// The largest body read (a 250-product Shopify page is a few MB at most).
const MAX_BODY: u64 = 32 * 1024 * 1024;

/// HTTPS through Windows' own TLS and certificate store, with one honest identity that is never
/// varied: no browser headers, no fingerprint tricks, no proxies. Redirects are not followed
/// automatically, so a request only ever reaches the host it names.
pub struct Http {
    agent: ureq::Agent,
}

impl Http {
    pub fn new() -> Http {
        Http::build(true)
    }

    /// Plain http allowed, for tests against a server on 127.0.0.1.
    pub fn for_local_tests() -> Http {
        Http::build(false)
    }

    fn build(https_only: bool) -> Http {
        let agent = ureq::Agent::config_builder()
            .user_agent(gift_check_core::USER_AGENT)
            .timeout_global(Some(Duration::from_secs(45)))
            .http_status_as_error(false)
            .max_redirects(0)
            .https_only(https_only)
            .tls_config(ureq::tls::TlsConfig::builder().provider(ureq::tls::TlsProvider::NativeTls).root_certs(ureq::tls::RootCerts::PlatformVerifier).build())
            .build()
            .into();
        Http { agent }
    }
}

impl Default for Http {
    fn default() -> Self {
        Http::new()
    }
}

impl Transport for Http {
    fn get(&self, url: &str) -> Result<Response, String> {
        let mut resp = self.agent.get(url).header("Accept", "application/json, text/plain;q=0.9, */*;q=0.5").call().map_err(|e| e.to_string())?;
        let status = resp.status().as_u16();
        let headers = resp.headers().iter().map(|(k, v)| (k.as_str().to_owned(), v.to_str().unwrap_or("").to_owned())).collect();
        let body = resp.body_mut().with_config().limit(MAX_BODY).lossy_utf8(true).read_to_string().map_err(|e| e.to_string())?;
        Ok(Response { status, headers, body })
    }
}
