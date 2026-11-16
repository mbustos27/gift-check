//! gift-check: shop for gifts across store catalogs. See README.md.

use gift_check_core::capture;
use gift_check_core::db::{Db, Query, Sort};
use gift_check_core::model::{Product, Recipient};
use gift_check_core::stores::{valid_currency, valid_host, SourceKind, StoreCfg};
use gift_check_core::{plan, time, Config};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

const HELP: &str = "gift-check - shop for gifts across store catalogs, on this computer

USAGE: gift-check [--data-dir DIR] <command> ...

  stores                                   the configured stores and what is stored for each
  fetch shopify <domain> [--collection H] [--currency C]
                                           read a Shopify store's public catalog (politely);
                                           a store not in stores.toml needs --currency (USD, EUR, ...)
  import <capture.json> [--force]          store a capture made in the browser (docs/CAPTURE.md);
                                           --force takes one older than 30 days
  search <words...> [--store S] [--kind K] [--max N] [--on-sale] [--json]
  product <store> <id>                     details and price history
  recipient add <nick> [options]           someone to shop for (a nickname is enough)
  recipient edit <nick> [options]          options: --wrist CM --metal M --budget N --likes a,b
                                           --systems a,b --owns store:id,... --notes TEXT
  recipient list
  plan --recipient <nick> --budget <N> [--store S] [--json]
                                           suggest a bracelet and charms within the budget
  export --json                            everything as JSON, for agents
  ui [--no-open] [--port N]                the window, on http://127.0.0.1:8781/

Data lives in %APPDATA%\\gift-check (gift.db) unless --data-dir says otherwise.
gift-check only reads public product listings. It never signs in, never touches carts or
checkout, and stops at bot checks instead of trying to get past them.";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(match run(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("gift-check: {e}");
            1
        }
    });
}

/// Positional words, `--name value` options and `--switch` flags.
struct Args {
    pos: Vec<String>,
    vals: HashMap<String, String>,
    flags: HashSet<String>,
}

impl Args {
    fn parse(args: &[String], value_opts: &[&str], flag_opts: &[&str]) -> Result<Args, String> {
        let mut a = Args { pos: vec![], vals: HashMap::new(), flags: HashSet::new() };
        let mut it = args.iter();
        while let Some(x) = it.next() {
            if let Some(name) = x.strip_prefix("--") {
                if value_opts.contains(&name) {
                    let v = it.next().ok_or_else(|| format!("--{name} needs a value"))?;
                    a.vals.insert(name.to_string(), v.clone());
                } else if flag_opts.contains(&name) {
                    a.flags.insert(name.to_string());
                } else {
                    return Err(format!("unknown option --{name}"));
                }
            } else {
                a.pos.push(x.clone());
            }
        }
        Ok(a)
    }

    fn val(&self, k: &str) -> Option<&str> {
        self.vals.get(k).map(String::as_str)
    }

    fn num(&self, k: &str) -> Result<Option<f64>, String> {
        match self.val(k) {
            None => Ok(None),
            Some(v) => v.trim().trim_start_matches('$').parse::<f64>().ok().filter(|n| n.is_finite() && *n >= 0.0).map(Some).ok_or(format!("--{k} must be a number")),
        }
    }
}

fn run(mut args: Vec<String>) -> Result<i32, String> {
    let mut data_dir: Option<PathBuf> = None;
    if let Some(i) = args.iter().position(|a| a == "--data-dir") {
        let dir = args.get(i + 1).ok_or("--data-dir needs a folder")?.clone();
        data_dir = Some(PathBuf::from(dir));
        args.drain(i..=i + 1);
    }
    let Some(cmd) = args.first().cloned() else {
        println!("{HELP}");
        return Ok(0);
    };
    let rest = &args[1..];
    if matches!(cmd.as_str(), "help" | "--help" | "-h") {
        println!("{HELP}");
        return Ok(0);
    }
    if matches!(cmd.as_str(), "--version" | "-V" | "version") {
        println!("gift-check {}", gift_check_core::VERSION);
        return Ok(0);
    }
    let dir = match data_dir {
        Some(d) => d,
        None => gift_check_core::default_data_dir()?,
    };
    if cmd == "ui" {
        return Ok(gift_check_ui::main(&dir, rest));
    }
    let cfg = Config::load(&dir)?;
    let db = Db::open(&gift_check_core::db_path(&dir))?;
    db.sync_rules(&cfg.rules, &cfg.rules_hash)?;
    for s in &cfg.stores.stores {
        db.upsert_store(s)?;
    }
    match cmd.as_str() {
        "stores" => stores(&db, &cfg, &dir),
        "fetch" => fetch(&db, &cfg, rest),
        "import" => import(&db, &cfg, rest),
        "search" => search(&db, &cfg, rest),
        "product" => product(&db, &cfg, rest),
        "recipient" => recipient(&db, rest),
        "plan" => plan_cmd(&db, &cfg, rest),
        "export" => {
            let a = Args::parse(rest, &[], &["json"])?;
            if !a.flags.contains("json") {
                return Err("export writes JSON; run `gift-check export --json`".into());
            }
            println!("{}", serde_json::to_string_pretty(&db.export()?).map_err(|e| e.to_string())?);
            Ok(0)
        }
        other => Err(format!("unknown command {other:?}; run `gift-check help`")),
    }
}

