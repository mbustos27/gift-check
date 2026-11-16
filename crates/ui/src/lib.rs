//! The gift-check window: a small web server on 127.0.0.1 that only this computer can reach,
//! serving the window, the capture bridge, and a JSON API over the same core the command line uses.
//!
//! Safety (the buy-check model): it binds to 127.0.0.1 only; it refuses requests whose Host isn't
//! this server (blocks DNS-rebinding pages); API reads refuse other origins; every POST must carry
//! the `X-Gift-Check: 1` header and a JSON body, which a page on another site can't send without a
//! CORS preflight this server never approves; the pages load no outside scripts, and the CSP only
//! lets images come from the configured store image hosts. Store text is only ever shown with
//! `textContent`.

use gift_check_core::capture::{self, Chunk, MAX_CHUNK_BYTES, MAX_CHUNK_PRODUCTS, MAX_FILE_BYTES};
use gift_check_core::db::{Db, Query, Scope, Sort};
use gift_check_core::model::{GiftPlan, PlanItem, Product, Recipient, KINDS};
use gift_check_core::stores::SourceKind;
use gift_check_core::{plan, time, Config};
use serde::Deserialize;
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const INDEX: &str = include_str!("../web/index.html");
const APP_JS: &str = include_str!("../web/app.js");
const STYLE: &str = include_str!("../web/style.css");
const BRIDGE: &str = include_str!("../web/bridge.html");
const BRIDGE_JS: &str = include_str!("../web/bridge.js");

pub const DEFAULT_PORT: u16 = 8781;
/// The custom header every API call from the window carries.
pub const MARKER: &str = "X-Gift-Check";
/// Bridge captures held open at once (each waiting for its remaining chunks).
const MAX_PENDING: usize = 4;

/// `gift-check ui [--no-open] [--port N]`: serves until the terminal is closed (Ctrl+C).
pub fn main(data_dir: &Path, args: &[String]) -> i32 {
    let mut open = true;
    let mut port = DEFAULT_PORT;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--no-open" => open = false,
            "--port" => match it.next().and_then(|p| p.parse::<u16>().ok()).filter(|p| *p >= 1024) {
                Some(p) => port = p,
                None => {
                    eprintln!("gift-check ui: --port needs a number from 1024 to 65535");
                    return 2;
                }
            },
            other => {
                eprintln!("gift-check ui: unknown option {other:?} (use --no-open, --port N)");
                return 2;
            }
        }
    }
    let app = match App::open(data_dir, port, true) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("gift-check ui: {e}");
            return 1;
        }
    };
    let url = format!("http://127.0.0.1:{port}/");
    let server = match tiny_http::Server::http(("127.0.0.1", port)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("gift-check ui: can't use port {port} ({e}). If gift-check is already running, use {url}");
            return 2;
        }
    };
    println!("gift-check window: {url}  (Ctrl+C here to stop)");
    println!("Capture bridge:    {url}bridge");
    if port != DEFAULT_PORT {
        println!("Note: capture scripts post to http://127.0.0.1:{DEFAULT_PORT}; on another port they need their target changed.");
    }
    if open {
        open_browser(&url);
    }
    for req in server.incoming_requests() {
        // One bad request mustn't take the whole server down.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| app.handle(req)));
    }
    0
}

pub fn open_browser(url: &str) {
    #[cfg(windows)]
    let r = std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler", url]).spawn();
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let r = std::process::Command::new("xdg-open").arg(url).spawn();
    if r.is_err() {
        println!("Open {url} in your browser.");
    }
}

