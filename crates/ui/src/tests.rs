use super::*;

const HOST: Option<&str> = Some("127.0.0.1:8781");
const JSON: Option<&str> = Some("application/json");
const OURS: Option<&str> = Some("http://127.0.0.1:8781");

/// Every test gets its own in-memory database and a temporary data folder, never the real one.
fn app() -> App {
    let dir = std::env::temp_dir().join(format!("gift-check-ui-tests-{}", std::process::id()));
    App::with(Db::memory().unwrap(), Config::built_in().unwrap(), &dir, 8781, false).unwrap()
}

fn get(a: &App, url: &str) -> Reply {
    a.route("GET", url, HOST, None, None, None, "")
}

fn post(a: &App, path: &str, body: &str) -> Reply {
    a.route("POST", path, HOST, Some("1"), JSON, OURS, body)
}

fn body(r: &Reply) -> Value {
    serde_json::from_str(&r.body).unwrap_or_else(|e| panic!("{e}: {}", r.body))
}

fn chunk(id: &str, n: u32, of: u32, complete: bool, products: Value) -> String {
    json!({
        "type": "gift-check-capture", "schema": 1, "capture_id": id, "chunk": n, "chunks": of,
        "store": "pandora-us", "origin": "https://us.pandora.net", "captured_at": "2026-11-15T16:00:00Z",
        "complete": complete, "sources": [format!("https://us.pandora.net/en/charms/?page={n}")], "products": products,
    })
    .to_string()
}

fn product(id: &str, name: &str, price: f64, list: f64) -> Value {
    json!({ "id": id, "name": name, "url": format!("https://us.pandora.net/en/charms/{id}.html"), "category": "charms",
            "list_price": list, "price": price, "image": format!("https://us.pandora.net/dw/image/v2/{id}.jpg"), "in_stock": true })
}

#[test]
fn other_hosts_are_refused() {
    let a = app();
    // A DNS-rebinding page arrives with its own host name.
    assert_eq!(a.route("GET", "/api/state", Some("evil.example:8781"), None, None, None, "").status, 421);
    assert_eq!(a.route("GET", "/", None, None, None, None, "").status, 421);
    assert_eq!(a.route("GET", "/", Some("127.0.0.1:9999"), None, None, None, "").status, 421);
    assert_eq!(a.route("GET", "/", Some("localhost:8781"), None, None, None, "").status, 200);
}

#[test]
fn api_reads_refuse_other_origins_and_posts_need_marker_json_and_our_origin() {
    let a = app();
    assert_eq!(a.route("GET", "/api/state", HOST, None, None, Some("https://us.pandora.net"), "").status, 403, "a store page can't read the API");
    assert_eq!(a.route("GET", "/api/state", HOST, None, None, Some("null"), "").status, 403);
    assert_eq!(a.route("GET", "/api/state", HOST, None, None, OURS, "").status, 200);
    let rec = r#"{"nick":"Mom"}"#;
    assert_eq!(a.route("POST", "/api/recipient", HOST, None, JSON, OURS, rec).status, 403, "missing marker");
    assert_eq!(a.route("POST", "/api/recipient", HOST, Some("0"), JSON, OURS, rec).status, 403, "wrong marker");
    assert_eq!(a.route("POST", "/api/recipient", HOST, Some("1"), Some("text/plain"), OURS, rec).status, 403, "a plain form post");
    assert_eq!(a.route("POST", "/api/recipient", HOST, Some("1"), JSON, Some("https://evil.example"), rec).status, 403);
    assert_eq!(a.route("POST", "/api/recipient", HOST, Some("1"), JSON, Some("null"), rec).status, 403);
    // The bridge posts captures from our own page only; a store page posting directly is refused.
    let cap = chunk("abcdefgh01", 1, 1, false, json!([product("794948C01", "Cat Charm", 68.0, 98.0)]));
    assert_eq!(a.route("POST", "/api/capture", HOST, Some("1"), JSON, Some("https://us.pandora.net"), &cap).status, 403);
    assert_eq!(a.route("POST", "/api/capture", HOST, None, JSON, None, &cap).status, 403);
    assert_eq!(post(&a, "/api/recipient", rec).status, 200);
    assert_eq!(a.route("DELETE", "/api/recipient", HOST, Some("1"), JSON, OURS, rec).status, 405);
}

