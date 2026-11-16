//! Capture files (schema 1): product listings read in a real browser for stores that block plain
//! HTTP. `gift-check import` and the bridge both go through [`validate`], so a capture is checked
//! the same way whichever road it took. See docs/CAPTURE.md for the format.

use crate::model::{Product, Variant, KINDS};
use crate::stores::{Stores, StoreCfg};
use crate::text::clean;
use serde::Deserialize;
use serde_json::Value;

pub const SCHEMA: u64 = 1;
pub const MAX_FILE_BYTES: u64 = 50 * 1024 * 1024;
/// One bridge message (one chunk).
pub const MAX_CHUNK_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_PRODUCTS: usize = 50_000;
pub const MAX_CHUNK_PRODUCTS: usize = 5_000;
pub const MAX_CHUNKS: u32 = 500;
pub const MAX_PRICE: f64 = 1_000_000.0;
pub const MESSAGE_TYPE: &str = "gift-check-capture";

/// A validated capture (or one chunk of one).
#[derive(Debug, Clone, PartialEq)]
pub struct Capture {
    pub store: String,
    pub origin: String,
    pub captured_at: String,
    pub complete: bool,
    pub sources: Vec<String>,
    pub products: Vec<Product>,
    /// Products that appeared more than once and were merged into one.
    pub merged: usize,
    pub warnings: Vec<String>,
    pub chunk: Option<Chunk>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub capture_id: String,
    pub chunk: u32,
    pub chunks: u32,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum IdValue {
    Text(String),
    Number(u64),
}

impl IdValue {
    fn text(self) -> String {
        match self {
            IdValue::Text(s) => s.trim().to_string(),
            IdValue::Number(n) => n.to_string(),
        }
    }
}

#[derive(Deserialize)]
struct RawVariant {
    id: IdValue,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    sku: Option<String>,
    #[serde(default)]
    price: Option<f64>,
    #[serde(default)]
    list_price: Option<f64>,
    #[serde(default)]
    available: Option<bool>,
}

#[derive(Deserialize)]
struct RawProduct {
    id: IdValue,
    name: String,
    url: String,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    metal: Option<String>,
    #[serde(default)]
    metal_group: Option<String>,
    #[serde(default)]
    collection: Option<String>,
    #[serde(default)]
    themes: Option<Vec<String>>,
    #[serde(default)]
    badges: Option<Vec<String>>,
    #[serde(default)]
    engravable: Option<bool>,
    #[serde(default)]
    in_stock: Option<bool>,
    #[serde(default)]
    currency: Option<String>,
    #[serde(default)]
    list_price: Option<f64>,
    #[serde(default)]
    price: Option<f64>,
    #[serde(default)]
    image: Option<String>,
    #[serde(default)]
    variants: Option<Vec<RawVariant>>,
}

pub fn valid_product_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Parses and validates a whole capture file's text.
pub fn validate_text(text: &str, stores: &Stores) -> Result<Capture, String> {
    if text.len() as u64 > MAX_FILE_BYTES {
        return Err(format!("the capture is {} MB; the limit is 50 MB", text.len() / (1024 * 1024)));
    }
    let v: Value = serde_json::from_str(text.trim_start_matches('\u{feff}'))
        .map_err(|e| format!("the capture isn't valid JSON ({e}). NaN, Infinity and comments aren't allowed in JSON."))?;
    let c = validate_value(&v, stores, MAX_PRODUCTS)?;
    if c.chunk.as_ref().is_some_and(|k| k.chunks > 1) {
        return Err("this is one chunk of a bridge capture; import needs the whole capture in one file".into());
    }
    Ok(Capture { chunk: None, ..c })
}

/// Validates a parsed capture or bridge chunk. `max_products` bounds this one message.
pub fn validate_value(v: &Value, stores: &Stores, max_products: usize) -> Result<Capture, String> {
    let obj = v.as_object().ok_or("the capture must be a JSON object")?;
    match obj.get("schema") {
        None => return Err("the capture has no \"schema\" field; this gift-check reads schema 1".into()),
        Some(s) if s.as_u64() == Some(SCHEMA) => {}
        Some(s) => return Err(format!("schema {s} isn't supported; this gift-check reads schema 1")),
    }
    if let Some(t) = obj.get("type") {
        if t.as_str() != Some(MESSAGE_TYPE) {
            return Err(format!("\"type\" must be \"{MESSAGE_TYPE}\""));
        }
    }
    let field = |k: &str| obj.get(k).and_then(Value::as_str).map(str::trim);
    let store_id = field("store").ok_or("the capture needs \"store\" (a store id from data/stores.toml)")?;
    let store = stores.get(store_id).ok_or_else(|| {
        let known: Vec<&str> = stores.stores.iter().map(|s| s.id.as_str()).collect();
        format!("unknown store {store_id:?}; known stores: {}", known.join(", "))
    })?;
    let origin = field("origin").ok_or("the capture needs \"origin\"")?;
    if origin.trim_end_matches('/') != store.origin {
        return Err(format!("origin {origin:?} doesn't match {}'s origin {}", store.id, store.origin));
    }
    let captured_at = field("captured_at").ok_or("the capture needs \"captured_at\" (like 2026-11-15T16:00:00Z)")?;
    let at = crate::time::parse_rfc3339(captured_at).ok_or_else(|| format!("captured_at {captured_at:?} isn't an RFC 3339 time like 2026-11-15T16:00:00Z"))?;
    if at > crate::time::now() + 86_400 {
        return Err(format!("captured_at {captured_at} is in the future"));
    }
    let complete = match obj.get("complete") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err("\"complete\" must be true or false".into()),
    };
    let chunk = chunk_of(obj)?;