/// A bridge capture whose chunks are still arriving.
struct Pending {
    capture_id: String,
    db_id: i64,
    store: String,
    origin: String,
    captured_at: String,
    complete: bool,
    complete_listings: Vec<String>,
    chunks: u32,
    received: BTreeSet<u32>,
    sources: Vec<String>,
    merged: usize,
    warnings: Vec<String>,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
struct FetchState {
    running: bool,
    store: Option<String>,
    started_at: Option<String>,
    lines: Vec<String>,
    result: Option<Value>,
}

pub struct App {
    pub port: u16,
    data_dir: PathBuf,
    db: Db,
    cfg: Config,
    pending: RefCell<Vec<Pending>>,
    /// Bridge capture ids already stored, so a resent chunk isn't stored twice.
    done: RefCell<Vec<String>>,
    /// Bridge capture ids closed as incomplete to make room; their late chunks are refused.
    evicted: RefCell<Vec<String>>,
    fetch: Arc<Mutex<FetchState>>,
    allow_fetch: bool,
    /// Ids for single-message captures that carry no capture_id.
    counter: Cell<u32>,
}

/// A reply before it becomes an HTTP response, so tests can look at it.
#[derive(Debug)]
pub struct Reply {
    pub status: u16,
    pub content_type: &'static str,
    pub body: String,
}

fn ok_json(v: Value) -> Reply {
    Reply { status: 200, content_type: "application/json; charset=utf-8", body: v.to_string() }
}

fn err_json(status: u16, msg: impl Into<String>) -> Reply {
    Reply { status, content_type: "application/json; charset=utf-8", body: json!({ "error": msg.into() }).to_string() }
}

/// The largest request body each path accepts.
pub fn body_limit(path: &str) -> usize {
    if path == "/api/capture" {
        MAX_CHUNK_BYTES
    } else if path == "/api/import" {
        MAX_FILE_BYTES as usize
    } else {
        1_000_000
    }
}

impl App {
    /// Opens the database in `data_dir` and applies the current rules. `allow_fetch` lets the
    /// window start Shopify fetches (off in tests).
    pub fn open(data_dir: &Path, port: u16, allow_fetch: bool) -> Result<App, String> {
        let cfg = Config::load(data_dir)?;
        let db = Db::open(&gift_check_core::db_path(data_dir))?;
        App::with(db, cfg, data_dir, port, allow_fetch)
    }

    pub fn with(db: Db, cfg: Config, data_dir: &Path, port: u16, allow_fetch: bool) -> Result<App, String> {
        db.sync_rules(&cfg.rules, &cfg.rules_hash)?;
        // Chunks still owed to a bridge capture from before a restart will never come.
        db.close_open_captures("bridge", "incomplete: gift-check was restarted before all chunks arrived")?;
        for s in &cfg.stores.stores {
            db.upsert_store(s)?;
        }
        Ok(App {
            port,
            data_dir: data_dir.to_path_buf(),
            db,
            cfg,
            pending: RefCell::new(Vec::new()),
            done: RefCell::new(Vec::new()),
            evicted: RefCell::new(Vec::new()),
            fetch: Arc::new(Mutex::new(FetchState::default())),
            allow_fetch,
            counter: Cell::new(0),
        })
    }

    /// The headers every response carries. Images may only come from the stores' image hosts.
    pub fn security_headers(&self) -> Vec<(&'static str, String)> {
        let imgs: Vec<String> = self.cfg.stores.image_hosts().iter().map(|h| format!("https://{h}")).collect();
        vec![
            ("X-Content-Type-Options", "nosniff".into()),
            ("Cache-Control", "no-store".into()),
            ("Referrer-Policy", "no-referrer".into()),
            ("X-Frame-Options", "DENY".into()),
            (
                "Content-Security-Policy",
                format!(
                    "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' {}; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'",
                    imgs.join(" ")
                ),
            ),
        ]
    }

