//! gift-check core: the local database, capture validation, store and rule files, robots.txt
//! parsing, Shopify JSON mapping and the gift planner. No network code lives here.
//!
//! Text from stores is data. It is stored as plain text (HTML stripped from descriptions) and the
//! window only ever shows it with `textContent`.

pub mod capture;
pub mod db;
pub mod model;
pub mod plan;
pub mod robots;
pub mod rules;
pub mod shopify;
pub mod stores;
pub mod text;
pub mod time;

use std::path::{Path, PathBuf};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The one identity gift-check uses on the web. It is never varied to get past a refusal.
pub const USER_AGENT: &str = concat!("gift-check/", env!("CARGO_PKG_VERSION"), " (personal gift price lookup)");
/// The product token robots.txt groups are matched against.
pub const UA_TOKEN: &str = "gift-check";

const BUILT_IN_STORES: &str = include_str!("../../../data/stores.toml");
const BUILT_IN_RULES: &str = include_str!("../../../data/rules.toml");
/// The repository's `data` folder, so edits to the rule files take effect without a rebuild.
const REPO_DATA: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../data");

/// `%APPDATA%\gift-check` on Windows (`~/.local/share/gift-check` elsewhere).
pub fn default_data_dir() -> Result<PathBuf, String> {
    if let Some(app) = std::env::var_os("APPDATA").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(app).join("gift-check"));
    }
    if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(home).join(".local").join("share").join("gift-check"));
    }
    Err("can't find %APPDATA% (or $HOME); pass --data-dir".into())
}

pub fn db_path(data_dir: &Path) -> PathBuf {
    data_dir.join("gift.db")
}

/// The store list and the rules, with where each came from.
#[derive(Debug, Clone)]
pub struct Config {
    pub stores: stores::Stores,
    pub rules: rules::Rules,
    pub stores_from: String,
    pub rules_from: String,
    /// A fingerprint of the rules text, so products are re-classified when the rules change.
    pub rules_hash: String,
}

impl Config {
    pub fn from_texts(stores_text: &str, rules_text: &str) -> Result<Config, String> {
        Ok(Config {
            stores: stores::Stores::parse(stores_text)?,
            rules: rules::Rules::parse(rules_text)?,
            stores_from: "given".into(),
            rules_from: "given".into(),
            rules_hash: fingerprint(rules_text),
        })
    }

    /// The built-in copies compiled into the program.
    pub fn built_in() -> Result<Config, String> {
        let mut c = Config::from_texts(BUILT_IN_STORES, BUILT_IN_RULES)?;
        c.stores_from = "built in".into();
        c.rules_from = "built in".into();
        Ok(c)
    }

    /// Each file is read from the data folder if a copy is there, else from the repository's
    /// `data` folder, else the copy built into the program.
    pub fn load(data_dir: &Path) -> Result<Config, String> {
        let (stores_text, stores_from) = read_config(data_dir, "stores.toml", BUILT_IN_STORES)?;
        let (rules_text, rules_from) = read_config(data_dir, "rules.toml", BUILT_IN_RULES)?;
        let mut c = Config::from_texts(&stores_text, &rules_text).map_err(|e| format!("{e} (read from {stores_from} / {rules_from})"))?;
        c.stores_from = stores_from;
        c.rules_from = rules_from;
        Ok(c)
    }
}

fn read_config(data_dir: &Path, name: &str, built_in: &str) -> Result<(String, String), String> {
    for dir in [data_dir.to_path_buf(), PathBuf::from(REPO_DATA)] {
        let p = dir.join(name);
        if p.is_file() {
            let text = std::fs::read_to_string(&p).map_err(|e| format!("can't read {}: {e}", p.display()))?;
            let shown = std::fs::canonicalize(&p).unwrap_or(p);
            let shown = shown.display().to_string();
            return Ok((text.trim_start_matches('\u{feff}').to_string(), shown.trim_start_matches(r"\\?\").to_string()));
        }
    }
    Ok((built_in.to_string(), "built in".into()))
}

/// FNV-1a, as hex. Only used to notice that a rules file changed.
pub fn fingerprint(text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_config_parses() {
        let c = Config::built_in().unwrap();
        let pandora = c.stores.get("pandora-us").expect("pandora-us is configured");
        assert_eq!(pandora.source, stores::SourceKind::BrowserCapture);
        assert!(pandora.start_urls.iter().any(|u| u == "https://us.pandora.net/en/charms/"));
        assert_eq!(c.stores.get("hardjewelry").unwrap().source, stores::SourceKind::Shopify);
        assert_eq!(c.stores.image_hosts(), vec!["cdn.shopify.com", "us.pandora.net", "www.hardjewelry.com"]);
    }

    /// The shipped rules on realistic Pandora names.
    #[test]
    fn shipped_rules_classify_pandora_products() {
        let r = Config::built_in().unwrap().rules;
        let classify = |id: &str, name: &str, badges: &[&str]| {
            let mut p = model::Product { store: "pandora-us".into(), id: id.into(), name: name.into(), ..Default::default() };
            if !badges.is_empty() {
                p.badges = Some(badges.iter().map(|b| b.to_string()).collect());
            }
            r.classify(&mut p);
            (p.kind.unwrap_or_default(), p.system.unwrap_or_default(), p.metal_group.unwrap_or_default())
        };
        let s = |a: &str, b: &str, c: &str| (a.to_string(), b.to_string(), c.to_string());
        assert_eq!(classify("590702", "Pandora Moments Snake Chain Bracelet", &[]), s("bracelet", "pandora-moments", "Silver"));
        assert_eq!(classify("583050C00", "Pandora Moments Heart Clasp Snake Chain Bracelet", &[]), s("bracelet", "pandora-moments", "Rose gold"));
        assert_eq!(classify("597602", "Pandora Reflexions Mesh Bracelet", &[]), s("bracelet", "pandora-reflexions", "Silver"));
        assert_eq!(classify("791088", "Band of Hearts Safety Chain Clip Charm", &[]), s("safety-chain", "pandora-moments", "Silver"));
        assert_eq!(classify("797602", "Sparkling Heart Clip Charm", &[]), s("clip", "pandora-moments", "Silver"));
        assert_eq!(classify("763369C01", "Pandora ME Anchor Mini Dangle Charm", &[]), s("charm", "pandora-me", "Gold"));
        assert_eq!(classify("793448C01", "Gradient Planet Mini Dangle Charm", &["NEW - COMPATIBLE WITH ME & MOMENTS"]), s("charm", "pandora-me-moments", "Silver"));
        assert_eq!(classify("794948C01", "Engagement Ring Dangle Charm", &[]).0, "charm");
        assert_eq!(classify("190000C01", "Sparkling Halo Ring", &[]).0, "ring");
        assert_eq!(classify("290000C01", "Heart Hoop Earrings", &[]).0, "earring");
        assert_eq!(classify("799999", "Mystery", &[]).0, "charm", "style number 7 = charm");
        assert!(matches!(r.fit(Some("pandora-me-moments"), Some("pandora-moments")), rules::Fit::Yes { .. }));
        assert!(matches!(r.fit(Some("pandora-me"), Some("pandora-moments")), rules::Fit::No { .. }));
        assert!(matches!(r.fit(Some("pandora-reflexions"), Some("pandora-moments")), rules::Fit::No { .. }));
        assert_eq!(r.todo_count(), 0, "every shipped rule is sourced");
    }

    #[test]
    fn fingerprint_changes_with_text() {
        assert_ne!(fingerprint("a"), fingerprint("b"));
        assert_eq!(fingerprint("rules"), fingerprint("rules"));
    }
}