fn money(v: Option<f64>) -> String {
    v.map_or_else(|| "?".into(), |v| format!("${v:.2}"))
}

fn stores(db: &Db, cfg: &Config, dir: &Path) -> Result<i32, String> {
    let counts = db.store_counts()?;
    for s in &cfg.stores.stores {
        let c = counts.iter().find(|c| c["store"] == s.id.as_str());
        let n = c.and_then(|c| c["products"].as_i64()).unwrap_or(0);
        let last = c.and_then(|c| c["last_capture"].as_str()).map(|t| time::parse_rfc3339(t).map_or(t.to_string(), |x| format!("{} ago", time::ago(time::now() - x)))).unwrap_or_else(|| "never".into());
        let how = match s.source {
            SourceKind::Shopify => format!("gift-check fetch shopify {}", s.host()),
            SourceKind::BrowserCapture => "browser capture (gift-check ui, then the bridge)".to_string(),
        };
        println!("{:<14} {:<22} {:>6} products   last {:<12} {}", s.id, s.name, n, last, how);
    }
    for c in counts.iter().filter(|c| cfg.stores.get(c["store"].as_str().unwrap_or("")).is_none()) {
        println!("{:<14} (not in stores.toml)  {:>6} products", c["store"].as_str().unwrap_or("?"), c["products"].as_i64().unwrap_or(0));
    }
    println!("\nData: {}\nStores from: {}\nRules from: {} ({} rules, {} not yet sourced)", dir.display(), cfg.stores_from, cfg.rules_from, cfg.rules.rule_count(), cfg.rules.todo_count());
    Ok(0)
}

fn fetch(db: &Db, cfg: &Config, rest: &[String]) -> Result<i32, String> {
    let a = Args::parse(rest, &["collection", "currency"], &[])?;
    if a.pos.first().map(String::as_str) != Some("shopify") || a.pos.len() != 2 {
        return Err("usage: gift-check fetch shopify <domain> [--collection <handle>] [--currency <code>]".into());
    }
    let host = a.pos[1].trim().trim_start_matches("https://").trim_start_matches("http://").trim_end_matches('/').to_ascii_lowercase();
    if !valid_host(&host) {
        return Err(format!("{:?} isn't a plain domain like www.hardjewelry.com", a.pos[1]));
    }
    let store = match cfg.stores.by_host(&host) {
        Some(s) if s.source == SourceKind::BrowserCapture => {
            return Err(format!("{} blocks plain requests, so it is captured in the browser: run `gift-check ui` and use the bridge", s.name));
        }
        Some(s) => {
            if a.val("currency").is_some_and(|c| c != s.currency) {
                return Err(format!("{} is in stores.toml with prices in {}; --currency can't change that", s.name, s.currency));
            }
            s.clone()
        }
        None => {
            // Prices from different currencies must never be added up, so a one-off store says its own.
            let currency = a.val("currency").map(|c| c.trim().to_ascii_uppercase()).ok_or(format!(
                "{host} isn't in stores.toml, so gift-check doesn't know its currency: add --currency USD (or EUR, GBP, ...), or add the store to stores.toml"
            ))?;
            if !valid_currency(&currency) {
                return Err(format!("--currency {currency:?} must be a three-letter code like USD"));
            }
            println!("{host} isn't in stores.toml; fetching it as a one-off store in {currency} (its images won't show in the window).");
            StoreCfg::ad_hoc_shopify(&host, &currency)
        }
    };
    let http = gift_check_fetch::Http::new();
    let clock = gift_check_fetch::SystemClock;
    let base = store.origin.clone();
    let rep = gift_check_fetch::run_shopify(db, &cfg.rules, &store, &base, a.val("collection"), &http, &clock, &mut |line| println!("  {line}"))?;
    for l in &rep.log {
        println!("  {l}");
    }
    println!(
        "{}: {} products from {} page(s), {} requests{}{}.",
        store.name,
        rep.products,
        rep.pages,
        rep.requests,
        if rep.complete { ", complete" } else { ", not complete" },
        if rep.discontinued > 0 { format!(", {} marked gone", rep.discontinued) } else { String::new() }
    );
    match rep.stop {
        Some(s) => {
            println!("Stopped: {s}");
            Ok(3)
        }
        None => Ok(0),
    }
}