    fn handle(&self, mut req: tiny_http::Request) {
        let method = req.method().as_str().to_string();
        let url = req.url().to_string();
        let header = |name: &str| req.headers().iter().find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name)).map(|h| h.value.as_str().to_string());
        let (host, marker, ctype, origin) = (header("Host"), header(MARKER), header("Content-Type"), header("Origin"));
        let path = url.split('?').next().unwrap_or("").to_string();
        let mut body = String::new();
        let mut too_big = false;
        if method == "POST" {
            let limit = body_limit(&path);
            let mut buf = Vec::new();
            let _ = req.as_reader().take(limit as u64 + 1).read_to_end(&mut buf);
            too_big = buf.len() > limit;
            body = String::from_utf8_lossy(&buf).into_owned();
        }
        let reply = if too_big {
            err_json(413, format!("request too large (limit {} MB)", body_limit(&path) / 1_000_000))
        } else {
            self.route(&method, &url, host.as_deref(), marker.as_deref(), ctype.as_deref(), origin.as_deref(), &body)
        };
        let mut resp = tiny_http::Response::from_string(reply.body).with_status_code(reply.status);
        let mut headers = vec![("Content-Type", reply.content_type.to_string())];
        headers.extend(self.security_headers());
        for (k, v) in headers {
            if let Ok(h) = tiny_http::Header::from_bytes(k.as_bytes(), v.as_bytes()) {
                resp.add_header(h);
            }
        }
        let _ = req.respond(resp);
    }

    /// Everything except the socket, so it can be tested directly.
    #[allow(clippy::too_many_arguments)]
    pub fn route(&self, method: &str, url: &str, host: Option<&str>, marker: Option<&str>, ctype: Option<&str>, origin: Option<&str>, body: &str) -> Reply {
        let own = [format!("127.0.0.1:{}", self.port), format!("localhost:{}", self.port)];
        if !host.is_some_and(|h| own.iter().any(|o| o.eq_ignore_ascii_case(h))) {
            return err_json(421, "wrong host");
        }
        let ours = |o: &str| own.iter().any(|h| o.eq_ignore_ascii_case(&format!("http://{h}")));
        let (path, query) = url.split_once('?').unwrap_or((url, ""));
        if path.starts_with("/api/") {
            // Reads hold recipients and plans: another origin may not read them either.
            if origin.is_some_and(|o| !ours(o)) {
                return err_json(403, "requests must come from the gift-check window");
            }
            if method == "POST" && (marker != Some("1") || !ctype.is_some_and(|c| c.starts_with("application/json"))) {
                return err_json(403, "requests must come from the gift-check window");
            }
        }
        let html = |b: &str| Reply { status: 200, content_type: "text/html; charset=utf-8", body: b.into() };
        let js = |b: &str| Reply { status: 200, content_type: "text/javascript; charset=utf-8", body: b.into() };
        let result = match (method, path) {
            ("GET", "/") => return html(INDEX),
            ("GET", "/bridge") => return html(BRIDGE),
            ("GET", "/app.js") => return js(APP_JS),
            ("GET", "/bridge.js") => return js(BRIDGE_JS),
            ("GET", "/style.css") => return Reply { status: 200, content_type: "text/css; charset=utf-8", body: STYLE.into() },
            ("GET", "/api/state") => self.state(),
            ("GET", "/api/search") => self.search(query),
            ("GET", "/api/product") => self.product(query),
            ("GET", "/api/compare") => self.db.compare(&param(query, "by").unwrap_or_else(|| "store".into()), param(query, "currency").filter(|c| !c.is_empty()).as_deref()),
            ("GET", "/api/plans") => self.plans(),
            ("GET", "/api/fetch") => Ok(json!(self.fetch.lock().map(|s| s.clone()).unwrap_or_default())),
            ("POST", "/api/recipient") => self.save_recipient(body),
            ("POST", "/api/plan/evaluate") => self.evaluate(body),
            ("POST", "/api/plan/save") => self.save_plan(body),
            ("POST", "/api/plan/delete") => self.delete_plan(body),
            ("POST", "/api/plan/suggest") => self.suggest(body),
            ("POST", "/api/capture") => self.capture(body),
            ("POST", "/api/import") => self.import(body),
            ("POST", "/api/fetch") => self.start_fetch(body),
            ("GET", _) | ("POST", _) => return err_json(404, "not found"),
            _ => return err_json(405, "method not allowed"),
        };
        match result {
            Ok(v) => ok_json(v),
            Err(e) => err_json(400, e),
        }
    }

    fn state(&self) -> Result<Value, String> {
        let counts = self.db.store_counts()?;
        let stores: Vec<Value> = self
            .cfg
            .stores
            .stores
            .iter()
            .map(|s| {
                let c = counts.iter().find(|c| c["store"] == s.id.as_str()).cloned().unwrap_or(json!({}));
                json!({
                    "id": s.id, "name": s.name, "origin": s.origin, "source": s.source.as_str(), "start_urls": s.start_urls,
                    "note": s.note, "products": c["products"].as_i64().unwrap_or(0), "discontinued": c["discontinued"].as_i64().unwrap_or(0),
                    "on_sale": c["on_sale"].as_i64().unwrap_or(0), "last_capture": c["last_capture"],
                })
            })
            .collect();
        let r = &self.cfg.rules;
        let mut metal_groups: Vec<String> = self.db.metal_groups()?;
        for m in r.metal_groups.iter().map(|m| m.group.clone()) {
            if !metal_groups.contains(&m) {
                metal_groups.push(m);
            }
        }
        metal_groups.sort();
        let now = time::now();
        let captures: Vec<Value> = self
            .db
            .captures(50)?
            .into_iter()
            .map(|c| {
                let when = c.finished_at.clone().unwrap_or_else(|| c.started_at.clone());
                let age = time::parse_rfc3339(&when).map(|t| time::ago(now - t));
                json!({ "capture": c, "age": age })
            })
            .collect();
        Ok(json!({
            "version": gift_check_core::VERSION,
            "port": self.port,
            "data_dir": self.data_dir.display().to_string(),
            "stores_from": self.cfg.stores_from,
            "rules_from": self.cfg.rules_from,
            "stores": stores,
            "kinds": KINDS,
            "metal_groups": metal_groups,
            "systems": r.systems.iter().map(|s| json!({ "id": s.id, "label": s.label, "note": s.note, "source": s.source })).collect::<Vec<_>>(),
            "rules": { "count": r.rule_count(), "todo": r.todo_count(), "base_kinds": r.build.base_kinds, "addon_kinds": r.build.addon_kinds },
            "recipients": self.db.recipients()?,
            "captures": captures,
            "fetch": self.fetch.lock().map(|s| s.clone()).unwrap_or_default(),
            "image_hosts": self.cfg.stores.image_hosts(),
        }))
    }

    fn row(&self, p: &Product) -> Value {
        json!({
            "store": p.store, "id": p.id, "name": p.name, "url": p.url, "kind": p.kind, "system": p.system,
            "system_label": p.system.as_deref().map(|s| self.cfg.rules.system_label(s)),
            "metal": p.metal, "metal_group": p.metal_group, "collection": p.collection, "badges": p.badges,
            "price": p.current(), "list_price": p.regular(), "pct_off": p.pct_off(), "in_stock": p.in_stock,
            "discontinued": p.discontinued, "image": p.image, "first_seen": p.first_seen, "last_seen": p.last_seen,
            "engravable": p.engravable,
        })
    }

    fn search(&self, query: &str) -> Result<Value, String> {
        let p = |k: &str| param(query, k).filter(|v| !v.trim().is_empty());
        let num = |k: &str| -> Result<Option<f64>, String> {
            match p(k) {
                None => Ok(None),
                Some(v) => v.trim().parse::<f64>().ok().filter(|n| n.is_finite() && *n >= 0.0).map(Some).ok_or(format!("{k} must be a number")),
            }
        };
        let flag = |k: &str| p(k).is_some_and(|v| v == "1" || v == "true");
        let mut q = Query {
            words: p("q").map(|s| s.split_whitespace().map(str::to_string).collect()).unwrap_or_default(),
            store: p("store"),
            kind: p("kind"),
            metal_group: p("metal"),
            min: num("min")?,
            max: num("max")?,
            on_sale: flag("sale"),
            in_stock: flag("stock"),
            include_discontinued: flag("all"),
            sort: Sort::parse(&p("sort").unwrap_or_default()).ok_or("sort must be price, price-desc, off, newest or name")?,
            limit: p("limit").and_then(|l| l.parse().ok()).unwrap_or(100).min(500),
            offset: p("offset").and_then(|o| o.parse().ok()).unwrap_or(0),
            ..Default::default()
        };
        // Add-ons that fit a bracelet system (the Build tab).
        if let Some(base) = p("fits") {
            let systems: Vec<String> = self.cfg.rules.fits.iter().filter(|f| f.base_systems.contains(&base)).map(|f| f.addon_system.clone()).collect();
            q.systems = Some(systems);
            q.kinds = Some(self.cfg.rules.build.addon_kinds.clone());
        }
        if flag("addons") && q.kinds.is_none() {
            q.kinds = Some(self.cfg.rules.build.addon_kinds.clone());
        }
        let (rows, total) = self.db.search(&q)?;
        Ok(json!({ "total": total, "rows": rows.iter().map(|p| self.row(p)).collect::<Vec<_>>() }))
    }

    fn product(&self, query: &str) -> Result<Value, String> {
        let store = param(query, "store").ok_or("which store?")?;
        let id = param(query, "id").ok_or("which product?")?;
        let p = self.db.product(&store, &id)?.ok_or("no such product")?;
        let history = self.db.history(&store, &id)?;
        let policies: Vec<Value> = self.cfg.rules.policy_warnings(&p).into_iter().map(|(t, s)| json!({ "text": t, "source": s })).collect();
        let mut row = self.row(&p);
        row["variants"] = json!(p.variants);
        row["description"] = json!(p.description);
        row["category"] = json!(p.category);
        row["themes"] = json!(p.themes);
        row["tags"] = json!(p.tags);
        row["price_at"] = json!(p.price_at);
        Ok(json!({ "product": row, "history": history, "policies": policies }))
    }

    fn save_recipient(&self, body: &str) -> Result<Value, String> {
        let r: Recipient = serde_json::from_str(body).map_err(|e| format!("bad request: {e}"))?;
        self.db.save_recipient(&r)?;
        Ok(json!({ "ok": true, "recipients": self.db.recipients()? }))
    }

    fn recipient(&self, nick: Option<&str>) -> Result<Option<Recipient>, String> {
        match nick.map(str::trim).filter(|n| !n.is_empty()) {
            None => Ok(None),
            Some(n) => self.db.recipient(n)?.map(Some).ok_or_else(|| format!("no recipient called {n:?}")),
        }
    }

    fn evaluate(&self, body: &str) -> Result<Value, String> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct In {
            #[serde(default)]
            recipient: Option<String>,
            #[serde(default)]
            budget: Option<f64>,
            items: Vec<PlanItem>,
        }
        let i: In = serde_json::from_str(body).map_err(|e| format!("bad request: {e}"))?;
        if i.items.len() > 60 {
            return Err("a plan holds at most 60 items".into());
        }
        if i.budget.is_some_and(|b| !b.is_finite() || b < 0.0) {
            return Err("budget must be a positive number".into());
        }
        let r = self.recipient(i.recipient.as_deref())?;
        let (items, missing) = self.db.plan_products(&i.items)?;
        let ev = plan::evaluate(&items, r.as_ref(), i.budget, &self.cfg.rules);
        Ok(json!({ "evaluation": ev, "missing": missing }))
    }

    fn save_plan(&self, body: &str) -> Result<Value, String> {
        let p: GiftPlan = serde_json::from_str(body).map_err(|e| format!("bad request: {e}"))?;
        if let Some(n) = &p.recipient {
            self.recipient(Some(n))?;
        }
        let id = self.db.save_plan(&p)?;
        Ok(json!({ "id": id }))
    }

    fn delete_plan(&self, body: &str) -> Result<Value, String> {
        #[derive(Deserialize)]
        struct In {
            id: i64,
        }
        let i: In = serde_json::from_str(body).map_err(|e| format!("bad request: {e}"))?;
        if !self.db.delete_plan(i.id)? {
            return Err("no such plan".into());
        }
        Ok(json!({ "ok": true }))
    }

    /// Saved plans, each re-priced from the latest observations.
    fn plans(&self) -> Result<Value, String> {
        let mut out = Vec::new();
        for p in self.db.plans()? {
            let r = self.recipient(p.recipient.as_deref()).ok().flatten();
            let (items, missing) = self.db.plan_products(&p.items)?;
            let ev = plan::evaluate(&items, r.as_ref(), p.budget, &self.cfg.rules);
            let stale = ev.oldest_price_at.as_deref().and_then(time::parse_rfc3339).is_some_and(|t| time::now() - t > plan::STALE_PRICE_DAYS * 86_400);
            let saved_total = p.items.iter().map(|it| it.saved_price.map(|x| x * f64::from(it.qty.max(1)))).sum::<Option<f64>>().map(|t| (t * 100.0).round() / 100.0);
            out.push(json!({
                "plan": p, "total": ev.total, "list_total": ev.list_total, "savings": ev.savings, "left": ev.left,
                "currency": ev.currency, "oldest_price_at": ev.oldest_price_at, "stale": stale, "saved_total": saved_total,
                "stops": ev.warnings.iter().filter(|w| w.level == "stop").count(),
                "warns": ev.warnings.iter().filter(|w| w.level == "warn").count(),
                "missing": missing,
            }));
        }
        Ok(json!({ "plans": out }))
    }

    fn suggest(&self, body: &str) -> Result<Value, String> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct In {
            #[serde(default)]
            recipient: Option<String>,
            budget: f64,
            #[serde(default)]
            store: Option<String>,
        }
        let i: In = serde_json::from_str(body).map_err(|e| format!("bad request: {e}"))?;
        if !i.budget.is_finite() || i.budget <= 0.0 || i.budget > 100_000.0 {
            return Err("give a budget above 0".into());
        }
        let r = self.recipient(i.recipient.as_deref())?.unwrap_or_else(|| Recipient { nick: "them".into(), ..Default::default() });
        let store = i.store.filter(|s| !s.is_empty());
        let catalog = self.db.catalog(store.as_deref())?;
        let s = plan::suggest(&catalog, &r, i.budget, store.as_deref(), &self.cfg.rules, 4);
        Ok(json!({ "suggestions": s, "catalog_size": catalog.len() }))
    }

    /// One bridge message: a whole capture or one chunk of one. Validated exactly like `import`.
    fn capture(&self, body: &str) -> Result<Value, String> {
        let v: Value = serde_json::from_str(body).map_err(|e| format!("the capture isn't valid JSON ({e})"))?;
        let c = capture::validate_value(&v, &self.cfg.stores, MAX_CHUNK_PRODUCTS)?;
        let chunk = c.chunk.clone().unwrap_or_else(|| {
            self.counter.set(self.counter.get() + 1);
            Chunk { capture_id: format!("single-{}-{}", time::now(), self.counter.get()), chunk: 1, chunks: 1 }
        });
        if self.evicted.borrow().contains(&chunk.capture_id) {
            return Err(format!(
                "capture {} was closed as incomplete (too many captures were half-sent at once); this chunk wasn't stored. Send the capture again (a new run of send())",
                chunk.capture_id
            ));
        }
        if self.done.borrow().contains(&chunk.capture_id) {
            return Ok(json!({ "duplicate": true, "done": true, "capture_id": chunk.capture_id, "chunk": chunk.chunk, "chunks": chunk.chunks }));
        }
        let mut pending = self.pending.borrow_mut();
        let idx = match pending.iter().position(|p| p.capture_id == chunk.capture_id) {
            Some(i) => {
                let p = &pending[i];
                if p.store != c.store || p.origin != c.origin || p.captured_at != c.captured_at || p.complete != c.complete || p.complete_listings != c.complete_listings || p.chunks != chunk.chunks {
                    return Err(format!("chunk {} of capture {} doesn't match its first chunk (store, origin, captured_at, complete, complete_listings and chunks must be the same)", chunk.chunk, chunk.capture_id));
                }
                i
            }
            None => {
                if pending.len() >= MAX_PENDING {
                    let old = pending.remove(0);
                    let note = format!("incomplete: only {} of {} chunks arrived", old.received.len(), old.chunks);
                    self.db.finish_capture(old.db_id, false, &Scope::None, Some(&note))?;
                    let mut evicted = self.evicted.borrow_mut();
                    evicted.push(old.capture_id);
                    if evicted.len() > 100 {
                        evicted.remove(0);
                    }
                }
                let store = self.cfg.stores.get(&c.store).ok_or("unknown store")?;
                self.db.upsert_store(store)?;
                let db_id = self.db.begin_capture(&c.store, "bridge", Some(&chunk.capture_id), Some(&c.captured_at), &c.sources)?;
                pending.push(Pending {
                    capture_id: chunk.capture_id.clone(),
                    db_id,
                    store: c.store.clone(),
                    origin: c.origin.clone(),
                    captured_at: c.captured_at.clone(),
                    complete: c.complete,
                    complete_listings: c.complete_listings.clone(),
                    chunks: chunk.chunks,
                    received: BTreeSet::new(),
                    sources: vec![],
                    merged: 0,
                    warnings: vec![],
                });
                pending.len() - 1
            }
        };
        let p = &mut pending[idx];
        if p.received.contains(&chunk.chunk) {
            return Ok(json!({ "duplicate": true, "done": false, "capture_id": p.capture_id, "chunk": chunk.chunk, "chunks": p.chunks, "received": p.received.len(), "stored": self.db.capture_count(p.db_id)? }));
        }
        self.db.store_products(p.db_id, &c.captured_at, &c.products, &self.cfg.rules)?;
        p.received.insert(chunk.chunk);
        for s in c.sources {
            if !p.sources.contains(&s) && p.sources.len() < 1000 {
                p.sources.push(s);
            }
        }
        p.merged += c.merged;
        let room = 50usize.saturating_sub(p.warnings.len());
        p.warnings.extend(c.warnings.into_iter().take(room));
        let stored = self.db.capture_count(p.db_id)?;
        if p.received.len() as u32 == p.chunks {
            let p = pending.remove(idx);
            self.db.set_capture_sources(p.db_id, &p.sources)?;
            let mut notes = p.warnings.clone();
            let scope = capture::scope(p.complete, &p.complete_listings, &mut notes);
            let note = (!notes.is_empty()).then(|| notes.join("; "));
            let fin = self.db.finish_capture(p.db_id, p.complete, &scope, note.as_deref())?;
            notes.extend(fin.held);
            let (count, gone) = (fin.products, fin.discontinued);
            let mut done = self.done.borrow_mut();
            done.push(p.capture_id.clone());
            if done.len() > 100 {
                done.remove(0);
            }
            return Ok(json!({ "done": true, "capture": p.db_id, "capture_id": p.capture_id, "chunk": chunk.chunk, "chunks": p.chunks, "received": p.chunks, "stored": count, "discontinued": gone, "complete": p.complete, "merged": p.merged, "warnings": notes }));
        }
        Ok(json!({ "done": false, "capture": p.db_id, "capture_id": p.capture_id, "chunk": chunk.chunk, "chunks": p.chunks, "received": p.received.len(), "stored": stored }))
    }

    /// A capture file chosen in the window: the same checks and storing as `gift-check import`
    /// (an old capture needs the command line's --force).
    fn import(&self, body: &str) -> Result<Value, String> {
        let c = capture::validate_file(body, &self.cfg.stores, false)?;
        let s = capture::store(&self.db, &self.cfg, &c, "import")?;
        Ok(json!({ "imported": s, "captured_at": c.captured_at }))
    }

    /// Starts a Shopify fetch of a configured store in the background.
    fn start_fetch(&self, body: &str) -> Result<Value, String> {
        #[derive(Deserialize)]
        struct In {
            store: String,
        }
        let i: In = serde_json::from_str(body).map_err(|e| format!("bad request: {e}"))?;
        let store = self.cfg.stores.get(&i.store).ok_or("unknown store")?.clone();
        if store.source != SourceKind::Shopify {
            return Err(format!("{} is captured in the browser (it blocks plain requests); use the bridge", store.name));
        }
        if !self.allow_fetch {
            return Err("fetching is turned off in this window".into());
        }
        {
            let mut s = self.fetch.lock().map_err(|_| "fetch state unavailable")?;
            if s.running {
                return Err(format!("already fetching {}", s.store.clone().unwrap_or_default()));
            }
            *s = FetchState { running: true, store: Some(store.id.clone()), started_at: Some(time::now_utc()), lines: vec![], result: None };
        }
        let state = self.fetch.clone();
        let (dir, rules) = (self.data_dir.clone(), self.cfg.rules.clone());
        std::thread::spawn(move || {
            let mut log = |line: &str| {
                if let Ok(mut s) = state.lock() {
                    s.lines.push(line.to_string());
                }
            };
            let res = Db::open(&gift_check_core::db_path(&dir)).and_then(|db| {
                let http = gift_check_fetch::Http::new();
                let clock = gift_check_fetch::SystemClock;
                gift_check_fetch::run_shopify(&db, &rules, &store, &store.origin, None, &http, &clock, &mut log)
            });
            if let Ok(mut s) = state.lock() {
                s.running = false;
                s.result = Some(match res {
                    Ok(r) => json!({ "products": r.products, "pages": r.pages, "complete": r.complete, "discontinued": r.discontinued, "requests": r.requests, "stop": r.stop, "log": r.log }),
                    Err(e) => json!({ "error": e }),
                });
            }
        });
        Ok(json!({ "started": true }))
    }
}

/// One query parameter, percent-decoded.
fn param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        (decode(k) == key).then(|| decode(v))
    })
}

/// Percent-decoding for a query value.
fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'+' {
            out.push(b' ');
        } else if b == b'%' && i + 2 < bytes.len() {
            match std::str::from_utf8(&bytes[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()) {
                Some(v) => {
                    out.push(v);
                    i += 3;
                    continue;
                }
                None => out.push(b),
            }
        } else {
            out.push(b);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests;