    let sources = match obj.get("sources") {
        None | Some(Value::Null) => vec![],
        Some(Value::Array(a)) => {
            if a.len() > 1000 {
                return Err("at most 1000 sources".into());
            }
            let mut out = Vec::new();
            for s in a {
                let s = s.as_str().ok_or("each source must be a URL string")?;
                if !store.owns_url(s) {
                    return Err(format!("source {:?} must be an https page on {}", crate::text::truncate(s, 120), store.host()));
                }
                out.push(s.to_string());
            }
            out
        }
        Some(_) => return Err("\"sources\" must be a list of URLs".into()),
    };

    let raw = match obj.get("products") {
        Some(Value::Array(a)) => a,
        _ => return Err("the capture needs \"products\": [ ... ]".into()),
    };
    if raw.len() > max_products {
        return Err(format!("{} products in one message; the limit is {max_products}", raw.len()));
    }
    let mut products: Vec<Product> = Vec::with_capacity(raw.len());
    let mut warnings = Vec::new();
    let mut merged = 0;
    for (i, pv) in raw.iter().enumerate() {
        let p = product(pv, store).map_err(|e| {
            let id = pv.get("id").map(|v| v.to_string()).unwrap_or_else(|| "?".into());
            format!("product #{} (id {}): {e}", i + 1, crate::text::truncate(&id, 70))
        })?;
        match products.iter_mut().find(|x| x.id == p.id) {
            Some(first) => {
                merged += 1;
                if let Some(w) = merge_duplicate(first, p) {
                    warnings.push(w);
                }
            }
            None => products.push(p),
        }
    }
    if complete && products.is_empty() && chunk.as_ref().is_none_or(|k| k.chunks == 1) {
        return Err("a complete capture with no products would mark the whole store discontinued; refusing it".into());
    }
    Ok(Capture { store: store.id.clone(), origin: store.origin.clone(), captured_at: captured_at.to_string(), complete, sources, products, merged, warnings, chunk })
}

fn chunk_of(obj: &serde_json::Map<String, Value>) -> Result<Option<Chunk>, String> {
    let (id, n, of) = (obj.get("capture_id"), obj.get("chunk"), obj.get("chunks"));
    if id.is_none() && n.is_none() && of.is_none() {
        return Ok(None);
    }
    let capture_id = id.and_then(Value::as_str).ok_or("\"capture_id\" must be a string")?;
    if !(8..=64).contains(&capture_id.len()) || !capture_id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        return Err("\"capture_id\" must be 8-64 characters of A-Z, a-z, 0-9, - and _".into());
    }
    let chunks = of.and_then(Value::as_u64).filter(|c| (1..=u64::from(MAX_CHUNKS)).contains(c)).ok_or(format!("\"chunks\" must be 1 to {MAX_CHUNKS}"))?;
    let chunk = n.and_then(Value::as_u64).filter(|c| (1..=chunks).contains(c)).ok_or("\"chunk\" must be 1 to \"chunks\"")?;
    Ok(Some(Chunk { capture_id: capture_id.to_string(), chunk: chunk as u32, chunks: chunks as u32 }))
}