fn import(db: &Db, cfg: &Config, rest: &[String]) -> Result<i32, String> {
    let a = Args::parse(rest, &[], &["force"])?;
    let [file] = a.pos.as_slice() else { return Err("usage: gift-check import <capture.json> [--force]".into()) };
    let size = std::fs::metadata(file).map_err(|e| format!("can't read {file}: {e}"))?.len();
    if size > capture::MAX_FILE_BYTES {
        return Err(format!("{file} is {} MB; the limit is 50 MB", size / (1024 * 1024)));
    }
    let text = std::fs::read_to_string(file).map_err(|e| format!("can't read {file}: {e}"))?;
    let c = capture::validate_file(&text, &cfg.stores, a.flags.contains("force")).map_err(|e| {
        let known: Vec<&str> = cfg.stores.stores.iter().map(|s| s.id.as_str()).collect();
        if e.starts_with("unknown store") { format!("{file}: {e}; known stores: {}", known.join(", ")) } else { format!("{file}: {e}") }
    })?;
    let s = capture::store(db, cfg, &c, "import")?;
    println!(
        "Imported {} products for {} (capture #{}){}{}.",
        s.products,
        s.store,
        s.capture_id,
        if s.complete { format!(", complete: {} marked gone", s.discontinued) } else { ", partial: nothing marked gone".into() },
        if s.merged > 0 { format!(", {} duplicates merged", s.merged) } else { String::new() }
    );
    for w in &s.warnings {
        println!("  note: {w}");
    }
    Ok(0)
}

fn print_row(p: &Product, cfg: &Config) {
    let off = p.pct_off().map(|o| format!("  was {} ({o:.0}% off)", money(p.list_price))).unwrap_or_default();
    println!("{:>9}{off}  {}", money(p.current()), p.name);
    let mut facts = vec![format!("{}:{}", p.store, p.id)];
    facts.extend(p.kind.clone());
    facts.extend(p.system.as_deref().map(|s| cfg.rules.system_label(s)));
    facts.extend(p.metal_group.clone().or_else(|| p.metal.clone()));
    facts.extend(p.badges.iter().flatten().cloned());
    if p.in_stock == Some(false) {
        facts.push("out of stock".into());
    }
    if p.discontinued {
        facts.push("not in the latest capture".into());
    }
    println!("           {}", facts.join(" · "));
    println!("           {}", p.url);
}

fn search(db: &Db, cfg: &Config, rest: &[String]) -> Result<i32, String> {
    let a = Args::parse(rest, &["store", "kind", "max", "min", "metal", "sort", "limit"], &["on-sale", "json", "in-stock", "all"])?;
    let q = Query {
        words: a.pos.clone(),
        store: a.val("store").map(str::to_string),
        kind: a.val("kind").map(str::to_string),
        metal_group: a.val("metal").map(str::to_string),
        min: a.num("min")?,
        max: a.num("max")?,
        on_sale: a.flags.contains("on-sale"),
        in_stock: a.flags.contains("in-stock"),
        include_discontinued: a.flags.contains("all"),
        sort: Sort::parse(a.val("sort").unwrap_or("price")).ok_or("--sort is price, price-desc, off, newest or name")?,
        limit: a.val("limit").and_then(|l| l.parse().ok()).unwrap_or(30),
        ..Default::default()
    };
    let (rows, total) = db.search(&q)?;
    if a.flags.contains("json") {
        println!("{}", serde_json::to_string_pretty(&serde_json::json!({ "total": total, "products": rows })).map_err(|e| e.to_string())?);
        return Ok(0);
    }
    if rows.is_empty() {
        println!("Nothing matches.");
        return Ok(0);
    }
    for p in &rows {
        print_row(p, cfg);
    }
    if total as usize > rows.len() {
        println!("\n{} of {total} shown (--limit N for more).", rows.len());
    }
    Ok(0)
}

fn product(db: &Db, cfg: &Config, rest: &[String]) -> Result<i32, String> {
    let a = Args::parse(rest, &[], &["json"])?;
    let [store, id] = a.pos.as_slice() else { return Err("usage: gift-check product <store> <id>".into()) };
    let p = db.product(store, id)?.ok_or_else(|| format!("no product {store}:{id}"))?;
    let h = db.history(store, id)?;
    if a.flags.contains("json") {
        println!("{}", serde_json::to_string_pretty(&serde_json::json!({ "product": p, "history": h })).map_err(|e| e.to_string())?);
        return Ok(0);
    }
    print_row(&p, cfg);
    for (text, source) in cfg.rules.policy_warnings(&p) {
        println!("           {text} (source: {source})");
    }
    if !p.variants.is_empty() {
        println!("\nVariants:");
        for v in &p.variants {
            let avail = match v.available {
                Some(true) => "available",
                Some(false) => "sold out",
                None => "",
            };
            println!("  {:<30} {:>9} {:>9}  {avail}", v.title.clone().unwrap_or_else(|| v.id.clone()), money(v.price), money(v.list_price));
        }
    }
    println!("\nPrice history (oldest first):");
    for o in &h {
        println!("  {}  {:>9}  list {:>9}  {} #{}", o.at, money(o.price), money(o.list_price), o.source.as_deref().unwrap_or(""), o.capture_id);
    }
    Ok(0)
}

