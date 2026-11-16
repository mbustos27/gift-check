//! The local database (SQLite): stores, products, variants, price observations, captures,
//! recipients and saved gift plans.
//!
//! Every capture or fetch appends a price observation per product (and per variant); the product
//! row keeps the latest values, so the observations are the price history.

use crate::model::{GiftPlan, PlanItem, Product, Recipient, Variant};
use crate::rules::Rules;
use crate::stores::StoreCfg;
use crate::time;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, Row};
use serde::Serialize;
use serde_json::{json, Value};
use std::path::Path;

const SCHEMA_VERSION: i64 = 2;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS store(id TEXT PRIMARY KEY, name TEXT NOT NULL, origin TEXT NOT NULL, source TEXT NOT NULL, currency TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS product(
  store TEXT NOT NULL, id TEXT NOT NULL, name TEXT NOT NULL, url TEXT NOT NULL,
  category TEXT, kind TEXT, kind_given INTEGER NOT NULL DEFAULT 0, system TEXT,
  metal TEXT, metal_group TEXT, metal_group_given INTEGER NOT NULL DEFAULT 0,
  collection TEXT, themes TEXT, badges TEXT, tags TEXT NOT NULL DEFAULT '[]', vendor TEXT, description TEXT,
  engravable INTEGER, in_stock INTEGER, currency TEXT NOT NULL,
  price REAL, list_price REAL, price_at TEXT, image TEXT,
  first_seen TEXT NOT NULL, last_seen TEXT NOT NULL, last_capture INTEGER, discontinued INTEGER NOT NULL DEFAULT 0,
  listing TEXT, system_inferred INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY(store, id));
CREATE INDEX IF NOT EXISTS product_kind ON product(kind);
CREATE TABLE IF NOT EXISTS variant(
  store TEXT NOT NULL, product_id TEXT NOT NULL, id TEXT NOT NULL, title TEXT, sku TEXT,
  price REAL, list_price REAL, available INTEGER, options TEXT NOT NULL DEFAULT '[]',
  PRIMARY KEY(store, product_id, id));
CREATE TABLE IF NOT EXISTS price_obs(
  id INTEGER PRIMARY KEY, capture_id INTEGER NOT NULL, store TEXT NOT NULL, product_id TEXT NOT NULL,
  variant_id TEXT NOT NULL DEFAULT '', at TEXT NOT NULL, price REAL, list_price REAL,
  UNIQUE(capture_id, store, product_id, variant_id));
CREATE INDEX IF NOT EXISTS price_obs_product ON price_obs(store, product_id, at);
CREATE TABLE IF NOT EXISTS capture(
  id INTEGER PRIMARY KEY, store TEXT NOT NULL, source TEXT NOT NULL, external_id TEXT,
  started_at TEXT NOT NULL, finished_at TEXT, captured_at TEXT,
  product_count INTEGER NOT NULL DEFAULT 0, complete INTEGER NOT NULL DEFAULT 0, discontinued INTEGER NOT NULL DEFAULT 0,
  note TEXT, sources TEXT NOT NULL DEFAULT '[]');
CREATE TABLE IF NOT EXISTS recipient(
  nick TEXT PRIMARY KEY COLLATE NOCASE, wrist_cm REAL, metal TEXT, systems TEXT NOT NULL DEFAULT '[]', owned TEXT NOT NULL DEFAULT '[]',
  likes TEXT NOT NULL DEFAULT '[]', budget REAL, notes TEXT, updated_at TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS gift_plan(
  id INTEGER PRIMARY KEY, name TEXT NOT NULL, recipient TEXT, budget REAL, items TEXT NOT NULL,
  created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
-- Per host, across runs: when gift-check last asked it for anything (epoch ms), the spacing it
-- used, and a Retry-After the host gave (epoch ms). Every fetch run starts by honouring these.
CREATE TABLE IF NOT EXISTS host_state(host TEXT PRIMARY KEY, last_request_ms INTEGER, gap_ms INTEGER, retry_not_before_ms INTEGER);
";

const COLS: &str = "store, id, name, url, category, kind, kind_given, system, metal, metal_group, metal_group_given, collection, themes, badges, tags, vendor, description, engravable, in_stock, currency, price, list_price, price_at, image, first_seen, last_seen, discontinued, listing, system_inferred";

fn e(err: rusqlite::Error) -> String {
    format!("database: {err}")
}

fn to_json<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "null".into())
}

fn from_json<T: serde::de::DeserializeOwned + Default>(s: Option<String>) -> T {
    s.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn product_from_row(r: &Row) -> rusqlite::Result<Product> {
    Ok(Product {
        store: r.get(0)?,
        id: r.get(1)?,
        name: r.get(2)?,
        url: r.get(3)?,
        category: r.get(4)?,
        kind: r.get(5)?,
        kind_given: r.get::<_, i64>(6)? != 0,
        system: r.get(7)?,
        metal: r.get(8)?,
        metal_group: r.get(9)?,
        metal_group_given: r.get::<_, i64>(10)? != 0,
        collection: r.get(11)?,
        themes: r.get::<_, Option<String>>(12)?.and_then(|s| serde_json::from_str(&s).ok()),
        badges: r.get::<_, Option<String>>(13)?.and_then(|s| serde_json::from_str(&s).ok()),
        tags: from_json(r.get(14)?),
        vendor: r.get(15)?,
        description: r.get(16)?,
        engravable: r.get::<_, Option<i64>>(17)?.map(|v| v != 0),
        in_stock: r.get::<_, Option<i64>>(18)?.map(|v| v != 0),
        currency: r.get(19)?,
        price: r.get(20)?,
        list_price: r.get(21)?,
        price_at: r.get(22)?,
        image: r.get(23)?,
        variants: vec![],
        first_seen: r.get(24)?,
        last_seen: r.get(25)?,
        discontinued: r.get::<_, i64>(26)? != 0,
        listing: r.get(27)?,
        system_inferred: r.get::<_, i64>(28)? != 0,
    })
}

/// A newer capture's values win; what it left unknown is kept from the older row. Lists the new
/// capture stated (even empty ones, like "no badges any more") replace the old ones. Variants are
/// left to the caller: a capture without variants leaves the stored ones alone.
fn carry_over(p: &mut Product, old: Product) {
    macro_rules! keep {
        ($($f:ident),*) => { $( if p.$f.is_none() { p.$f = old.$f; } )* };
    }
    keep!(category, listing, metal, collection, vendor, description, engravable, in_stock, image, themes, badges);
    if !p.kind_given && old.kind_given {
        p.kind = old.kind;
        p.kind_given = true;
    }
    if !p.metal_group_given && old.metal_group_given {
        p.metal_group = old.metal_group;
        p.metal_group_given = true;
    }
    if p.price.is_none() && p.list_price.is_none() {
        p.price = old.price;
        p.list_price = old.list_price;
        p.price_at = old.price_at;
    }
    if p.tags.is_empty() {
        p.tags = old.tags;
    }
}

/// A plan's products with their quantities, and the "store:id" of any no longer stored.
pub type PlanProducts = (Vec<(Product, u32)>, Vec<String>);

/// Which products a finished capture speaks for, when deciding what is discontinued.
#[derive(Debug, Clone, PartialEq)]
pub enum Scope {
    /// Nothing is marked discontinued (partial captures, collection fetches).
    None,
    /// The whole store (a full Shopify catalog fetch that reached the empty page).
    All,
    /// Only products whose stored `listing` is one of these (a browser capture's
    /// `complete_listings`). Categories never scope this: a product's category says nothing about
    /// which listing pages a capture read.
    Listings(Vec<String>),
}

/// A listing loses at most this share of its stored products to one capture; above it, the
/// capture is suspect (a listing that drew half its tiles, a renamed listing) and marks nothing.
pub const MAX_GONE_SHARE: f64 = 0.20;

/// What closing a capture did.
#[derive(Debug, Clone, PartialEq)]
pub struct Finished {
    /// Distinct products the capture stored.
    pub products: i64,
    /// Products newly marked "not in the latest capture".
    pub discontinued: i64,
    /// The note stored on the capture (the caller's, plus why nothing was marked, if so).
    pub note: Option<String>,
    /// Why nothing was marked gone, when the safety limit held it back.
    pub held: Option<String>,
}

/// Saved per host across fetch runs (times in Unix milliseconds).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HostState {
    pub last_request_ms: Option<u64>,
    pub gap_ms: Option<u64>,
    pub retry_not_before_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CaptureRow {
    pub id: i64,
    pub store: String,
    pub source: String,
    pub external_id: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub captured_at: Option<String>,
    pub product_count: i64,
    pub complete: bool,
    pub discontinued: i64,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Observation {
    pub at: String,
    pub price: Option<f64>,
    pub list_price: Option<f64>,
    pub capture_id: i64,
    pub source: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sort {
    #[default]
    Price,
    PriceDesc,
    PctOff,
    Newest,
    Name,
}

impl Sort {
    pub fn parse(s: &str) -> Option<Sort> {
        Some(match s {
            "" | "price" => Sort::Price,
            "price-desc" | "price_desc" => Sort::PriceDesc,
            "off" | "pct-off" => Sort::PctOff,
            "new" | "newest" => Sort::Newest,
            "name" => Sort::Name,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct Query {
    pub words: Vec<String>,
    pub store: Option<String>,
    pub kind: Option<String>,
    /// Any of these kinds (when `kind` is not set).
    pub kinds: Option<Vec<String>>,
    pub metal_group: Option<String>,
    /// Only these systems (for "fits this bracelet").
    pub systems: Option<Vec<String>>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub on_sale: bool,
    pub in_stock: bool,
    pub include_discontinued: bool,
    pub sort: Sort,
    pub limit: usize,
    pub offset: usize,
}

const CUR: &str = "COALESCE(price, list_price)";

pub struct Db {
    conn: Connection,
}

impl Db {
    pub fn open(path: &Path) -> Result<Db, String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|err| format!("can't create {}: {err}", dir.display()))?;
        }
        let conn = Connection::open(path).map_err(|err| format!("can't open {}: {err}", path.display()))?;
        conn.busy_timeout(std::time::Duration::from_secs(10)).map_err(e)?;
        let _: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0)).map_err(e)?;
        Db::init(conn)
    }

    pub fn memory() -> Result<Db, String> {
        Db::init(Connection::open_in_memory().map_err(e)?)
    }

    fn init(conn: Connection) -> Result<Db, String> {
        let v: Option<String> = conn
            .query_row("SELECT value FROM meta WHERE key='schema_version'", [], |r| r.get(0))
            .optional()
            .or_else(|err| if err.to_string().contains("no such table") { Ok(None) } else { Err(err) })
            .map_err(e)?;
        if let Some(Ok(n)) = v.as_deref().map(str::parse::<i64>) {
            if n > SCHEMA_VERSION {
                return Err("this database was made by a newer gift-check; update gift-check".into());
            }
        }
        conn.execute_batch(SCHEMA).map_err(e)?;
        // Version 1 databases lack the product columns added in version 2.
        let cols: Vec<String> = conn.prepare("SELECT name FROM pragma_table_info('product')").and_then(|mut st| st.query_map([], |r| r.get(0))?.collect()).map_err(e)?;
        for (col, ddl) in [("listing", "ALTER TABLE product ADD COLUMN listing TEXT"), ("system_inferred", "ALTER TABLE product ADD COLUMN system_inferred INTEGER NOT NULL DEFAULT 0")] {
            if !cols.iter().any(|c| c == col) {
                conn.execute_batch(ddl).map_err(e)?;
                // Products are re-classified (the inferred mark is new) on the next sync_rules.
                conn.execute("DELETE FROM meta WHERE key='rules_hash'", []).map_err(e)?;
            }
        }
        conn.execute("INSERT INTO meta(key, value) VALUES ('schema_version', ?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [SCHEMA_VERSION.to_string()]).map_err(e)?;
        Ok(Db { conn })
    }

    fn meta(&self, key: &str) -> Result<Option<String>, String> {
        self.conn.query_row("SELECT value FROM meta WHERE key=?1", [key], |r| r.get(0)).optional().map_err(e)
    }

    fn set_meta(&self, key: &str, value: &str) -> Result<(), String> {
        self.conn.execute("INSERT INTO meta(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [key, value]).map_err(e)?;
        Ok(())
    }

    /// Re-applies the rules to every product when the rules file changed since the last time.
    pub fn sync_rules(&self, rules: &Rules, hash: &str) -> Result<usize, String> {
        if self.meta("rules_hash")?.as_deref() == Some(hash) {
            return Ok(0);
        }
        let n = self.reclassify(rules)?;
        self.set_meta("rules_hash", hash)?;
        Ok(n)
    }

    pub fn reclassify(&self, rules: &Rules) -> Result<usize, String> {
        let tx = self.conn.unchecked_transaction().map_err(e)?;
        let all = self.products_where("1=1", &[])?;
        let mut changed = 0;
        for mut p in all {
            let before = (p.kind.clone(), p.metal_group.clone(), p.system.clone(), p.system_inferred);
            rules.classify(&mut p);
            if before != (p.kind.clone(), p.metal_group.clone(), p.system.clone(), p.system_inferred) {
                self.conn
                    .execute(
                        "UPDATE product SET kind=?3, metal_group=?4, system=?5, system_inferred=?6 WHERE store=?1 AND id=?2",
                        params![p.store, p.id, p.kind, p.metal_group, p.system, p.system_inferred],
                    )
                    .map_err(e)?;
                changed += 1;
            }
        }
        tx.commit().map_err(e)?;
        Ok(changed)
    }

    pub fn upsert_store(&self, s: &StoreCfg) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO store(id, name, origin, source, currency) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(id) DO UPDATE SET name=excluded.name, origin=excluded.origin, source=excluded.source, currency=excluded.currency",
                params![s.id, s.name, s.origin, s.source.as_str(), s.currency],
            )
            .map_err(e)?;
        Ok(())
    }

    pub fn begin_capture(&self, store: &str, source: &str, external_id: Option<&str>, captured_at: Option<&str>, sources: &[String]) -> Result<i64, String> {
        self.conn
            .execute(
                "INSERT INTO capture(store, source, external_id, started_at, captured_at, sources) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![store, source, external_id, time::now_utc(), captured_at, to_json(&sources)],
            )
            .map_err(e)?;
        Ok(self.conn.last_insert_rowid())
    }

    fn get_product(&self, store: &str, id: &str) -> Result<Option<(Product, Option<i64>)>, String> {
        self.conn
            .query_row(&format!("SELECT {COLS}, last_capture FROM product WHERE store=?1 AND id=?2"), [store, id], |r| Ok((product_from_row(r)?, r.get(29)?)))
            .optional()
            .map_err(e)
    }

    /// Stores products seen by one capture at time `at`: the product rows are updated, the
    /// variants replaced when the capture lists some, and one price observation per product and
    /// variant is appended (once per capture; a repeat within the same capture only fills gaps).
    /// A capture older than what is stored for a product (its `last_seen` or `price_at`) never
    /// overwrites it: the observation is added to the history and the row only gets gaps filled.
    pub fn store_products(&self, capture_id: i64, at: &str, products: &[Product], rules: &Rules) -> Result<usize, String> {
        let tx = self.conn.unchecked_transaction().map_err(e)?;
        for fresh in products {
            let existing = self.get_product(&fresh.store, &fresh.id)?;
            let mut older = false;
            let mut p = match existing {
                None => {
                    let mut p = fresh.clone();
                    p.first_seen = Some(at.to_string());
                    p.price_at = Some(at.to_string());
                    p
                }
                Some((mut old, last)) if last == Some(capture_id) => {
                    // Seen earlier in this same capture: the first sighting wins.
                    old.variants = self.variants(&old.store, &old.id)?;
                    crate::capture::fill(&mut old, fresh.clone());
                    old
                }
                // Times are all "YYYY-MM-DDTHH:MM:SSZ", so text order is time order.
                Some((mut old, _)) if [&old.last_seen, &old.price_at].into_iter().flatten().any(|t| at < t.as_str()) => {
                    older = true;
                    old.variants = self.variants(&old.store, &old.id)?;
                    let first = old.first_seen.clone();
                    crate::capture::fill(&mut old, fresh.clone());
                    old.first_seen = [first, Some(at.to_string())].into_iter().flatten().min();
                    old
                }
                Some((old, _)) => {
                    let mut p = fresh.clone();
                    p.first_seen = old.first_seen.clone();
                    if p.price.is_some() || p.list_price.is_some() {
                        p.price_at = Some(at.to_string());
                    }
                    carry_over(&mut p, old);
                    p
                }
            };
            if !older {
                p.last_seen = Some(at.to_string());
                p.discontinued = false;
            }
            rules.classify(&mut p);
            self.write_product(&p, capture_id)?;
            if !p.variants.is_empty() {
                self.conn.execute("DELETE FROM variant WHERE store=?1 AND product_id=?2", [&p.store, &p.id]).map_err(e)?;
                for v in &p.variants {
                    self.conn
                        .execute(
                            "INSERT INTO variant(store, product_id, id, title, sku, price, list_price, available, options) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                            params![p.store, p.id, v.id, v.title, v.sku, v.price, v.list_price, v.available, to_json(&v.options)],
                        )
                        .map_err(e)?;
                }
            }
            let obs = "INSERT OR IGNORE INTO price_obs(capture_id, store, product_id, variant_id, at, price, list_price) VALUES (?1,?2,?3,?4,?5,?6,?7)";
            self.conn.execute(obs, params![capture_id, fresh.store, fresh.id, "", at, fresh.price, fresh.list_price]).map_err(e)?;
            for v in &fresh.variants {
                self.conn.execute(obs, params![capture_id, fresh.store, fresh.id, v.id, at, v.price, v.list_price]).map_err(e)?;
            }
        }
        tx.commit().map_err(e)?;
        Ok(products.len())
    }

    fn write_product(&self, p: &Product, capture_id: i64) -> Result<(), String> {
        let opt_json = |v: &Option<Vec<String>>| v.as_ref().map(to_json);
        self.conn
            .execute(
                &format!(
                    "INSERT OR REPLACE INTO product({COLS}, last_capture) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27,?28,?29,?30)"
                ),
                params![
                    p.store,
                    p.id,
                    p.name,
                    p.url,
                    p.category,
                    p.kind,
                    p.kind_given,
                    p.system,
                    p.metal,
                    p.metal_group,
                    p.metal_group_given,
                    p.collection,
                    opt_json(&p.themes),
                    opt_json(&p.badges),
                    to_json(&p.tags),
                    p.vendor,
                    p.description,
                    p.engravable,
                    p.in_stock,
                    p.currency,
                    p.price,
                    p.list_price,
                    p.price_at,
                    p.image,
                    p.first_seen,
                    p.last_seen,
                    p.discontinued,
                    p.listing,
                    p.system_inferred,
                    capture_id
                ],
            )
            .map_err(e)?;
        Ok(())
    }

    /// Closes a capture: counts what it stored and marks the products in its scope that it didn't
    /// see as discontinued. `Scope::All` needs `complete`; `Scope::Listings` names the listings
    /// that were read completely. When any listing would lose more than [`MAX_GONE_SHARE`] of its
    /// stored products, nothing is marked and the capture gets a note saying so.
    pub fn finish_capture(&self, capture_id: i64, complete: bool, scope: &Scope, note: Option<&str>) -> Result<Finished, String> {
        let (store, count): (String, i64) = self
            .conn
            .query_row(
                "SELECT c.store, (SELECT COUNT(*) FROM price_obs o WHERE o.capture_id=c.id AND o.variant_id='') FROM capture c WHERE c.id=?1",
                [capture_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(e)?;
        let mut gone = 0i64;
        let mut notes: Vec<String> = note.map(str::to_string).into_iter().collect();
        let mut held: Option<String> = None;
        let unseen = "discontinued=0 AND store=?1 AND (last_capture IS NULL OR last_capture != ?2)";
        // Nothing seen means nothing to compare against: never mark a whole store gone.
        if count > 0 {
            match scope {
                Scope::None => {}
                Scope::All if complete => {
                    gone += self.conn.execute(&format!("UPDATE product SET discontinued=1 WHERE {unseen}"), params![store, capture_id]).map_err(e)? as i64;
                }
                Scope::All => {}
                Scope::Listings(listings) => {
                    let mut over = Vec::new();
                    for l in listings {
                        let (stored, missing): (i64, i64) = self
                            .conn
                            .query_row(
                                &format!("SELECT COUNT(*), COALESCE(SUM(CASE WHEN {unseen} THEN 1 ELSE 0 END), 0) FROM product WHERE store=?1 AND discontinued=0 AND listing=?3"),
                                params![store, capture_id, l],
                                |r| Ok((r.get(0)?, r.get(1)?)),
                            )
                            .map_err(e)?;
                        if missing as f64 > stored as f64 * MAX_GONE_SHARE {
                            over.push(format!("{missing} of the {stored} stored products of listing {l:?}"));
                        }
                    }
                    if over.is_empty() {
                        for l in listings {
                            gone += self.conn.execute(&format!("UPDATE product SET discontinued=1 WHERE {unseen} AND listing=?3"), params![store, capture_id, l]).map_err(e)? as i64;
                        }
                    } else {
                        held = Some(format!(
                            "nothing marked gone: this capture didn't include {}, more than the {:.0}% one capture may mark gone; check the listing before trusting it",
                            over.join(", "),
                            MAX_GONE_SHARE * 100.0
                        ));
                    }
                }
            }
        }
        notes.extend(held.clone());
        let note = (!notes.is_empty()).then(|| notes.join("; "));
        self.conn
            .execute(
                "UPDATE capture SET finished_at=?2, product_count=?3, complete=?4, discontinued=?5, note=?6 WHERE id=?1",
                params![capture_id, time::now_utc(), count, complete, gone, note],
            )
            .map_err(e)?;
        Ok(Finished { products: count, discontinued: gone, note, held })
    }

    /// Closes every capture of `source` that is still open (a bridge capture whose chunks stopped
    /// arriving when the window was closed) as incomplete. Nothing is marked discontinued.
    pub fn close_open_captures(&self, source: &str, note: &str) -> Result<usize, String> {
        let ids: Vec<i64> = self
            .conn
            .prepare("SELECT id FROM capture WHERE source=?1 AND finished_at IS NULL")
            .and_then(|mut st| st.query_map([source], |r| r.get(0))?.collect())
            .map_err(e)?;
        for id in &ids {
            self.finish_capture(*id, false, &Scope::None, Some(note))?;
        }
        Ok(ids.len())
    }

    pub fn host_state(&self, host: &str) -> Result<HostState, String> {
        let row = self
            .conn
            .query_row("SELECT last_request_ms, gap_ms, retry_not_before_ms FROM host_state WHERE host=?1", [host], |r| {
                let n = |i: usize| r.get::<_, Option<i64>>(i).map(|v| v.map(|v| v.max(0) as u64));
                Ok(HostState { last_request_ms: n(0)?, gap_ms: n(1)?, retry_not_before_ms: n(2)? })
            })
            .optional()
            .map_err(e)?;
        Ok(row.unwrap_or_default())
    }

    /// Records a request to `host` at `at_ms` made with spacing `gap_ms`.
    pub fn note_request(&self, host: &str, at_ms: u64, gap_ms: u64) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO host_state(host, last_request_ms, gap_ms) VALUES (?1, ?2, ?3)
                 ON CONFLICT(host) DO UPDATE SET last_request_ms=MAX(IFNULL(last_request_ms, 0), excluded.last_request_ms), gap_ms=excluded.gap_ms",
                params![host, at_ms as i64, gap_ms as i64],
            )
            .map_err(e)?;
        Ok(())
    }

    /// Records that `host` asked for no requests before `not_before_ms` (a Retry-After).
    pub fn note_retry_after(&self, host: &str, not_before_ms: u64) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO host_state(host, retry_not_before_ms) VALUES (?1, ?2)
                 ON CONFLICT(host) DO UPDATE SET retry_not_before_ms=MAX(IFNULL(retry_not_before_ms, 0), excluded.retry_not_before_ms)",
                params![host, not_before_ms as i64],
            )
            .map_err(e)?;
        Ok(())
    }

    pub fn set_capture_sources(&self, capture_id: i64, sources: &[String]) -> Result<(), String> {
        self.conn.execute("UPDATE capture SET sources=?2 WHERE id=?1", params![capture_id, to_json(&sources)]).map_err(e)?;
        Ok(())
    }

    /// Distinct products a capture has stored so far.
    pub fn capture_count(&self, capture_id: i64) -> Result<i64, String> {
        self.conn.query_row("SELECT COUNT(*) FROM price_obs WHERE capture_id=?1 AND variant_id=''", [capture_id], |r| r.get(0)).map_err(e)
    }

    fn products_where(&self, cond: &str, args: &[rusqlite::types::Value]) -> Result<Vec<Product>, String> {
        let mut st = self.conn.prepare(&format!("SELECT {COLS} FROM product WHERE {cond}")).map_err(e)?;
        let rows = st.query_map(params_from_iter(args.iter()), product_from_row).map_err(e)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(e)
    }

    pub fn variants(&self, store: &str, id: &str) -> Result<Vec<Variant>, String> {
        let mut st = self.conn.prepare("SELECT id, title, sku, price, list_price, available, options FROM variant WHERE store=?1 AND product_id=?2 ORDER BY rowid").map_err(e)?;
        let rows = st
            .query_map([store, id], |r| {
                Ok(Variant {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    sku: r.get(2)?,
                    price: r.get(3)?,
                    list_price: r.get(4)?,
                    available: r.get::<_, Option<i64>>(5)?.map(|v| v != 0),
                    options: from_json(r.get(6)?),
                })
            })
            .map_err(e)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(e)
    }

    /// One product with its variants.
    pub fn product(&self, store: &str, id: &str) -> Result<Option<Product>, String> {
        let Some((mut p, _)) = self.get_product(store, id)? else { return Ok(None) };
        p.variants = self.variants(store, id)?;
        Ok(Some(p))
    }

    /// Product-level price history, oldest first.
    pub fn history(&self, store: &str, id: &str) -> Result<Vec<Observation>, String> {
        let mut st = self
            .conn
            .prepare("SELECT o.at, o.price, o.list_price, o.capture_id, c.source FROM price_obs o LEFT JOIN capture c ON c.id=o.capture_id WHERE o.store=?1 AND o.product_id=?2 AND o.variant_id='' ORDER BY o.at, o.id")
            .map_err(e)?;
        let rows = st.query_map([store, id], |r| Ok(Observation { at: r.get(0)?, price: r.get(1)?, list_price: r.get(2)?, capture_id: r.get(3)?, source: r.get(4)? })).map_err(e)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(e)
    }

    /// Search results and the number of matches before `limit`.
    pub fn search(&self, q: &Query) -> Result<(Vec<Product>, i64), String> {
        use rusqlite::types::Value as V;
        let mut cond = String::from("1=1");
        let mut args: Vec<V> = Vec::new();
        for w in q.words.iter().map(|w| w.trim()).filter(|w| !w.is_empty()).take(12) {
            let pat = format!("%{}%", crate::text::like_escape(w));
            cond.push_str(
                " AND (name LIKE ? ESCAPE '\\' OR IFNULL(collection,'') LIKE ? ESCAPE '\\' OR IFNULL(themes,'') LIKE ? ESCAPE '\\' OR tags LIKE ? ESCAPE '\\' OR IFNULL(category,'') LIKE ? ESCAPE '\\' OR IFNULL(metal,'') LIKE ? ESCAPE '\\' OR id = ?)",
            );
            for _ in 0..6 {
                args.push(V::Text(pat.clone()));
            }
            args.push(V::Text(w.to_string()));
        }
        for (col, val) in [("store", &q.store), ("kind", &q.kind), ("metal_group", &q.metal_group)] {
            if let Some(v) = val.as_ref().filter(|v| !v.is_empty()) {
                cond.push_str(&format!(" AND {col} = ? COLLATE NOCASE"));
                args.push(V::Text(v.clone()));
            }
        }
        if let Some(kinds) = q.kinds.as_ref().filter(|_| q.kind.is_none()) {
            if kinds.is_empty() {
                cond.push_str(" AND 0");
            } else {
                cond.push_str(&format!(" AND kind IN ({})", vec!["?"; kinds.len()].join(",")));
                args.extend(kinds.iter().map(|k| V::Text(k.clone())));
            }
        }
        if let Some(systems) = &q.systems {
            if systems.is_empty() {
                cond.push_str(" AND 0");
            } else {
                cond.push_str(&format!(" AND system IN ({})", vec!["?"; systems.len()].join(",")));
                args.extend(systems.iter().map(|s| V::Text(s.clone())));
            }
        }
        if let Some(min) = q.min {
            cond.push_str(&format!(" AND {CUR} >= ?"));
            args.push(V::Real(min));
        }
        if let Some(max) = q.max {
            cond.push_str(&format!(" AND {CUR} <= ?"));
            args.push(V::Real(max));
        }
        if q.on_sale {
            cond.push_str(&format!(" AND list_price - {CUR} >= 0.005"));
        }
        if q.in_stock {
            cond.push_str(" AND in_stock IS NOT 0");
        }
        if !q.include_discontinued {
            cond.push_str(" AND discontinued = 0");
        }
        let total: i64 = self.conn.query_row(&format!("SELECT COUNT(*) FROM product WHERE {cond}"), params_from_iter(args.iter()), |r| r.get(0)).map_err(e)?;
        let pct = format!("CASE WHEN list_price > 0 AND list_price - {CUR} >= 0.005 THEN (list_price - {CUR}) / list_price ELSE 0 END");
        let order = match q.sort {
            Sort::Price => format!("{CUR} IS NULL, {CUR} ASC, name"),
            Sort::PriceDesc => format!("{CUR} IS NULL, {CUR} DESC, name"),
            Sort::PctOff => format!("{pct} DESC, {CUR} ASC, name"),
            Sort::Newest => "first_seen DESC, name".into(),
            Sort::Name => "name COLLATE NOCASE, store".into(),
        };
        let limit = if q.limit == 0 { 100 } else { q.limit.min(1000) };
        let rows = self.products_where(&format!("{cond} ORDER BY {order} LIMIT {limit} OFFSET {}", q.offset.min(1_000_000)), &args)?;
        Ok((rows, total))
    }

    /// Everything not discontinued (optionally one store), for the planner.
    pub fn catalog(&self, store: Option<&str>) -> Result<Vec<Product>, String> {
        match store {
            Some(s) => self.products_where("discontinued = 0 AND store = ?", &[rusqlite::types::Value::Text(s.to_string())]),
            None => self.products_where("discontinued = 0", &[]),
        }
    }

    /// Price stats per kind for each store (`by = "store"`) or category (`by = "category"`).
    /// Prices in different currencies are never compared: with more than one currency stored,
    /// `currency` must pick one.
    pub fn compare(&self, by: &str, currency: Option<&str>) -> Result<Value, String> {
        let col = match by {
            "store" => "store",
            "category" => "IFNULL(category, '(none)')",
            _ => return Err("compare by store or category".into()),
        };
        let currencies: Vec<String> = self
            .conn
            .prepare(&format!("SELECT DISTINCT currency FROM product WHERE discontinued = 0 AND {CUR} IS NOT NULL ORDER BY 1"))
            .and_then(|mut st| st.query_map([], |r| r.get(0))?.collect())
            .map_err(e)?;
        let currency = match currency {
            Some(c) => c.to_string(),
            None if currencies.len() > 1 => return Err(format!("prices are stored in {}; compare one currency at a time", currencies.join(" and "))),
            None => currencies.first().cloned().unwrap_or_else(|| "USD".into()),
        };
        let mut st = self
            .conn
            .prepare(&format!("SELECT {col}, IFNULL(kind, '(unknown)'), {CUR}, list_price FROM product WHERE discontinued = 0 AND {CUR} IS NOT NULL AND currency = ?1 ORDER BY 1, 2"))
            .map_err(e)?;
        let rows = st.query_map([&currency], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, f64>(2)?, r.get::<_, Option<f64>>(3)?))).map_err(e)?;
        let mut cells: std::collections::BTreeMap<(String, String), Vec<(f64, bool)>> = Default::default();
        for row in rows {
            let (g, k, p, l) = row.map_err(e)?;
            cells.entry((g, k)).or_default().push((p, l.is_some_and(|l| l - p >= 0.005)));
        }
        let mut groups: Vec<String> = cells.keys().map(|(g, _)| g.clone()).collect();
        groups.dedup();
        let mut kinds: Vec<String> = cells.keys().map(|(_, k)| k.clone()).collect();
        kinds.sort();
        kinds.dedup();
        let stats: Vec<Value> = cells
            .into_iter()
            .map(|((g, k), mut v)| {
                v.sort_by(|a, b| a.0.total_cmp(&b.0));
                let n = v.len();
                let median = if n % 2 == 1 { v[n / 2].0 } else { (v[n / 2 - 1].0 + v[n / 2].0) / 2.0 };
                let sale = v.iter().filter(|x| x.1).count();
                json!({ "group": g, "kind": k, "count": n, "min": v[0].0, "median": (median * 100.0).round() / 100.0, "max": v[n - 1].0, "on_sale": sale, "share_on_sale": (sale as f64 / n as f64 * 1000.0).round() / 1000.0 })
            })
            .collect();
        Ok(json!({ "by": by, "currency": currency, "groups": groups, "kinds": kinds, "stats": stats }))
    }

    pub fn captures(&self, limit: usize) -> Result<Vec<CaptureRow>, String> {
        let mut st = self
            .conn
            .prepare("SELECT id, store, source, external_id, started_at, finished_at, captured_at, product_count, complete, discontinued, note FROM capture ORDER BY id DESC LIMIT ?1")
            .map_err(e)?;
        let rows = st
            .query_map([limit as i64], |r| {
                Ok(CaptureRow {
                    id: r.get(0)?,
                    store: r.get(1)?,
                    source: r.get(2)?,
                    external_id: r.get(3)?,
                    started_at: r.get(4)?,
                    finished_at: r.get(5)?,
                    captured_at: r.get(6)?,
                    product_count: r.get(7)?,
                    complete: r.get::<_, i64>(8)? != 0,
                    discontinued: r.get(9)?,
                    note: r.get(10)?,
                })
            })
            .map_err(e)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(e)
    }

    /// Per store: products, discontinued, on sale, and the last finished capture time.
    pub fn store_counts(&self) -> Result<Vec<Value>, String> {
        let mut st = self
            .conn
            .prepare(&format!(
                "SELECT p.store, COUNT(*), SUM(p.discontinued), SUM(CASE WHEN p.discontinued=0 AND list_price - {CUR} >= 0.005 THEN 1 ELSE 0 END),
                 (SELECT MAX(COALESCE(c.finished_at, c.started_at)) FROM capture c WHERE c.store=p.store)
                 FROM product p GROUP BY p.store ORDER BY p.store"
            ))
            .map_err(e)?;
        let rows = st
            .query_map([], |r| {
                Ok(json!({ "store": r.get::<_, String>(0)?, "products": r.get::<_, i64>(1)?, "discontinued": r.get::<_, i64>(2)?, "on_sale": r.get::<_, i64>(3)?, "last_capture": r.get::<_, Option<String>>(4)? }))
            })
            .map_err(e)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(e)
    }

    pub fn metal_groups(&self) -> Result<Vec<String>, String> {
        let mut st = self.conn.prepare("SELECT DISTINCT metal_group FROM product WHERE metal_group IS NOT NULL ORDER BY 1").map_err(e)?;
        let rows = st.query_map([], |r| r.get::<_, String>(0)).map_err(e)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(e)
    }

    pub fn save_recipient(&self, r: &Recipient) -> Result<(), String> {
        let mut r = r.clone();
        r.validate()?;
        self.conn
            .execute(
                "INSERT INTO recipient(nick, wrist_cm, metal, systems, owned, likes, budget, notes, updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)
                 ON CONFLICT(nick) DO UPDATE SET wrist_cm=excluded.wrist_cm, metal=excluded.metal, systems=excluded.systems, owned=excluded.owned,
                 likes=excluded.likes, budget=excluded.budget, notes=excluded.notes, updated_at=excluded.updated_at",
                params![r.nick, r.wrist_cm, r.metal, to_json(&r.systems), to_json(&r.owned), to_json(&r.likes), r.budget, r.notes, time::now_utc()],
            )
            .map_err(e)?;
        Ok(())
    }

    pub fn recipients(&self) -> Result<Vec<Recipient>, String> {
        let mut st = self.conn.prepare("SELECT nick, wrist_cm, metal, systems, owned, likes, budget, notes, updated_at FROM recipient ORDER BY nick COLLATE NOCASE").map_err(e)?;
        let rows = st
            .query_map([], |r| {
                Ok(Recipient {
                    nick: r.get(0)?,
                    wrist_cm: r.get(1)?,
                    metal: r.get(2)?,
                    systems: from_json(r.get(3)?),
                    owned: from_json(r.get(4)?),
                    likes: from_json(r.get(5)?),
                    budget: r.get(6)?,
                    notes: r.get(7)?,
                    updated_at: r.get(8)?,
                })
            })
            .map_err(e)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(e)
    }

    pub fn recipient(&self, nick: &str) -> Result<Option<Recipient>, String> {
        Ok(self.recipients()?.into_iter().find(|r| r.nick.eq_ignore_ascii_case(nick.trim())))
    }

    /// Saves a plan with a snapshot of each item's current price and when that price was seen.
    /// A plan mixing currencies is refused.
    pub fn save_plan(&self, plan: &GiftPlan) -> Result<i64, String> {
        let mut p = plan.clone();
        p.validate()?;
        let mut currencies: Vec<String> = Vec::new();
        for it in &mut p.items {
            let found = self.get_product(&it.store, &it.id)?.map(|(prod, _)| prod);
            it.saved_price = found.as_ref().and_then(Product::current);
            it.saved_price_at = found.as_ref().and_then(|x| x.price_at.clone());
            if let Some(c) = found.map(|x| x.currency) {
                if !currencies.contains(&c) {
                    currencies.push(c);
                }
            }
        }
        if currencies.len() > 1 {
            return Err(format!("a plan can't mix currencies ({}); keep one store's currency per plan", currencies.join(" and ")));
        }
        let now = time::now_utc();
        match p.id {
            Some(id) => {
                let n = self
                    .conn
                    .execute("UPDATE gift_plan SET name=?2, recipient=?3, budget=?4, items=?5, updated_at=?6 WHERE id=?1", params![id, p.name, p.recipient, p.budget, to_json(&p.items), now])
                    .map_err(e)?;
                if n == 0 {
                    return Err(format!("no saved plan {id}"));
                }
                Ok(id)
            }
            None => {
                self.conn
                    .execute("INSERT INTO gift_plan(name, recipient, budget, items, created_at, updated_at) VALUES (?1,?2,?3,?4,?5,?5)", params![p.name, p.recipient, p.budget, to_json(&p.items), now])
                    .map_err(e)?;
                Ok(self.conn.last_insert_rowid())
            }
        }
    }

    pub fn plans(&self) -> Result<Vec<GiftPlan>, String> {
        let mut st = self.conn.prepare("SELECT id, name, recipient, budget, items, created_at, updated_at FROM gift_plan ORDER BY updated_at DESC, id DESC").map_err(e)?;
        let rows = st
            .query_map([], |r| {
                Ok(GiftPlan {
                    id: Some(r.get(0)?),
                    name: r.get(1)?,
                    recipient: r.get(2)?,
                    budget: r.get(3)?,
                    items: from_json::<Vec<PlanItem>>(r.get(4)?),
                    created_at: r.get(5)?,
                    updated_at: r.get(6)?,
                })
            })
            .map_err(e)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(e)
    }

    pub fn delete_plan(&self, id: i64) -> Result<bool, String> {
        Ok(self.conn.execute("DELETE FROM gift_plan WHERE id=?1", [id]).map_err(e)? > 0)
    }

    /// The products a plan names, with variants, in plan order (missing ones skipped).
    pub fn plan_products(&self, items: &[PlanItem]) -> Result<PlanProducts, String> {
        let mut found = Vec::new();
        let mut missing = Vec::new();
        for it in items {
            match self.product(&it.store, &it.id)? {
                Some(p) => found.push((p, it.qty.max(1))),
                None => missing.push(format!("{}:{}", it.store, it.id)),
            }
        }
        Ok((found, missing))
    }

    /// Everything, for agents: stores, products with variants, recipients, plans, captures.
    pub fn export(&self) -> Result<Value, String> {
        let mut products = self.products_where("1=1 ORDER BY store, id", &[])?;
        for p in &mut products {
            p.variants = self.variants(&p.store, &p.id)?;
        }
        let mut st = self.conn.prepare("SELECT id, name, origin, source, currency FROM store ORDER BY id").map_err(e)?;
        let stores = st
            .query_map([], |r| Ok(json!({ "id": r.get::<_, String>(0)?, "name": r.get::<_, String>(1)?, "origin": r.get::<_, String>(2)?, "source": r.get::<_, String>(3)?, "currency": r.get::<_, String>(4)? })))
            .map_err(e)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(e)?;
        Ok(json!({
            "gift_check": crate::VERSION,
            "exported_at": time::now_utc(),
            "stores": stores,
            "products": products,
            "recipients": self.recipients()?,
            "plans": self.plans()?,
            "captures": self.captures(10_000)?,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::tests::rules;

    fn charm(id: &str, name: &str, price: f64, list: f64) -> Product {
        Product {
            store: "pandora-us".into(),
            id: id.into(),
            name: name.into(),
            url: format!("https://us.pandora.net/en/{id}.html"),
            category: Some("charms".into()),
            currency: "USD".into(),
            price: Some(price),
            list_price: Some(list),
            ..Default::default()
        }
    }

    #[test]
    fn price_history_appends_rather_than_overwrites() {
        let db = Db::memory().unwrap();
        let r = rules();
        let c1 = db.begin_capture("pandora-us", "import", None, None, &[]).unwrap();
        db.store_products(c1, "2026-11-01T10:00:00Z", &[charm("794948C01", "Lucky Cat Charm", 98.0, 98.0)], &r).unwrap();
        db.finish_capture(c1, false, &Scope::None, None).unwrap();
        let c2 = db.begin_capture("pandora-us", "import", None, None, &[]).unwrap();
        db.store_products(c2, "2026-11-15T10:00:00Z", &[charm("794948C01", "Lucky Cat Charm", 68.0, 98.0)], &r).unwrap();
        db.finish_capture(c2, false, &Scope::None, None).unwrap();

        let h = db.history("pandora-us", "794948C01").unwrap();
        assert_eq!(h.len(), 2, "{h:?}");
        assert_eq!((h[0].price, h[1].price), (Some(98.0), Some(68.0)));
        let p = db.product("pandora-us", "794948C01").unwrap().unwrap();
        assert_eq!(p.price, Some(68.0));
        assert_eq!(p.first_seen.as_deref(), Some("2026-11-01T10:00:00Z"));
        assert_eq!(p.price_at.as_deref(), Some("2026-11-15T10:00:00Z"));
        assert_eq!(p.kind.as_deref(), Some("charm"), "classified by the rules on the way in");
        assert_eq!(p.system.as_deref(), Some("pandora-moments"));
    }

    #[test]
    fn a_repeat_within_one_capture_keeps_the_first_and_one_observation() {
        let db = Db::memory().unwrap();
        let c = db.begin_capture("pandora-us", "bridge", Some("abc"), None, &[]).unwrap();
        db.store_products(c, "2026-11-15T10:00:00Z", &[charm("1", "A Charm", 50.0, 60.0)], &rules()).unwrap();
        let mut again = charm("1", "A Charm", 40.0, 60.0);
        again.collection = Some("Disney".into());
        db.store_products(c, "2026-11-15T10:00:00Z", &[again], &rules()).unwrap();
        let p = db.product("pandora-us", "1").unwrap().unwrap();
        assert_eq!(p.price, Some(50.0));
        assert_eq!(p.collection.as_deref(), Some("Disney"));
        assert_eq!(db.history("pandora-us", "1").unwrap().len(), 1);
        assert_eq!(db.capture_count(c).unwrap(), 1);
    }

    fn listed(id: &str, listing: &str, category: &str) -> Product {
        let mut p = charm(id, &format!("Charm {id}"), 50.0, 50.0);
        p.listing = Some(listing.into());
        p.category = Some(category.into());
        p
    }

    #[test]
    fn complete_listings_mark_unseen_products_of_those_listings_only() {
        let db = Db::memory().unwrap();
        let r = rules();
        let run = |at: &str, products: &[Product], scope: Scope| {
            let c = db.begin_capture("pandora-us", "import", None, None, &[]).unwrap();
            db.store_products(c, at, products, &r).unwrap();
            db.finish_capture(c, true, &scope, None).unwrap()
        };
        let charms: Vec<Product> = (1..=6).map(|i| listed(&format!("79000{i}"), "charms", "charms")).collect();
        // A bracelet read from the bracelets listing whose structured category says "charms".
        let bracelet = listed("590702", "bracelets", "charms");
        let mut all = charms.clone();
        all.push(bracelet.clone());
        run("2026-11-01T10:00:00Z", &all, Scope::Listings(vec!["charms".into(), "bracelets".into()]));

        // A complete read of the bracelets listing says nothing about the charms listing, whatever
        // the categories say (the old category scope marked every charm gone here).
        let f = run("2026-11-02T10:00:00Z", std::slice::from_ref(&bracelet), Scope::Listings(vec!["bracelets".into()]));
        assert_eq!((f.products, f.discontinued), (1, 0));
        assert!(charms.iter().all(|c| !db.product("pandora-us", &c.id).unwrap().unwrap().discontinued));

        // A partial capture marks nothing.
        let c = db.begin_capture("pandora-us", "import", None, None, &[]).unwrap();
        db.store_products(c, "2026-11-03T10:00:00Z", &charms[..1], &r).unwrap();
        assert_eq!(db.finish_capture(c, false, &Scope::None, None).unwrap().discontinued, 0);

        // A complete charms listing without one of its six charms (17%): that charm is gone.
        let f = run("2026-11-04T10:00:00Z", &charms[..5], Scope::Listings(vec!["charms".into()]));
        assert_eq!((f.products, f.discontinued, f.note, f.held), (5, 1, None, None));
        assert!(db.product("pandora-us", "790006").unwrap().unwrap().discontinued);
        assert!(!db.product("pandora-us", "590702").unwrap().unwrap().discontinued);
        // Seen again later, it comes back.
        run("2026-11-05T10:00:00Z", &charms, Scope::None);
        assert!(!db.product("pandora-us", "790006").unwrap().unwrap().discontinued);

        // Missing two of six (33%) is more than one capture may hide: nothing marked, a note instead.
        let f = run("2026-11-06T10:00:00Z", &charms[..4], Scope::Listings(vec!["charms".into()]));
        assert_eq!(f.discontinued, 0);
        assert!(f.held.as_deref().unwrap().contains("2 of the 6 stored products of listing \"charms\""), "{:?}", f.held);
        assert_eq!(db.captures(1).unwrap()[0].note, f.note, "the note is on the capture");
        assert!(charms.iter().all(|c| !db.product("pandora-us", &c.id).unwrap().unwrap().discontinued));
    }

    #[test]
    fn an_older_capture_adds_history_but_never_overwrites_newer_values() {
        let db = Db::memory().unwrap();
        let r = rules();
        let c1 = db.begin_capture("pandora-us", "import", None, None, &[]).unwrap();
        db.store_products(c1, "2026-11-15T16:00:00Z", &[charm("794948C01", "Lucky Cat Charm", 68.0, 98.0)], &r).unwrap();
        let c2 = db.begin_capture("pandora-us", "import", None, None, &[]).unwrap();
        let mut old = charm("794948C01", "Lucky Cat Charm (old name)", 98.0, 98.0);
        old.collection = Some("Disney".into());
        db.store_products(c2, "2026-10-20T16:00:00Z", &[old], &r).unwrap();
        let p = db.product("pandora-us", "794948C01").unwrap().unwrap();
        assert_eq!((p.price, p.price_at.as_deref(), p.last_seen.as_deref()), (Some(68.0), Some("2026-11-15T16:00:00Z"), Some("2026-11-15T16:00:00Z")));
        assert_eq!(p.name, "Lucky Cat Charm");
        assert_eq!(p.collection.as_deref(), Some("Disney"), "gaps are still filled");
        assert_eq!(p.first_seen.as_deref(), Some("2026-10-20T16:00:00Z"));
        let h = db.history("pandora-us", "794948C01").unwrap();
        assert_eq!(h.iter().map(|o| o.price).collect::<Vec<_>>(), vec![Some(98.0), Some(68.0)], "history is in time order");
    }

    #[test]
    fn plans_and_comparisons_never_mix_currencies() {
        let db = Db::memory().unwrap();
        let c = db.begin_capture("pandora-us", "import", None, None, &[]).unwrap();
        let mut euro = charm("2", "Euro Charm", 20.0, 20.0);
        euro.store = "eu-shop".into();
        euro.currency = "EUR".into();
        db.store_products(c, "2026-11-15T10:00:00Z", &[charm("1", "A Charm", 50.0, 50.0), euro], &rules()).unwrap();
        let err = db.compare("store", None).unwrap_err();
        assert!(err.contains("EUR and USD"), "{err}");
        assert_eq!(db.compare("store", Some("EUR")).unwrap()["stats"][0]["group"], "eu-shop");
        let item = |store: &str, id: &str| PlanItem { store: store.into(), id: id.into(), qty: 1, ..Default::default() };
        let mixed = GiftPlan { name: "Mixed".into(), items: vec![item("pandora-us", "1"), item("eu-shop", "2")], ..Default::default() };
        assert!(db.save_plan(&mixed).unwrap_err().contains("can't mix currencies"));
        let one = GiftPlan { name: "One".into(), items: vec![item("pandora-us", "1")], ..Default::default() };
        db.save_plan(&one).unwrap();
        assert_eq!(db.plans().unwrap()[0].items[0].saved_price, Some(50.0), "the price is kept with the plan");
    }

    #[test]
    fn host_state_keeps_the_latest_request_and_retry_time() {
        let db = Db::memory().unwrap();
        assert_eq!(db.host_state("a.example").unwrap(), HostState::default());
        db.note_request("a.example", 5_000, 3_000).unwrap();
        db.note_request("a.example", 4_000, 3_000).unwrap();
        db.note_retry_after("a.example", 90_000).unwrap();
        db.note_retry_after("a.example", 60_000).unwrap();
        assert_eq!(db.host_state("a.example").unwrap(), HostState { last_request_ms: Some(5_000), gap_ms: Some(3_000), retry_not_before_ms: Some(90_000) });
    }

    #[test]
    fn a_version_1_database_is_upgraded() {
        let conn = Connection::open_in_memory().unwrap();
        let v1 = SCHEMA.replace("  listing TEXT, system_inferred INTEGER NOT NULL DEFAULT 0,\n", "");
        conn.execute_batch(&v1).unwrap();
        conn.execute("INSERT INTO meta(key, value) VALUES ('schema_version', '1'), ('rules_hash', 'old')", []).unwrap();
        let db = Db::init(conn).unwrap();
        assert_eq!(db.meta("schema_version").unwrap().as_deref(), Some("2"));
        assert_eq!(db.meta("rules_hash").unwrap(), None, "products get re-classified");
        let c = db.begin_capture("pandora-us", "import", None, None, &[]).unwrap();
        db.store_products(c, "2026-11-15T10:00:00Z", &[listed("790001", "charms", "charms")], &rules()).unwrap();
        assert_eq!(db.product("pandora-us", "790001").unwrap().unwrap().listing.as_deref(), Some("charms"));
    }

    #[test]
    fn search_filters_and_sorts() {
        let db = Db::memory().unwrap();
        let r = rules();
        let c = db.begin_capture("pandora-us", "import", None, None, &[]).unwrap();
        let mut out = charm("3", "Out Charm", 10.0, 10.0);
        out.in_stock = Some(false);
        db.store_products(c, "2026-11-15T10:00:00Z", &[charm("1", "Cat Charm", 68.0, 98.0), charm("2", "Dog 50%_off Charm", 40.0, 40.0), out], &r).unwrap();
        let q = |f: &dyn Fn(&mut Query)| {
            let mut q = Query::default();
            f(&mut q);
            db.search(&q).unwrap().0.into_iter().map(|p| p.id).collect::<Vec<_>>()
        };
        assert_eq!(q(&|_| {}), vec!["3", "2", "1"]);
        assert_eq!(q(&|q| q.on_sale = true), vec!["1"]);
        assert_eq!(q(&|q| q.in_stock = true), vec!["2", "1"]);
        assert_eq!(q(&|q| q.words = vec!["cat".into()]), vec!["1"]);
        assert_eq!(q(&|q| q.words = vec!["c_t".into()]), Vec::<String>::new(), "LIKE wildcards are escaped");
        assert_eq!(q(&|q| q.words = vec!["50%_off".into()]), vec!["2"]);
        assert_eq!(q(&|q| q.max = Some(50.0)), vec!["3", "2"]);
        assert_eq!(q(&|q| q.sort = Sort::PctOff), vec!["1", "3", "2"]);
        assert_eq!(q(&|q| q.systems = Some(vec![])), Vec::<String>::new());
    }

    #[test]
    fn recipients_and_plans_round_trip() {
        let db = Db::memory().unwrap();
        db.save_recipient(&Recipient { nick: "Mom".into(), likes: vec!["cats".into()], budget: Some(150.0), ..Default::default() }).unwrap();
        db.save_recipient(&Recipient { nick: "mom".into(), likes: vec!["dogs".into()], ..Default::default() }).unwrap();
        let all = db.recipients().unwrap();
        assert_eq!(all.len(), 1, "nicknames are case-insensitive: an edit, not a second person");
        assert_eq!(all[0].likes, vec!["dogs"]);
        let id = db.save_plan(&GiftPlan { name: "Xmas".into(), items: vec![PlanItem { store: "pandora-us".into(), id: "1".into(), qty: 1, ..Default::default() }], ..Default::default() }).unwrap();
        assert_eq!(db.plans().unwrap()[0].id, Some(id));
        assert!(db.delete_plan(id).unwrap());
        assert!(db.plans().unwrap().is_empty());
    }

    #[test]
    fn rules_changes_reclassify_once() {
        let db = Db::memory().unwrap();
        let c = db.begin_capture("pandora-us", "import", None, None, &[]).unwrap();
        db.store_products(c, "2026-11-15T10:00:00Z", &[charm("594595C00", "Mystery", 50.0, 50.0)], &Rules::default()).unwrap();
        assert_eq!(db.product("pandora-us", "594595C00").unwrap().unwrap().kind, None);
        assert_eq!(db.sync_rules(&rules(), "h1").unwrap(), 1);
        assert_eq!(db.product("pandora-us", "594595C00").unwrap().unwrap().metal_group.as_deref(), Some("Silver"));
        assert_eq!(db.sync_rules(&rules(), "h1").unwrap(), 0, "same rules: nothing to do");
    }
}
