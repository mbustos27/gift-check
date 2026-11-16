//! Every test talks to a local mock server on 127.0.0.1 through the real HTTP client; none ever
//! reaches a real store. A fake clock records the waits instead of waiting.

use super::*;
use gift_check_core::Config;
use std::sync::{Arc, Mutex};

type Reply = (u16, Vec<(&'static str, String)>, String);
type Routes = Arc<Mutex<Vec<(String, Vec<Reply>)>>>;

/// Answers each URL (path + query) from its own queue of replies; the last reply repeats.
struct Mock {
    server: Arc<tiny_http::Server>,
    base: String,
    seen: Arc<Mutex<Vec<(String, String)>>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Mock {
    fn start(routes: Vec<(&str, Vec<Reply>)>) -> Mock {
        let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").unwrap());
        let port = server.server_addr().to_ip().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let routes: Routes = Arc::new(Mutex::new(routes.into_iter().map(|(k, v)| (k.to_string(), v)).collect()));
        let (s, seen2) = (server.clone(), seen.clone());
        let thread = std::thread::spawn(move || {
            for req in s.incoming_requests() {
                let url = req.url().to_string();
                let ua = req.headers().iter().find(|h| h.field.equiv("User-Agent")).map(|h| h.value.to_string()).unwrap_or_default();
                seen2.lock().unwrap().push((url.clone(), ua));
                let reply = {
                    let mut r = routes.lock().unwrap();
                    match r.iter_mut().find(|(k, _)| *k == url) {
                        Some((_, q)) if q.len() > 1 => q.remove(0),
                        Some((_, q)) => q[0].clone(),
                        None => (404, vec![], "not found".into()),
                    }
                };
                let mut resp = tiny_http::Response::from_string(reply.2).with_status_code(reply.0);
                for (k, v) in reply.1 {
                    resp.add_header(tiny_http::Header::from_bytes(k.as_bytes(), v.as_bytes()).unwrap());
                }
                let _ = req.respond(resp);
            }
        });
        Mock { server, base: format!("http://127.0.0.1:{port}"), seen, thread: Some(thread) }
    }

    fn urls(&self) -> Vec<String> {
        self.seen.lock().unwrap().iter().map(|(u, _)| u.clone()).collect()
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.server.unblock();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn ok(body: &str) -> Reply {
    (200, vec![("Content-Type", "application/json".into())], body.to_string())
}

fn robots_ok() -> (&'static str, Vec<Reply>) {
    ("/robots.txt", vec![(200, vec![], "User-agent: *\nDisallow: /cart\nDisallow: /checkout\n".into())])
}

/// A products.json page with these ids.
fn page(ids: &[u64]) -> Reply {
    let items: Vec<String> = ids
        .iter()
        .map(|id| {
            format!(
                r#"{{"id":{id},"title":"Charm {id}","handle":"charm-{id}","body_html":"<p>Nice</p>","product_type":"Charm","tags":[],
                "variants":[{{"id":{v},"title":"Default Title","price":"20.00","compare_at_price":null,"available":true}}],"images":[]}}"#,
                v = id * 10
            )
        })
        .collect();
    ok(&format!(r#"{{"products":[{}]}}"#, items.join(",")))
}

fn store() -> StoreCfg {
    StoreCfg::ad_hoc_shopify("shop.example", "USD")
}

fn rules() -> Rules {
    Config::built_in().unwrap().rules
}

fn run(mock: &Mock, db: &Db, clock: &FakeClock) -> RunReport {
    let http = Http::for_local_tests();
    run_shopify(db, &rules(), &store(), &mock.base, None, &http, clock, &mut |_| {}).unwrap()
}

#[test]
fn pages_until_an_empty_page_with_3_s_between_requests_and_an_honest_user_agent() {
    let mock = Mock::start(vec![
        robots_ok(),
        ("/products.json?limit=250&page=1", vec![page(&[1, 2])]),
        ("/products.json?limit=250&page=2", vec![page(&[3])]),
        ("/products.json?limit=250&page=3", vec![page(&[])]),
    ]);
    let (db, clock) = (Db::memory().unwrap(), FakeClock::default());
    let rep = run(&mock, &db, &clock);
    assert_eq!(rep.stop, None);
    assert!(rep.complete);
    assert_eq!((rep.pages, rep.products, rep.requests), (2, 3, 4));
    assert_eq!(mock.urls(), vec!["/robots.txt", "/products.json?limit=250&page=1", "/products.json?limit=250&page=2", "/products.json?limit=250&page=3"]);
    assert!(mock.seen.lock().unwrap().iter().all(|(_, ua)| ua == gift_check_core::USER_AGENT), "{:?}", mock.seen.lock().unwrap());
    assert_eq!(*clock.sleeps.borrow(), vec![3000, 3000, 3000], "3 s before every request after the first");
    let p = db.product("shop.example", "1").unwrap().unwrap();
    assert_eq!(p.url, "https://shop.example/products/charm-1", "links use the store's origin, not the fetch base");
    assert_eq!(p.kind.as_deref(), Some("charm"));
}

#[test]
fn a_429_is_waited_out_once_then_the_run_continues() {
    let mock = Mock::start(vec![
        robots_ok(),
        ("/products.json?limit=250&page=1", vec![(429, vec![("Retry-After", "7".into())], "local_rate_limited".into()), page(&[1])]),
        ("/products.json?limit=250&page=2", vec![page(&[])]),
    ]);
    let (db, clock) = (Db::memory().unwrap(), FakeClock::default());
    let rep = run(&mock, &db, &clock);
    assert_eq!(rep.stop, None, "{:?}", rep.log);
    assert!(rep.complete);
    assert_eq!(rep.products, 1);
    assert!(clock.sleeps.borrow().contains(&7000), "honoured Retry-After: {:?}", clock.sleeps.borrow());
    assert!(rep.log.iter().any(|l| l.contains("waiting 7 s")));
}

#[test]
fn a_second_429_stops_the_run_and_keeps_what_was_fetched() {
    let mock = Mock::start(vec![
        robots_ok(),
        ("/products.json?limit=250&page=1", vec![page(&[1, 2])]),
        ("/products.json?limit=250&page=2", vec![(429, vec![("Retry-After", "1".into())], "slow down".into())]),
    ]);
    let (db, clock) = (Db::memory().unwrap(), FakeClock::default());
    // Mark an older product so we can see a stopped run doesn't discontinue anything.
    let old = Product { store: "shop.example".into(), id: "99".into(), name: "Old".into(), url: "https://shop.example/products/old".into(), currency: "USD".into(), ..Default::default() };
    let c = db.begin_capture("shop.example", "shopify", None, None, &[]).unwrap();
    db.store_products(c, "2026-11-01T00:00:00Z", &[old], &rules()).unwrap();
    let rep = run(&mock, &db, &clock);
    let stop = rep.stop.clone().unwrap();
    assert!(stop.contains("slow down") && stop.contains("kept"), "{stop}");
    assert!(!rep.complete);
    assert_eq!(rep.products, 2, "page 1 is kept");
    assert!(db.product("shop.example", "1").unwrap().is_some());
    assert!(!db.product("shop.example", "99").unwrap().unwrap().discontinued, "an incomplete run marks nothing discontinued");
    assert_eq!(mock.urls().iter().filter(|u| u.contains("page=2")).count(), 2, "one retry, no more");
    let caps = db.captures(5).unwrap();
    assert_eq!(caps[0].note.as_deref(), Some(stop.as_str()));
    assert!(!caps[0].complete);
}

#[test]
fn a_long_retry_after_stops_without_waiting() {
    let mock = Mock::start(vec![robots_ok(), ("/products.json?limit=250&page=1", vec![(429, vec![("Retry-After", "600".into())], "".into())])]);
    let (db, clock) = (Db::memory().unwrap(), FakeClock::default());
    let rep = run(&mock, &db, &clock);
    assert!(rep.stop.unwrap().contains("600 s"));
    assert!(clock.sleeps.borrow().iter().all(|s| *s <= 3000), "never waited 600 s: {:?}", clock.sleeps.borrow());
}

#[test]
fn a_bot_check_stops_at_once_and_points_to_browser_capture() {
    let challenge = (403, vec![("cf-mitigated", "challenge".into()), ("Content-Type", "text/html".into())], "<html><head><title>Just a moment...</title></head></html>".into());
    let mock = Mock::start(vec![("/robots.txt", vec![challenge])]);
    let (db, clock) = (Db::memory().unwrap(), FakeClock::default());
    let rep = run(&mock, &db, &clock);
    let stop = rep.stop.unwrap();
    assert!(stop.contains("bot check") && stop.contains("browser capture"), "{stop}");
    assert_eq!(mock.urls(), vec!["/robots.txt"], "no retries, no other requests");

    // A 200 page that is really a challenge is caught too.
    let mock = Mock::start(vec![robots_ok(), ("/products.json?limit=250&page=1", vec![(200, vec![], "<html><script src=\"/cdn-cgi/challenge-platform/h/b/orchestrate\"></script></html>".into())])]);
    let rep = run(&mock, &db, &FakeClock::default());
    assert!(rep.stop.unwrap().contains("Cloudflare challenge platform"));
}

#[test]
fn robots_rules_are_obeyed() {
    let mock = Mock::start(vec![("/robots.txt", vec![(200, vec![], "User-agent: other\nUser-agent: gift-check\nDisallow: /products.json\n\nUser-agent: *\nAllow: /\n".into())])]);
    let (db, clock) = (Db::memory().unwrap(), FakeClock::default());
    let rep = run(&mock, &db, &clock);
    assert!(rep.stop.unwrap().contains("robots.txt does not allow"));
    assert_eq!(mock.urls(), vec!["/robots.txt"], "the disallowed page is never requested");

    let mock = Mock::start(vec![("/robots.txt", vec![(503, vec![], "down".into())])]);
    let rep = run(&mock, &db, &clock);
    assert!(rep.stop.unwrap().contains("counts as disallowed"));
    assert_eq!(mock.urls(), vec!["/robots.txt"]);

    // No robots.txt (404) means no rules.
    let mock = Mock::start(vec![("/products.json?limit=250&page=1", vec![page(&[])])]);
    let rep = run(&mock, &db, &clock);
    assert_eq!(rep.stop, None);
}

#[test]
fn a_full_read_discontinues_what_is_gone_but_a_collection_read_does_not() {
    let db = Db::memory().unwrap();
    let mock = Mock::start(vec![robots_ok(), ("/products.json?limit=250&page=1", vec![page(&[1, 2]), page(&[1])]), ("/products.json?limit=250&page=2", vec![page(&[])])]);
    let rep = run(&mock, &db, &FakeClock::default());
    assert_eq!(rep.products, 2);
    let rep = run(&mock, &db, &FakeClock::default());
    assert_eq!((rep.products, rep.discontinued), (1, 1));
    assert!(db.product("shop.example", "2").unwrap().unwrap().discontinued);

    let mock = Mock::start(vec![robots_ok(), ("/collections/charms/products.json?limit=250&page=1", vec![page(&[3])]), ("/collections/charms/products.json?limit=250&page=2", vec![page(&[])])]);
    let http = Http::for_local_tests();
    let rep = run_shopify(&db, &rules(), &store(), &mock.base, Some("charms"), &http, &FakeClock::default(), &mut |_| {}).unwrap();
    assert!(rep.complete);
    assert_eq!(rep.discontinued, 0);
    assert!(!db.product("shop.example", "1").unwrap().unwrap().discontinued);
    assert!(run_shopify(&db, &rules(), &store(), &mock.base, Some("../admin"), &http, &FakeClock::default(), &mut |_| {}).is_err());
}

#[test]
fn not_shopify_and_redirects_stop_cleanly() {
    let db = Db::memory().unwrap();
    let mock = Mock::start(vec![robots_ok()]);
    assert!(run(&mock, &db, &FakeClock::default()).stop.unwrap().contains("no products.json"));
    let mock = Mock::start(vec![robots_ok(), ("/products.json?limit=250&page=1", vec![(302, vec![("Location", "/password".into())], "".into())])]);
    assert!(run(&mock, &db, &FakeClock::default()).stop.unwrap().contains("redirected to /password"));
}

#[test]
fn challenge_detection() {
    let r = |status: u16, headers: Vec<(&str, &str)>, body: &str| Response { status, headers: headers.into_iter().map(|(a, b)| (a.to_string(), b.to_string())).collect(), body: body.to_string() };
    assert!(detect_challenge(&r(403, vec![("cf-mitigated", "challenge")], "")).is_some());
    assert!(detect_challenge(&r(503, vec![], "<!DOCTYPE html><html><head><title>Just a moment...</title>")).is_some());
    assert!(detect_challenge(&r(403, vec![("Server", "AkamaiGHost")], "<HTML><HEAD><TITLE>Access Denied</TITLE>")).is_some());
    assert!(detect_challenge(&r(403, vec![], "Access Denied. Reference #18.abc")).is_some());
    assert!(detect_challenge(&r(429, vec![], "local_rate_limited")).is_none(), "a rate limit isn't a bot check");
    assert!(detect_challenge(&r(200, vec![], r#"{"products":[{"title":"Just a moment charm"}]}"#)).is_none());
    assert!(detect_challenge(&r(404, vec![("Server", "cloudflare")], "not found")).is_none());
}

#[test]
fn retry_after_values() {
    assert_eq!(retry_after_secs(Some("60"), 0), Some(60));
    assert_eq!(retry_after_secs(Some(" 5 "), 0), Some(5));
    let now = time::parse_http_date("Sun, 15 Nov 2026 16:00:00 GMT").unwrap();
    assert_eq!(retry_after_secs(Some("Sun, 15 Nov 2026 16:00:30 GMT"), now), Some(30));
    assert_eq!(retry_after_secs(Some("soon"), 0), None);
    assert_eq!(retry_after_secs(None, 0), None);
}


/// An in-memory transport for host names a local server can't stand in for (www vs apex).
struct Fake {
    routes: std::cell::RefCell<Vec<(String, Vec<Response>)>>,
    seen: std::cell::RefCell<Vec<String>>,
}

impl Fake {
    fn new(routes: Vec<(&str, Vec<Response>)>) -> Fake {
        Fake { routes: std::cell::RefCell::new(routes.into_iter().map(|(k, v)| (k.to_string(), v)).collect()), seen: Default::default() }
    }
}

impl Transport for Fake {
    fn get(&self, url: &str) -> Result<Response, String> {
        self.seen.borrow_mut().push(url.to_string());
        let mut r = self.routes.borrow_mut();
        Ok(match r.iter_mut().find(|(k, _)| k == url) {
            Some((_, q)) if q.len() > 1 => q.remove(0),
            Some((_, q)) => q[0].clone(),
            None => resp(404, &[], "not found"),
        })
    }
}

fn resp(status: u16, headers: &[(&str, &str)], body: &str) -> Response {
    Response { status, headers: headers.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect(), body: body.into() }
}

fn fake_page(ids: &[u64]) -> Response {
    let (status, _, body) = page(ids);
    resp(status, &[], &body)
}

const WWW: &str = "https://www.shop.example";

fn run_fake(t: &Fake, db: &Db, clock: &FakeClock) -> RunReport {
    run_shopify(db, &rules(), &store(), WWW, None, t, clock, &mut |_| {}).unwrap()
}

#[test]
fn a_crawl_delay_over_the_cap_stops_before_the_catalog() {
    let mock = Mock::start(vec![("/robots.txt", vec![(200, vec![], "User-agent: *\nCrawl-delay: 120\n".into())]), ("/products.json?limit=250&page=1", vec![page(&[1])])]);
    let (db, clock) = (Db::memory().unwrap(), FakeClock::default());
    let rep = run(&mock, &db, &clock);
    let stop = rep.stop.unwrap();
    assert!(stop.contains("asks for 120 s") && stop.contains("60 s"), "{stop}");
    assert_eq!(mock.urls(), vec!["/robots.txt"], "nothing else was requested");
    assert!(clock.sleeps.borrow().iter().all(|s| *s <= 3000), "it didn't quietly use 60 s instead: {:?}", clock.sleeps.borrow());
    // 60 s itself is honoured.
    let mock = Mock::start(vec![("/robots.txt", vec![(200, vec![], "User-agent: *\nCrawl-delay: 60\n".into())]), ("/products.json?limit=250&page=1", vec![page(&[])])]);
    let clock = FakeClock::default();
    let rep = run(&mock, &Db::memory().unwrap(), &clock);
    assert_eq!(rep.stop, None);
    assert_eq!(*clock.sleeps.borrow(), vec![60_000]);
}

#[test]
fn robots_redirects_are_resolved_properly_and_never_mean_allow_all() {
    let disallow_all = resp(200, &[], "User-agent: *\nDisallow: /\n");
    // Protocol-relative to the bare domain, whose robots.txt disallows everything.
    let t = Fake::new(vec![
        ("https://www.shop.example/robots.txt", vec![resp(301, &[("Location", "//shop.example/robots.txt")], "")]),
        ("https://shop.example/robots.txt", vec![disallow_all.clone()]),
        ("https://www.shop.example/products.json?limit=250&page=1", vec![fake_page(&[1])]),
    ]);
    let rep = run_fake(&t, &Db::memory().unwrap(), &FakeClock::default());
    assert!(rep.stop.as_deref().unwrap().contains("robots.txt does not allow"), "{:?}", rep.stop);
    assert_eq!(*t.seen.borrow(), vec!["https://www.shop.example/robots.txt", "https://shop.example/robots.txt"]);

    // Relative without a leading slash: resolved against the robots.txt URL.
    let t = Fake::new(vec![
        ("https://www.shop.example/robots.txt", vec![resp(302, &[("Location", "robots-main.txt")], "")]),
        ("https://www.shop.example/robots-main.txt", vec![disallow_all.clone()]),
        ("https://www.shop.example/products.json?limit=250&page=1", vec![fake_page(&[1])]),
    ]);
    let rep = run_fake(&t, &Db::memory().unwrap(), &FakeClock::default());
    assert!(rep.stop.as_deref().unwrap().contains("robots.txt does not allow"), "{:?}", rep.stop);

    // Redirects that can't be followed stop the run: nothing but robots.txt is requested.
    for (location, why) in [
        (None, "without a Location"),
        (Some("http://www.shop.example/robots.txt"), "https down to http"),
        (Some("https://robots.other.example/robots.txt"), "another site"),
        (Some("https://[bad"), "isn't a usable URL"),
    ] {
        let headers: Vec<(&str, &str)> = location.map(|l| vec![("Location", l)]).unwrap_or_default();
        let t = Fake::new(vec![("https://www.shop.example/robots.txt", vec![resp(301, &headers, "")]), ("https://www.shop.example/products.json?limit=250&page=1", vec![fake_page(&[1])])]);
        let rep = run_fake(&t, &Db::memory().unwrap(), &FakeClock::default());
        let stop = rep.stop.unwrap_or_default();
        assert!(stop.contains("could not be read") && stop.contains(why), "{location:?}: {stop}");
        assert_eq!(t.seen.borrow().len(), 1, "{location:?}: {:?}", t.seen.borrow());
    }

    // A loop of redirects ends after five.
    let t = Fake::new(vec![("https://www.shop.example/robots.txt", vec![resp(302, &[("Location", "/robots.txt")], "")])]);
    let rep = run_fake(&t, &Db::memory().unwrap(), &FakeClock::default());
    assert!(rep.stop.unwrap().contains("more than 5 redirects"));
    assert_eq!(t.seen.borrow().len(), 6);
}

#[test]
fn redirect_resolution() {
    assert_eq!(resolve("https://a.example/robots.txt", "/x/robots.txt").as_deref(), Ok("https://a.example/x/robots.txt"));
    assert_eq!(resolve("https://a.example/robots.txt", "https://www.a.example/robots.txt").as_deref(), Ok("https://www.a.example/robots.txt"));
    assert_eq!(resolve("https://a.example/dir/robots.txt", "robots2.txt").as_deref(), Ok("https://a.example/dir/robots2.txt"));
    assert_eq!(resolve("https://www.a.example/robots.txt", "//a.example/r.txt").as_deref(), Ok("https://a.example/r.txt"));
    assert_eq!(resolve("https://a.example/robots.txt", "../../r.txt").as_deref(), Ok("https://a.example/r.txt"));
    assert!(resolve("https://a.example/robots.txt", "http://a.example/robots.txt").is_err(), "no downgrade to http");
    assert!(resolve("https://a.example/robots.txt", "ftp://a.example/robots.txt").is_err());
    assert!(same_site("https://www.a.example/robots.txt", "https://a.example/robots.txt"));
    assert!(!same_site("https://a.example/robots.txt", "https://a.example.evil/robots.txt"));
    assert!(!same_site("https://a.example/robots.txt", "https://a.example:8443/robots.txt"));
}

#[test]
fn a_429_on_page_one_and_another_on_page_two_stops_without_a_second_wait() {
    let mock = Mock::start(vec![
        robots_ok(),
        ("/products.json?limit=250&page=1", vec![(429, vec![("Retry-After", "5".into())], "".into()), page(&[1])]),
        ("/products.json?limit=250&page=2", vec![(429, vec![("Retry-After", "5".into())], "".into()), page(&[])]),
    ]);
    let (db, clock) = (Db::memory().unwrap(), FakeClock::default());
    let rep = run(&mock, &db, &clock);
    assert!(rep.stop.as_deref().unwrap().contains("a second 429"), "{:?}", rep.stop);
    assert!(!rep.complete);
    assert_eq!(rep.products, 1, "page 1 is kept");
    assert_eq!(clock.sleeps.borrow().iter().filter(|s| **s == 5000).count(), 1, "waited once: {:?}", clock.sleeps.borrow());
    assert_eq!(mock.urls().iter().filter(|u| u.contains("page=2")).count(), 1, "page 2 was not retried");
}

#[test]
fn the_next_run_honours_the_spacing_and_retry_after_of_the_last() {
    let db = Db::memory().unwrap();
    let mock = Mock::start(vec![robots_ok(), ("/products.json?limit=250&page=1", vec![page(&[])])]);
    let first = FakeClock::at(1_000_000);
    run(&mock, &db, &first);
    let last = first.now_ms();
    // A second run one second later waits out the rest of the 3 s before its first request.
    let second = FakeClock::at(last + 1_000);
    run(&mock, &db, &second);
    assert_eq!(second.sleeps.borrow().first(), Some(&2_000), "{:?}", second.sleeps.borrow());

    // A 429 whose Retry-After is longer than gift-check waits: the next run doesn't even start.
    let mock = Mock::start(vec![robots_ok(), ("/products.json?limit=250&page=1", vec![(429, vec![("Retry-After", "600".into())], "".into())])]);
    let clock = FakeClock::at(10_000_000);
    assert!(run(&mock, &db, &clock).stop.unwrap().contains("600 s"));
    let n = mock.urls().len();
    let soon = FakeClock::at(clock.now_ms() + 60_000);
    let rep = run(&mock, &db, &soon);
    let stop = rep.stop.unwrap();
    assert!(stop.contains("Retry-After") && stop.contains("not to come back before"), "{stop}");
    assert_eq!((mock.urls().len(), rep.requests), (n, 0), "no request before the time the store asked for");
    // After that time it runs again.
    let later = FakeClock::at(clock.now_ms() + 601_000);
    assert!(!run(&mock, &db, &later).stop.unwrap_or_default().contains("not to come back"));
}

#[test]
fn a_store_that_ignores_the_page_number_stops_the_run() {
    let db = Db::memory().unwrap();
    let mock = Mock::start(vec![robots_ok(), ("/products.json?limit=250&page=1", vec![page(&[1, 2])]), ("/products.json?limit=250&page=2", vec![page(&[3])]), ("/products.json?limit=250&page=3", vec![page(&[])])]);
    run(&mock, &db, &FakeClock::default());
    // Now every page answers with page 1.
    let mock = Mock::start(vec![robots_ok(), ("/products.json?limit=250&page=1", vec![page(&[1, 2])]), ("/products.json?limit=250&page=2", vec![page(&[1, 2])])]);
    let rep = run(&mock, &db, &FakeClock::default());
    assert!(rep.stop.as_deref().unwrap().contains("ignore the page number"), "{:?}", rep.stop);
    assert!(!rep.complete);
    assert_eq!((rep.pages, rep.discontinued), (1, 0));
    assert!(!db.product("shop.example", "3").unwrap().unwrap().discontinued, "a run that never ended marks nothing gone");
    assert!(mock.urls().iter().all(|u| !u.contains("page=3")));
}