fn split(v: &str) -> Vec<String> {
    v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
}

fn recipient(db: &Db, rest: &[String]) -> Result<i32, String> {
    let a = Args::parse(rest, &["wrist", "metal", "systems", "owns", "likes", "budget", "notes"], &["json"])?;
    match a.pos.first().map(String::as_str) {
        Some("list") => {
            let all = db.recipients()?;
            if a.flags.contains("json") {
                println!("{}", serde_json::to_string_pretty(&all).map_err(|e| e.to_string())?);
                return Ok(0);
            }
            if all.is_empty() {
                println!("No one yet: gift-check recipient add <nick> --likes cats --budget 150");
            }
            for r in all {
                println!(
                    "{}: budget {}, wrist {}, metal {}, likes [{}], systems [{}], owns {} item(s){}",
                    r.nick,
                    money(r.budget),
                    r.wrist_cm.map_or("?".into(), |w| format!("{w} cm")),
                    r.metal.as_deref().unwrap_or("any"),
                    r.likes.join(", "),
                    r.systems.join(", "),
                    r.owned.len(),
                    r.notes.as_deref().map(|n| format!("; notes: {n}")).unwrap_or_default()
                );
            }
            Ok(0)
        }
        Some(op @ ("add" | "edit")) => {
            let nick = a.pos.get(1).ok_or(format!("usage: gift-check recipient {op} <nick> [options]"))?;
            let existing = db.recipient(nick)?;
            let mut r = match (op, existing) {
                ("add", Some(_)) => return Err(format!("{nick} already exists; use `gift-check recipient edit {nick}`")),
                ("add", None) => Recipient { nick: nick.clone(), ..Default::default() },
                (_, Some(r)) => r,
                (_, None) => return Err(format!("no recipient called {nick:?}; add them first")),
            };
            if let Some(w) = a.num("wrist")? {
                r.wrist_cm = Some(w);
            }
            if let Some(b) = a.num("budget")? {
                r.budget = Some(b);
            }
            if let Some(m) = a.val("metal") {
                r.metal = Some(m.to_string());
            }
            if let Some(v) = a.val("systems") {
                r.systems = split(v);
            }
            if let Some(v) = a.val("owns") {
                r.owned = split(v);
            }
            if let Some(v) = a.val("likes") {
                r.likes = split(v);
            }
            if let Some(v) = a.val("notes") {
                r.notes = Some(v.to_string());
            }
            db.save_recipient(&r)?;
            println!("Saved {}.", r.nick);
            Ok(0)
        }
        _ => Err("usage: gift-check recipient add|edit|list ...".into()),
    }
}

fn plan_cmd(db: &Db, cfg: &Config, rest: &[String]) -> Result<i32, String> {
    let a = Args::parse(rest, &["recipient", "budget", "store"], &["json"])?;
    let nick = a.val("recipient").ok_or("usage: gift-check plan --recipient <nick> --budget <N> [--store S]")?;
    let r = db.recipient(nick)?.ok_or_else(|| format!("no recipient called {nick:?}"))?;
    let budget = a.num("budget")?.or(r.budget).ok_or("give --budget (or set one on the recipient)")?;
    let catalog = db.catalog(a.val("store"))?;
    let s = plan::suggest(&catalog, &r, budget, a.val("store"), &cfg.rules, 4);
    if a.flags.contains("json") {
        println!("{}", serde_json::to_string_pretty(&s).map_err(|e| e.to_string())?);
        return Ok(0);
    }
    if s.is_empty() {
        println!("Nothing fits {} among {} products. Try a bigger budget, another store, or capture more of the catalog.", money(Some(budget)), catalog.len());
        return Ok(0);
    }
    for (i, sug) in s.iter().enumerate() {
        println!(
            "\nOption {}: {}  {}{}  ({} left)",
            i + 1,
            sug.title,
            money(Some(sug.total)),
            if sug.savings > 0.0 { format!(", list {} so you save {}", money(Some(sug.list_total)), money(Some(sug.savings))) } else { String::new() },
            money(Some(sug.left))
        );
        for p in sug.base.iter().chain(&sug.addons) {
            println!("  {:>9}  {}  [{}:{}]", money(Some(p.price)), p.name, p.store, p.id);
            println!("             {}", p.why);
            for w in &p.policies {
                println!("             ! {w}");
            }
        }
        for n in &sug.notes {
            println!("  {n}");
        }
    }
    Ok(0)
}