#[test]
fn csp_limits_images_to_the_store_hosts() {
    let a = app();
    let csp = a.security_headers().into_iter().find(|(k, _)| *k == "Content-Security-Policy").unwrap().1;
    assert!(csp.contains("img-src 'self' https://cdn.shopify.com https://us.pandora.net https://www.hardjewelry.com;"), "{csp}");
    assert!(csp.contains("script-src 'self';") && csp.contains("connect-src 'self';") && csp.contains("frame-ancestors 'none'"));
    assert!(!csp.contains('*') && !csp.contains("unsafe"), "{csp}");
    assert!(body_limit("/api/capture") > body_limit("/api/recipient"));
}

#[test]
fn pages_never_build_markup_from_data() {
    // Store text is shown with textContent only; none of these may appear in the window's code.
    for (name, code) in [("app.js", APP_JS), ("bridge.js", BRIDGE_JS)] {
        for bad in ["innerHTML", "outerHTML", "insertAdjacentHTML", "document.write", "eval(", "new Function"] {
            assert!(!code.contains(bad), "{name} uses {bad}");
        }
    }
    for (name, page) in [("index.html", INDEX), ("bridge.html", BRIDGE)] {
        assert!(!page.contains("<script>") && !page.contains("onclick") && !page.contains("http://") && !page.contains("https://"), "{name} must load only its own scripts");
    }
    // The bridge checks who sent a message before using it.
    assert!(BRIDGE_JS.contains("e.source") && BRIDGE_JS.contains("e.origin"));
    assert!(!BRIDGE_JS.contains("noopener"), "the bridge must keep the opener so the store tab can post back");
}

#[test]
fn chunked_bridge_capture_is_stored_once_complete() {
    let a = app();
    let id = "cap-0001-xyz";
    let r = post(&a, "/api/capture", &chunk(id, 1, 2, true, json!([product("794948C01", "Lucky Cat Charm", 68.0, 98.0)])));
    assert_eq!(r.status, 200, "{}", r.body);
    let v = body(&r);
    assert_eq!((v["done"].as_bool(), v["received"].as_u64(), v["stored"].as_i64()), (Some(false), Some(1), Some(1)));
    // The same chunk again is not stored twice.
    let v = body(&post(&a, "/api/capture", &chunk(id, 1, 2, true, json!([product("794948C01", "Lucky Cat Charm", 1.0, 98.0)]))));
    assert_eq!(v["duplicate"], true);
    // A chunk that disagrees with the first one is refused.
    let mismatch = chunk(id, 2, 2, false, json!([product("791088", "Safety Chain", 30.0, 30.0)]));
    assert_eq!(post(&a, "/api/capture", &mismatch).status, 400);
    let r = post(&a, "/api/capture", &chunk(id, 2, 2, true, json!([product("791088", "Band of Hearts Safety Chain Clip Charm", 30.0, 30.0)])));
    let v = body(&r);
    assert_eq!((v["done"].as_bool(), v["stored"].as_i64(), v["complete"].as_bool()), (Some(true), Some(2), Some(true)), "{v}");
    let p = a.db.product("pandora-us", "794948C01").unwrap().unwrap();
    assert_eq!(p.price, Some(68.0), "the duplicate chunk's price was not stored");
    assert_eq!(a.db.history("pandora-us", "794948C01").unwrap().len(), 1);
    // After it is done, a resent chunk is acknowledged and ignored.
    assert_eq!(body(&post(&a, "/api/capture", &chunk(id, 2, 2, true, json!([product("791088", "X", 1.0, 1.0)]))))["duplicate"], true);
    let state = body(&get(&a, "/api/state"));
    assert_eq!(state["captures"][0]["capture"]["source"], "bridge");
    assert_eq!(state["captures"][0]["capture"]["product_count"], 2);
}

