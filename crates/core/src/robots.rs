//! robots.txt per RFC 9309.
//!
//! - A group starts with one or more `user-agent` lines; consecutive `user-agent` lines (blank
//!   lines and comments between them don't break the run) form ONE group.
//! - The groups naming our product token are combined; only when none does are the `*` groups
//!   used (also combined). No matching group means everything is allowed.
//! - The longest matching rule wins; on equal length `allow` beats `disallow`.
//! - `*` matches any run of characters and a final `$` anchors the end of the path.
//! - `/robots.txt` itself is always allowed.

/// Only the first 500 KiB are read, the minimum RFC 9309 asks crawlers to handle.
pub const PARSE_LIMIT: usize = 500 * 1024;

#[derive(Debug, Clone, PartialEq)]
struct Rule {
    allow: bool,
    pattern: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
struct Group {
    agents: Vec<String>,
    rules: Vec<Rule>,
    crawl_delay: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Robots {
    rules: Vec<Rule>,
    crawl_delay: Option<f64>,
    /// Which group applied: "gift-check", "*" or "none".
    pub matched: String,
}

impl Robots {
    /// No robots.txt (a 4xx answer): everything may be fetched.
    pub fn allow_all() -> Robots {
        Robots { rules: vec![], crawl_delay: None, matched: "none".into() }
    }

    pub fn parse(text: &str, token: &str) -> Robots {
        let mut cut = text.len().min(PARSE_LIMIT);
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        let mut groups: Vec<Group> = Vec::new();
        let mut cur: Option<Group> = None;
        // True while the current group has only seen user-agent lines so far.
        let mut collecting_agents = false;
        for raw in text[..cut].lines() {
            let line = raw.split('#').next().unwrap_or("").trim();
            let Some((key, value)) = line.split_once(':') else { continue };
            let key = key.trim().to_ascii_lowercase();
            let value = value.trim();
            match key.as_str() {
                "user-agent" | "useragent" | "user agent" => {
                    if !collecting_agents {
                        if let Some(g) = cur.take() {
                            groups.push(g);
                        }
                        cur = Some(Group::default());
                        collecting_agents = true;
                    }
                    if let Some(g) = cur.as_mut() {
                        g.agents.push(value.to_ascii_lowercase());
                    }
                }
                "allow" | "disallow" => {
                    collecting_agents = false;
                    // Rules before any user-agent line belong to no group.
                    if let Some(g) = cur.as_mut() {
                        if !value.is_empty() {
                            g.rules.push(Rule { allow: key == "allow", pattern: normalize(value) });
                        }
                    }
                }
                "crawl-delay" => {
                    collecting_agents = false;
                    if let (Some(g), Ok(d)) = (cur.as_mut(), value.parse::<f64>()) {
                        if d.is_finite() && d >= 0.0 {
                            g.crawl_delay = Some(d);
                        }
                    }
                }
                // sitemap and unknown keys don't end a run of user-agent lines' group membership,
                // but they are not rules either.
                _ => {}
            }
        }
        if let Some(g) = cur.take() {
            groups.push(g);
        }
        let token = token.to_ascii_lowercase();
        let names_us = |a: &str| {
            let a = a.split(['/', ' ']).next().unwrap_or("");
            a == token
        };
        let pick = |pred: &dyn Fn(&str) -> bool| -> Option<(Vec<Rule>, Option<f64>)> {
            let chosen: Vec<&Group> = groups.iter().filter(|g| g.agents.iter().any(|a| pred(a))).collect();
            if chosen.is_empty() {
                return None;
            }
            let rules = chosen.iter().flat_map(|g| g.rules.iter().cloned()).collect();
            let delay = chosen.iter().filter_map(|g| g.crawl_delay).fold(None, |m: Option<f64>, d| Some(m.map_or(d, |x| x.max(d))));
            Some((rules, delay))
        };
        if let Some((rules, crawl_delay)) = pick(&names_us) {
            return Robots { rules, crawl_delay, matched: token };
        }
        if let Some((rules, crawl_delay)) = pick(&|a: &str| a == "*") {
            return Robots { rules, crawl_delay, matched: "*".into() };
        }
        Robots::allow_all()
    }

    /// May this path (with its query string) be fetched?
    pub fn allowed(&self, path_and_query: &str) -> bool {
        let path = normalize(path_and_query);
        if path == "/robots.txt" {
            return true;
        }
        let mut best: Option<(usize, bool)> = None;
        for r in &self.rules {
            if matches(&r.pattern, &path) {
                let len = r.pattern.len();
                best = match best {
                    Some((l, a)) if l > len || (l == len && a) => Some((l, a)),
                    _ => Some((len, r.allow)),
                };
            }
        }
        best.is_none_or(|(_, allow)| allow)
    }

    /// The `crawl-delay` of the matched group, in seconds (not part of RFC 9309, but honoured).
    pub fn crawl_delay(&self) -> Option<f64> {
        self.crawl_delay
    }
}

/// Upper-case percent escapes so "%2f" and "%2F" compare equal; non-ASCII is percent-encoded.
fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && b.get(i + 1).is_some_and(u8::is_ascii_hexdigit) && b.get(i + 2).is_some_and(u8::is_ascii_hexdigit) {
            out.push('%');
            out.push((b[i + 1] as char).to_ascii_uppercase());
            out.push((b[i + 2] as char).to_ascii_uppercase());
            i += 3;
        } else if b[i].is_ascii() {
            out.push(b[i] as char);
            i += 1;
        } else {
            out.push_str(&format!("%{:02X}", b[i]));
            i += 1;
        }
    }
    out
}