fn text_field(v: Option<String>, what: &str, max: usize) -> Result<Option<String>, String> {
    let Some(s) = v else { return Ok(None) };
    let s = clean(&s);
    if s.chars().count() > max {
        return Err(format!("{what} is longer than {max} characters"));
    }
    Ok((!s.is_empty()).then_some(s))
}

fn list_field(v: Option<Vec<String>>, what: &str, max_items: usize, max_len: usize) -> Result<Option<Vec<String>>, String> {
    let Some(list) = v else { return Ok(None) };
    if list.len() > max_items {
        return Err(format!("at most {max_items} {what}"));
    }
    let mut out: Vec<String> = Vec::new();
    for s in list {
        let s = text_field(Some(s), what, max_len)?;
        if let Some(s) = s {
            if !out.iter().any(|o| o.eq_ignore_ascii_case(&s)) {
                out.push(s);
            }
        }
    }
    Ok(Some(out))
}

fn price_field(v: Option<f64>, what: &str) -> Result<Option<f64>, String> {
    match v {
        Some(p) if !p.is_finite() || !(0.0..=MAX_PRICE).contains(&p) => Err(format!("{what} must be a number from 0 to {MAX_PRICE}")),
        other => Ok(other.map(|p| (p * 100.0).round() / 100.0)),
    }
}

fn product(v: &Value, store: &StoreCfg) -> Result<Product, String> {
    let r: RawProduct = serde_json::from_value(v.clone()).map_err(|e| e.to_string())?;
    let id = r.id.text();
    if !valid_product_id(&id) {
        return Err("id must be 1-64 characters of A-Z, a-z, 0-9, '.', '_' and '-'".into());
    }
    let name = text_field(Some(r.name), "name", 300)?.ok_or("name is empty")?;
    if !store.owns_url(&r.url) {
        return Err(format!("url {:?} must be an https page on {}", crate::text::truncate(&r.url, 120), store.host()));
    }
    let kind = match r.kind.map(|k| k.trim().to_ascii_lowercase()).filter(|k| !k.is_empty()) {
        Some(k) if KINDS.contains(&k.as_str()) => Some(k),
        Some(k) => return Err(format!("kind {k:?} isn't one of {}", KINDS.join(", "))),
        None => None,
    };
    let currency = match r.currency.map(|c| c.trim().to_string()) {
        Some(c) if c == store.currency => c,
        Some(c) => return Err(format!("currency {c:?}: {}'s prices are in {}", store.id, store.currency)),
        None => store.currency.clone(),
    };
    let image = match r.image.filter(|s| !s.trim().is_empty()) {
        Some(u) if store.allows_image(&u) => Some(u),
        Some(u) => {
            return Err(format!("image {:?} must be https on {} or one of its image hosts ({})", crate::text::truncate(&u, 120), store.host(), store.image_hosts.join(", ")));
        }
        None => None,
    };
    let mut variants: Vec<Variant> = Vec::new();
    let raw_variants = r.variants.unwrap_or_default();
    if raw_variants.len() > 200 {
        return Err("at most 200 variants".into());
    }
    for rv in raw_variants {
        let vid = rv.id.text();
        if !valid_product_id(&vid) {
            return Err(format!("variant id {vid:?} must be 1-64 characters of A-Z, a-z, 0-9, '.', '_' and '-'"));
        }
        if variants.iter().any(|x| x.id == vid) {
            continue;
        }
        variants.push(Variant {
            id: vid,
            title: text_field(rv.title, "variant title", 200)?,
            sku: text_field(rv.sku, "variant sku", 64)?,
            price: price_field(rv.price, "variant price")?,
            list_price: price_field(rv.list_price, "variant list_price")?,
            available: rv.available,
            options: vec![],
        });
    }
    let metal_group = text_field(r.metal_group, "metal_group", 64)?;
    Ok(Product {
        store: store.id.clone(),
        id,
        name,
        url: r.url,
        category: text_field(r.category, "category", 64)?,
        kind_given: kind.is_some(),
        kind,
        system: None,
        metal: text_field(r.metal, "metal", 100)?,
        metal_group_given: metal_group.is_some(),
        metal_group,
        collection: text_field(r.collection, "collection", 120)?,
        themes: list_field(r.themes, "themes", 30, 60)?,
        badges: list_field(r.badges, "badges", 12, 40)?,
        tags: vec![],
        vendor: None,
        description: None,
        engravable: r.engravable,
        in_stock: r.in_stock,
        currency,
        price: price_field(r.price, "price")?,
        list_price: price_field(r.list_price, "list_price")?,
        price_at: None,
        image,
        variants,
        discontinued: false,
        first_seen: None,
        last_seen: None,
    })
}