#[test]
fn bridge_captures_are_validated_like_import() {
    let a = app();
    let mut bad = product("794948C01", "Cat", 68.0, 98.0);
    bad["url"] = json!("https://evil.example/x.html");
    let r = post(&a, "/api/capture", &chunk("abcdefgh02", 1, 1, false, json!([bad])));
    assert_eq!(r.status, 400);
    assert!(body(&r)["error"].as_str().unwrap().contains("must be an https page on us.pandora.net"));
    let r = post(&a, "/api/capture", &chunk("abcdefgh03", 1, 1, false, json!([product("1", "Neg", -1.0, 5.0)])));
    assert_eq!(r.status, 400);
    let mut wrong = serde_json::from_str::<Value>(&chunk("abcdefgh04", 1, 1, false, json!([]))).unwrap();
    wrong["schema"] = json!(2);
    assert_eq!(post(&a, "/api/capture", &wrong.to_string()).status, 400);
    assert_eq!(body(&get(&a, "/api/search")).get("total").and_then(Value::as_i64), Some(0), "nothing was stored");
}

#[test]
fn names_with_script_tags_come_back_as_plain_strings() {
    let a = app();
    let name = "<script>alert(1)</script><img src=x onerror=alert(2)> Charm";
    assert_eq!(post(&a, "/api/capture", &chunk("abcdefgh05", 1, 1, false, json!([product("799999", name, 10.0, 10.0)]))).status, 200);
    let v = body(&get(&a, "/api/search?q=charm"));
    assert_eq!(v["rows"][0]["name"], name, "kept as text, exactly");
    let d = body(&get(&a, "/api/product?store=pandora-us&id=799999"));
    assert_eq!(d["product"]["name"], name);
}

#[test]
fn search_filters_and_fits() {
    let a = app();
    let products = json!([
        product("790001", "Lucky Cat Dangle Charm", 50.0, 70.0),
        product("790002", "Star Charm", 40.0, 40.0),
        product("797000", "Pandora Reflexions Cat Clip Charm", 45.0, 45.0),
        { "id": "590702", "name": "Pandora Moments Snake Chain Bracelet", "url": "https://us.pandora.net/en/bracelets/590702.html", "category": "bracelets", "price": 80.0, "list_price": 80.0 },
    ]);
    assert_eq!(post(&a, "/api/capture", &chunk("abcdefgh06", 1, 1, true, products)).status, 200);
    let ids = |url: &str| body(&get(&a, url))["rows"].as_array().unwrap().iter().map(|r| r["id"].as_str().unwrap().to_string()).collect::<Vec<_>>();
    assert_eq!(ids("/api/search?sale=1"), vec!["790001"]);
    assert_eq!(ids("/api/search?kind=bracelet"), vec!["590702"]);
    assert_eq!(ids("/api/search?q=cat&sort=price"), vec!["797000", "790001"]);
    assert_eq!(ids("/api/search?fits=pandora-moments"), vec!["790002", "790001"], "Moments add-ons only: no bracelet, no Reflexions clip");
    assert_eq!(ids("/api/search?fits=pandora-reflexions"), vec!["797000"]);
    assert_eq!(ids("/api/search?max=45&sort=price-desc"), vec!["797000", "790002"]);
    assert_eq!(get(&a, "/api/search?sort=sideways").status, 400);
    let cmp = body(&get(&a, "/api/compare?by=store"));
    assert!(cmp["stats"].as_array().unwrap().iter().any(|s| s["kind"] == "charm" && s["count"] == 2 && s["median"] == 45.0), "{cmp}");
}

