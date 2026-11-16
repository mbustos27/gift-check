//! The store allowlist (`data/stores.toml`): which stores gift-check knows, where their pages and
//! images live, and how their listings get in (a direct Shopify fetch or a browser capture).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceKind {
    /// Public `/products.json`, fetched politely over plain HTTPS.
    Shopify,
    /// Behind a bot check: captured in a real browser and sent through the bridge or a file.
    BrowserCapture,
}

impl SourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SourceKind::Shopify => "shopify",
            SourceKind::BrowserCapture => "browser-capture",
        }
    }
}

fn usd() -> String {
    "USD".into()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoreCfg {
    pub id: String,
    pub name: String,
    /// "https://host", nothing after the host.
    pub origin: String,
    pub source: SourceKind,
    #[serde(default = "usd")]
    pub currency: String,
    /// Hosts its product images may come from (besides its own origin).
    #[serde(default)]
    pub image_hosts: Vec<String>,
    /// Pages the bridge opens for a browser capture.
    #[serde(default)]
    pub start_urls: Vec<String>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stores {
    #[serde(default, rename = "store")]
    pub stores: Vec<StoreCfg>,
}

impl Stores {
    pub fn parse(text: &str) -> Result<Stores, String> {
        let s: Stores = toml::from_str(text).map_err(|e| format!("stores.toml: {e}"))?;
        s.validate()?;
        Ok(s)
    }

    fn validate(&self) -> Result<(), String> {
        let mut seen = std::collections::HashSet::new();
        for s in &self.stores {
            let at = |m: &str| format!("stores.toml, store {:?}: {m}", s.id);
            if !valid_id(&s.id) || s.id.len() > 40 {
                return Err(at("id must be 1-40 characters a-z, 0-9 and -"));
            }
            if !seen.insert(s.id.clone()) {
                return Err(at("listed twice"));
            }
            if s.name.trim().is_empty() || s.name.chars().count() > 80 {
                return Err(at("name must be 1-80 characters"));
            }
            let host = origin_host(&s.origin).ok_or_else(|| at("origin must be https://host with nothing after it"))?;
            if !valid_currency(&s.currency) {
                return Err(at("currency must be three capital letters, like USD"));
            }
            if let Some(bad) = s.image_hosts.iter().find(|h| !valid_host(h)) {
                return Err(at(&format!("image host {bad:?} is not a plain host name")));
            }
            if let Some(bad) = s.start_urls.iter().find(|u| https_host(u).as_deref() != Some(host.as_str())) {
                return Err(at(&format!("start URL {bad:?} must be https on {host}")));
            }
        }
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<&StoreCfg> {
        self.stores.iter().find(|s| s.id == id)
    }

    pub fn by_host(&self, host: &str) -> Option<&StoreCfg> {
        self.stores.iter().find(|s| s.host().eq_ignore_ascii_case(host))
    }

    /// Every host a product image may come from: each store's own host and its image hosts.
    pub fn image_hosts(&self) -> Vec<String> {
        let mut all: Vec<String> = self.stores.iter().flat_map(|s| s.image_hosts.iter().cloned().chain([s.host().to_string()])).collect();
        all.sort();
        all.dedup();
        all
    }
}

impl StoreCfg {
    pub fn host(&self) -> &str {
        self.origin.strip_prefix("https://").unwrap_or(&self.origin)
    }

    /// A product page or capture source: https on the store's own host.
    pub fn owns_url(&self, url: &str) -> bool {
        https_host(url).is_some_and(|h| h == self.host())
    }

    /// An image: https on the store's host or one of its image hosts.
    pub fn allows_image(&self, url: &str) -> bool {
        https_host(url).is_some_and(|h| h == self.host() || self.image_hosts.contains(&h))
    }

    /// A store gift-check met only on the command line (`fetch shopify <domain> --currency C`).
    /// Its currency is always stated: prices in different currencies are never added up.
    pub fn ad_hoc_shopify(host: &str, currency: &str) -> StoreCfg {
        StoreCfg {
            id: host.to_string(),
            name: host.to_string(),
            origin: format!("https://{host}"),
            source: SourceKind::Shopify,
            currency: currency.to_string(),
            image_hosts: vec!["cdn.shopify.com".into()],
            start_urls: vec![],
            note: None,
        }
    }
}

/// Lower-case letters, digits, '.', '-'; at least one dot; labels not starting or ending in '-'.
pub fn valid_host(h: &str) -> bool {
    !h.is_empty()
        && h.len() <= 253
        && h.contains('.')
        && h.split('.').all(|l| {
            !l.is_empty() && l.len() <= 63 && !l.starts_with('-') && !l.ends_with('-') && l.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

/// Store ids: a-z, 0-9, '-' and '.'.
pub fn valid_id(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
}

pub fn valid_currency(s: &str) -> bool {
    s.len() == 3 && s.bytes().all(|b| b.is_ascii_uppercase())
}

/// The host of an `https://host/...` URL, lower-cased. `None` for anything else: another scheme,
/// a user name or port in the address, spaces, backslashes or control characters.
pub fn https_host(url: &str) -> Option<String> {
    if url.len() > 2048 || url.chars().any(|c| c.is_whitespace() || c.is_control() || c == '\\') {
        return None;
    }
    let scheme = url.get(..8)?;
    if !scheme.eq_ignore_ascii_case("https://") {
        return None;
    }
    let rest = &url[8..];
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let host = rest[..end].to_ascii_lowercase();
    valid_host(&host).then_some(host)
}

/// The host of an origin "https://host" (no path, no trailing slash).
pub fn origin_host(origin: &str) -> Option<String> {
    let host = https_host(origin)?;
    (origin.len() == 8 + host.len()).then_some(host)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_host_refuses_tricks() {
        assert_eq!(https_host("https://us.pandora.net/en/charms/").as_deref(), Some("us.pandora.net"));
        assert_eq!(https_host("HTTPS://US.Pandora.net?x").as_deref(), Some("us.pandora.net"));
        for bad in [
            "http://us.pandora.net/",
            "https://evil.example@us.pandora.net/",
            "https://us.pandora.net:444/",
            "https://us.pandora.net\\@evil/",
            "https:// us.pandora.net/",
            "javascript:alert(1)",
            "//us.pandora.net/x",
            "https://localhost/",
        ] {
            assert_eq!(https_host(bad), None, "{bad}");
        }
        assert_eq!(origin_host("https://us.pandora.net").as_deref(), Some("us.pandora.net"));
        assert_eq!(origin_host("https://us.pandora.net/"), None);
    }

    #[test]
    fn bad_store_files_are_refused() {
        let base = "[[store]]\nid=\"a\"\nname=\"A\"\norigin=\"https://a.example\"\nsource=\"shopify\"\n";
        assert!(Stores::parse(base).is_ok());
        assert!(Stores::parse(&base.replace("https://a.example", "http://a.example")).is_err());
        assert!(Stores::parse(&format!("{base}start_urls=[\"https://b.example/x\"]\n")).is_err());
        assert!(Stores::parse(&format!("{base}image_hosts=[\"*.cdn.example\"]\n")).is_err());
        assert!(Stores::parse(&format!("{base}{base}")).is_err(), "duplicate id");
        assert!(Stores::parse(&base.replace("shopify", "scrape")).is_err());
    }
}