/// Merges a later sighting of the same product into the first one: the first sighting's values
/// stay, the later one only fills what the first left unknown. Returns a warning when the two
/// disagree on price.
pub fn merge_duplicate(first: &mut Product, later: Product) -> Option<String> {
    let warn = match (first.current(), later.current()) {
        (Some(a), Some(b)) if (a - b).abs() >= 0.005 => Some(format!("{} appeared twice with different prices ({a:.2} and {b:.2}); kept {a:.2}", first.id)),
        _ => None,
    };
    fill(first, later);
    warn
}

/// Fills `into`'s unknown fields from `from`; lists are combined.
pub fn fill(into: &mut Product, from: Product) {
    macro_rules! keep_or {
        ($($f:ident),*) => { $( if into.$f.is_none() { into.$f = from.$f; } )* };
    }
    keep_or!(category, metal, collection, vendor, description, engravable, in_stock, image);
    if into.kind.is_none() && from.kind_given {
        into.kind = from.kind;
        into.kind_given = true;
    }
    if into.metal_group.is_none() && from.metal_group_given {
        into.metal_group = from.metal_group;
        into.metal_group_given = true;
    }
    if into.price.is_none() && into.list_price.is_none() {
        into.price = from.price;
        into.list_price = from.list_price;
        if from.price_at.is_some() {
            into.price_at = from.price_at;
        }
    }
    for (a, b) in [(&mut into.themes, from.themes), (&mut into.badges, from.badges)] {
        match (a.as_mut(), b) {
            (Some(list), Some(more)) => {
                for m in more {
                    if !list.iter().any(|x| x.eq_ignore_ascii_case(&m)) {
                        list.push(m);
                    }
                }
            }
            (None, more) => *a = more,
            _ => {}
        }
    }
    for t in from.tags {
        if !into.tags.contains(&t) {
            into.tags.push(t);
        }
    }
    for v in from.variants {
        if !into.variants.iter().any(|x| x.id == v.id) {
            into.variants.push(v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    pub fn stores() -> Stores {
        crate::Config::built_in().unwrap().stores
    }

    fn good() -> Value {
        json!({
            "schema": 1, "store": "pandora-us", "origin": "https://us.pandora.net",
            "captured_at": "2026-11-15T16:00:00Z", "complete": true,
            "sources": ["https://us.pandora.net/en/charms/?page=2"],
            "unknown_field": {"ignored": true},
            "products": [
                { "id": "794948C01", "name": "Lucky Cat Charm", "url": "https://us.pandora.net/en/charms/lucky-cat/794948C01.html",
                  "category": "charms", "kind": "charm", "metal": "Sterling silver", "metal_group": "Silver",
                  "themes": ["cats", "luck"], "badges": ["NEW"], "engravable": false, "in_stock": true, "currency": "USD",
                  "list_price": 98.0, "price": 68.0, "image": "https://us.pandora.net/dw/image/v2/x.jpg",
                  "variants": [{"id": "794948C01", "title": "One size", "price": 68, "list_price": 98, "available": true}],
                  "extra": "ignored" }
            ]
        })
    }

    fn check(v: Value) -> Result<Capture, String> {
        validate_text(&v.to_string(), &stores())
    }

    #[test]
    fn a_good_capture_passes_and_unknown_fields_are_ignored() {
        let c = check(good()).unwrap();
        assert_eq!(c.products.len(), 1);
        let p = &c.products[0];
        assert_eq!((p.price, p.list_price, p.kind.as_deref(), p.kind_given), (Some(68.0), Some(98.0), Some("charm"), true));
        assert_eq!(p.themes.as_deref(), Some(&["cats".to_string(), "luck".to_string()][..]));
        assert!(c.complete);
    }

    #[test]
    fn urls_must_stay_on_the_store() {
        let mut v = good();
        v["products"][0]["url"] = json!("https://evil.example/794948C01.html");
        assert!(check(v).unwrap_err().contains("must be an https page on us.pandora.net"));
        let mut v = good();
        v["products"][0]["url"] = json!("http://us.pandora.net/en/x.html");
        assert!(check(v).is_err(), "plain http");
        let mut v = good();
        v["products"][0]["image"] = json!("https://images.evil.example/x.jpg");
        assert!(check(v).unwrap_err().contains("image"));
        let mut v = good();
        v["origin"] = json!("https://evil.example");
        assert!(check(v).unwrap_err().contains("doesn't match"));
        let mut v = good();
        v["sources"] = json!(["javascript:alert(1)"]);
        assert!(check(v).is_err());
    }

    #[test]
    fn prices_must_be_finite_and_not_negative() {
        let mut v = good();
        v["products"][0]["price"] = json!(-1.0);
        assert!(check(v).unwrap_err().contains("price must be a number from 0"));
        // JSON has no NaN; a capture that writes one is not JSON at all.
        let text = good().to_string().replace("\"price\":68.0", "\"price\":NaN");
        assert!(text.contains("NaN"));
        assert!(validate_text(&text, &stores()).unwrap_err().contains("isn't valid JSON"));
        let text = good().to_string().replace("\"price\":68.0", "\"price\":1e999");
        assert!(validate_text(&text, &stores()).is_err(), "out-of-range numbers are refused");
        let mut v = good();
        v["products"][0]["variants"][0]["list_price"] = json!(-5);
        assert!(check(v).is_err());
    }

    #[test]
    fn oversized_strings_and_bad_kinds_are_refused() {
        let mut v = good();
        v["products"][0]["name"] = json!("x".repeat(301));
        assert!(check(v).unwrap_err().contains("name is longer than 300"));
        let mut v = good();
        v["products"][0]["themes"] = json!(["y".repeat(61)]);
        assert!(check(v).is_err());
        let mut v = good();
        v["products"][0]["kind"] = json!("brooch");
        assert!(check(v).unwrap_err().contains("kind"));
        let mut v = good();
        v["products"][0]["id"] = json!("../../etc");
        assert!(check(v).is_err());
        let mut v = good();
        v["products"][0]["currency"] = json!("EUR");
        assert!(check(v).is_err());
    }

    #[test]
    fn schema_version_is_checked() {
        let mut v = good();
        v["schema"] = json!(2);
        assert!(check(v).unwrap_err().contains("schema 2 isn't supported"));
        let mut v = good();
        v.as_object_mut().unwrap().remove("schema");
        assert!(check(v).unwrap_err().contains("schema"));
        let mut v = good();
        v["store"] = json!("amazon");
        assert!(check(v).unwrap_err().contains("unknown store"));
        let mut v = good();
        v["captured_at"] = json!("yesterday");
        assert!(check(v).is_err());
    }

    #[test]
    fn duplicate_ids_are_merged_first_wins() {
        let mut v = good();
        let mut second = v["products"][0].clone();
        second["price"] = json!(58.0);
        second["collection"] = json!("Disney");
        second["themes"] = json!(["Cats", "gifts"]);
        second["variants"] = json!([{"id": "794948C01-2"}]);
        v["products"].as_array_mut().unwrap().push(second);
        let c = check(v).unwrap();
        assert_eq!(c.products.len(), 1);
        assert_eq!(c.merged, 1);
        let p = &c.products[0];
        assert_eq!(p.price, Some(68.0), "the first sighting's price stays");
        assert_eq!(p.collection.as_deref(), Some("Disney"), "gaps are filled from the later one");
        assert_eq!(p.themes.as_ref().unwrap().len(), 3);
        assert_eq!(p.variants.len(), 2);
        assert!(c.warnings[0].contains("different prices"));
    }

    #[test]
    fn an_empty_complete_capture_is_refused_and_chunks_need_the_bridge() {
        let mut v = good();
        v["products"] = json!([]);
        assert!(check(v.clone()).unwrap_err().contains("discontinued"));
        v["complete"] = json!(false);
        assert!(check(v).is_ok(), "an empty partial capture is harmless");
        let mut v = good();
        v["capture_id"] = json!("abcdefgh12");
        v["chunk"] = json!(1);
        v["chunks"] = json!(3);
        assert!(check(v.clone()).unwrap_err().contains("one chunk"));
        let c = validate_value(&v, &stores(), MAX_CHUNK_PRODUCTS).unwrap();
        assert_eq!(c.chunk, Some(Chunk { capture_id: "abcdefgh12".into(), chunk: 1, chunks: 3 }));
        v["chunk"] = json!(4);
        assert!(validate_value(&v, &stores(), MAX_CHUNK_PRODUCTS).is_err());
    }

    #[test]
    fn names_with_markup_stay_text() {
        let mut v = good();
        v["products"][0]["name"] = json!("<script>alert(1)</script> Charm");
        let c = check(v).unwrap();
        assert_eq!(c.products[0].name, "<script>alert(1)</script> Charm");
    }
}