/// `*` matches any run of characters; a final `$` anchors the end; otherwise a prefix match.
fn matches(pattern: &str, path: &str) -> bool {
    let (pat, anchored) = match pattern.strip_suffix('$') {
        Some(p) => (p, true),
        None => (pattern, false),
    };
    let pieces: Vec<&str> = pat.split('*').collect();
    let mut pos = 0;
    for (i, piece) in pieces.iter().enumerate() {
        if i == 0 {
            if !path.starts_with(piece) {
                return false;
            }
            pos = piece.len();
            continue;
        }
        let last = i == pieces.len() - 1;
        if last && anchored {
            // The final piece must sit at the very end, after `pos`.
            return path.len() >= pos + piece.len() && path.ends_with(piece);
        }
        match path[pos..].find(piece) {
            Some(at) => pos += at + piece.len(),
            None => return false,
        }
    }
    !anchored || pos == path.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    const UA: &str = "gift-check";

    #[test]
    fn consecutive_user_agents_form_one_group() {
        // A parser that only keeps the last user-agent line of a run would give gift-check the
        // `*` group and refuse /products.json.
        let txt = "User-agent: gift-check\n# a comment\n\nUser-agent: OtherBot\nDisallow: /private\n\nUser-agent: *\nDisallow: /\n";
        let r = Robots::parse(txt, UA);
        assert_eq!(r.matched, "gift-check");
        assert!(r.allowed("/products.json?limit=250&page=1"));
        assert!(!r.allowed("/private/x"));
        // And the other bot in the same group shares those rules.
        let other = Robots::parse(txt, "otherbot");
        assert!(!other.allowed("/private"));
        assert!(other.allowed("/products.json"));
    }

    #[test]
    fn groups_for_us_are_combined_and_star_is_ignored_then() {
        let txt = "User-agent: Gift-Check\nDisallow: /a\n\nUser-agent: *\nDisallow: /\n\nuser-agent: gift-check/0.1\nDisallow: /b\n";
        let r = Robots::parse(txt, UA);
        assert!(!r.allowed("/a1") && !r.allowed("/b1"));
        assert!(r.allowed("/products.json"));
    }

    #[test]
    fn star_group_applies_when_none_names_us_and_no_group_allows_all() {
        let r = Robots::parse("User-agent: *\nDisallow: /cart\nDisallow: /checkout\n", UA);
        assert_eq!(r.matched, "*");
        assert!(!r.allowed("/cart"));
        assert!(r.allowed("/products.json"));
        let none = Robots::parse("User-agent: googlebot\nDisallow: /\n", UA);
        assert!(none.allowed("/anything"));
        // Rules before any user-agent line belong to nobody.
        assert!(Robots::parse("Disallow: /\n", UA).allowed("/x"));
    }

    #[test]
    fn wildcards_and_end_anchor() {
        let txt = "User-agent: *\nDisallow: /*.json$\nDisallow: /collections/*sort_by*\nAllow: /collections/*/products.json$\nDisallow: /search\n";
        let r = Robots::parse(txt, UA);
        assert!(!r.allowed("/products.json"), "$ anchors the end");
        assert!(r.allowed("/products.json?limit=250&page=1"), "with a query the path no longer ends in .json");
        assert!(!r.allowed("/collections/charms?sort_by=price"));
        assert!(r.allowed("/collections/charms/products.json"), "the longer allow wins");
        assert!(!r.allowed("/search?q=x"));
        assert!(r.allowed("/robots.txt"));
        assert!(matches("/a*b*c$", "/a-x-b-y-c"));
        assert!(!matches("/a*b*c$", "/a-x-b-y-c-d"));
        assert!(matches("/*", "/"));
        assert!(matches("/fish*", "/fish.html"));
        assert!(!matches("/fish$", "/fish/"));
    }

    #[test]
    fn longest_match_wins_and_allow_wins_ties() {
        let r = Robots::parse("User-agent: *\nAllow: /p\nDisallow: /\n", UA);
        assert!(r.allowed("/page"));
        assert!(!r.allowed("/x"));
        let tie = Robots::parse("User-agent: *\nDisallow: /folder\nAllow: /folder\n", UA);
        assert!(tie.allowed("/folder/page"));
        let longer = Robots::parse("User-agent: *\nAllow: /folder\nDisallow: /folder/private\n", UA);
        assert!(!longer.allowed("/folder/private/x"));
        // An empty disallow means nothing is disallowed.
        assert!(Robots::parse("User-agent: *\nDisallow:\n", UA).allowed("/x"));
    }

    #[test]
    fn percent_escapes_and_crawl_delay() {
        let r = Robots::parse("User-agent: gift-check\nDisallow: /collections/*%2b*\nCrawl-delay: 5\n", UA);
        assert!(!r.allowed("/collections/a%2Bb"));
        assert_eq!(r.crawl_delay(), Some(5.0));
    }
}