#[test]
fn plans_are_checked_saved_and_repriced() {
    let a = app();
    let products = json!([
        product("790001", "Lucky Cat Dangle Charm", 50.0, 70.0),
        product("797000", "Pandora Reflexions Cat Clip Charm", 45.0, 45.0),
        { "id": "590702", "name": "Pandora Moments Snake Chain Bracelet", "url": "https://us.pandora.net/en/bracelets/590702.html", "category": "bracelets", "price": 80.0, "list_price": 80.0 },
    ]);
    post(&a, "/api/capture", &chunk("abcdefgh07", 1, 1, false, products));
    assert_eq!(post(&a, "/api/recipient", r#"{"nick":"Mom","likes":["cats"],"owned":["pandora-us:790001"],"budget":150}"#).status, 200);
    let items = r#"[{"store":"pandora-us","id":"590702"},{"store":"pandora-us","id":"790001"},{"store":"pandora-us","id":"797000"}]"#;
    let ev = body(&post(&a, "/api/plan/evaluate", &format!(r#"{{"recipient":"Mom","items":{items}}}"#)))["evaluation"].clone();
    let texts: Vec<String> = ev["warnings"].as_array().unwrap().iter().map(|w| w["text"].as_str().unwrap().to_string()).collect();
    assert!(texts.iter().any(|t| t.contains("already owns Lucky Cat")), "{texts:?}");
    assert!(texts.iter().any(|t| t.contains("doesn't fit the Pandora Moments bracelet")), "{texts:?}");
    assert!(texts.iter().any(|t| t.contains("Over budget")), "{texts:?}");
    assert_eq!((ev["total"].as_f64(), ev["list_total"].as_f64()), (Some(175.0), Some(195.0)));

    let r = post(&a, "/api/plan/save", &format!(r#"{{"name":"Xmas","recipient":"Mom","budget":150,"items":{items}}}"#));
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(post(&a, "/api/plan/save", r#"{"name":"X","recipient":"Nobody","items":[{"store":"pandora-us","id":"1"}]}"#).status, 400);
    // A new capture lowers the bracelet's price; the saved plan's total follows.
    post(&a, "/api/capture", &chunk("abcdefgh08", 1, 1, false, json!([{ "id": "590702", "name": "Pandora Moments Snake Chain Bracelet", "url": "https://us.pandora.net/en/bracelets/590702.html", "category": "bracelets", "price": 56.0, "list_price": 80.0 }])));
    let plans = body(&get(&a, "/api/plans"));
    assert_eq!(plans["plans"][0]["total"].as_f64(), Some(151.0));
    assert_eq!(plans["plans"][0]["stops"].as_u64(), Some(2), "{plans}");

    let s = body(&post(&a, "/api/plan/suggest", r#"{"recipient":"Mom","budget":150}"#));
    for sug in s["suggestions"].as_array().unwrap() {
        assert!(sug["total"].as_f64().unwrap() <= 150.0);
        assert!(sug["addons"].as_array().unwrap().iter().all(|p| p["id"] != "790001"), "owned charm suggested: {sug}");
    }
}

#[test]
fn fetch_is_only_for_shopify_stores() {
    let a = app();
    let r = post(&a, "/api/fetch", r#"{"store":"pandora-us"}"#);
    assert_eq!(r.status, 400);
    assert!(body(&r)["error"].as_str().unwrap().contains("use the bridge"));
    assert_eq!(post(&a, "/api/fetch", r#"{"store":"nope"}"#).status, 400);
    assert!(body(&post(&a, "/api/fetch", r#"{"store":"hardjewelry"}"#))["error"].as_str().unwrap().contains("turned off"), "tests never reach the network");
}

#[test]
fn pages_are_served_with_their_types() {
    let a = app();
    assert_eq!(get(&a, "/").content_type, "text/html; charset=utf-8");
    assert_eq!(get(&a, "/bridge").content_type, "text/html; charset=utf-8");
    assert!(get(&a, "/app.js").content_type.starts_with("text/javascript"));
    assert!(get(&a, "/bridge.js").content_type.starts_with("text/javascript"));
    assert!(get(&a, "/style.css").content_type.starts_with("text/css"));
    assert_eq!(get(&a, "/nope").status, 404);
    assert_eq!(get(&a, "/api/product?store=pandora-us&id=..%2F..%2Fx").status, 400);
}

#[test]
fn query_decoding() {
    assert_eq!(param("q=lucky+cat&store=pandora-us", "q").as_deref(), Some("lucky cat"));
    assert_eq!(param("q=50%25%20off", "q").as_deref(), Some("50% off"));
    assert_eq!(param("q=%zz", "q").as_deref(), Some("%zz"));
    assert_eq!(param("a=1", "q"), None);
}
