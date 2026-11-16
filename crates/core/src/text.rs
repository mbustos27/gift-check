//! Text from stores is data. These helpers turn it into bounded plain text; nothing here produces
//! markup, and the window shows the results with `textContent` only.

/// Plain text from an HTML fragment: tags are dropped (and `script`/`style`/`template` with their
/// contents), entities decoded, whitespace collapsed. A decoded `&lt;` becomes a literal `<` in
/// the text, which is harmless because the text is never parsed as HTML again.
pub fn strip_html(html: &str) -> String {
    let lower = html.to_ascii_lowercase(); // same byte offsets as `html`
    let mut out = String::with_capacity(html.len());
    let mut i = 0;
    while i < html.len() {
        let rest = &html[i..];
        let c = rest.chars().next().unwrap_or(' ');
        if c == '<' {
            let next = rest[1..].chars().next();
            let starts_tag = next.is_some_and(|n| n.is_ascii_alphabetic() || n == '/' || n == '!' || n == '?');
            if starts_tag {
                let lrest = &lower[i..];
                if lrest.starts_with("<!--") {
                    match lrest.find("-->") {
                        Some(e) => {
                            i += e + 3;
                            continue;
                        }
                        None => break,
                    }
                }
                let Some(end) = lrest.find('>') else { break };
                let inner = &lrest[1..end];
                let closing = inner.starts_with('/');
                let name: String = inner.trim_start_matches('/').chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
                if !closing && matches!(name.as_str(), "script" | "style" | "template" | "noscript") {
                    let close = format!("</{name}");
                    let Some(p) = lower[i + end..].find(&close) else { break };
                    let after = i + end + p;
                    match lower[after..].find('>') {
                        Some(q) => i = after + q + 1,
                        None => break,
                    }
                    out.push(' ');
                    continue;
                }
                // Block-level tags separate words.
                out.push(' ');
                i += end + 1;
                continue;
            }
        }
        if c == '&' {
            if let Some((decoded, used)) = entity(rest) {
                out.push_str(&decoded);
                i += used;
                continue;
            }
        }
        out.push(c);
        i += c.len_utf8();
    }
    clean(&out)
}

/// Decodes one entity at the start of `s` ("&amp;", "&#39;", "&#x27;"), returning the text and
/// how many bytes it used.
fn entity(s: &str) -> Option<(String, usize)> {
    let end = s.char_indices().take(12).find(|(_, c)| *c == ';').map(|(i, _)| i)?;
    let body = &s[1..end];
    let text = if let Some(num) = body.strip_prefix('#') {
        let code = match num.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => num.parse::<u32>().ok()?,
        };
        let ch = char::from_u32(code).filter(|c| !c.is_control() || c.is_whitespace())?;
        ch.to_string()
    } else {
        let named = match body {
            "amp" => "&",
            "lt" => "<",
            "gt" => ">",
            "quot" => "\"",
            "apos" => "'",
            "nbsp" | "ensp" | "emsp" | "thinsp" => " ",
            "hellip" => "…",
            "mdash" => "—",
            "ndash" => "–",
            "lsquo" => "‘",
            "rsquo" => "’",
            "ldquo" => "“",
            "rdquo" => "”",
            "trade" => "™",
            "reg" => "®",
            "copy" => "©",
            "deg" => "°",
            "middot" => "·",
            "bull" => "•",
            "times" => "×",
            _ => return None,
        };
        named.to_string()
    };
    Some((text, end + 1))
}

/// Control characters removed, runs of whitespace collapsed to one space, trimmed.
pub fn clean(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for c in s.chars() {
        if c.is_whitespace() || c.is_control() {
            space = !out.is_empty();
            continue;
        }
        if space {
            out.push(' ');
            space = false;
        }
        out.push(c);
    }
    out
}

/// At most `max` characters, ending in "…" when cut.
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// True when `phrase` occurs in `hay` as whole words, ignoring case; a trailing "s" or "es" on the
/// last word still counts ("Bracelet" matches "Bracelets", "Charm" does not match "Charming").
pub fn has_words(hay: &str, phrase: &str) -> bool {
    let h = hay.to_lowercase();
    let p = phrase.trim().to_lowercase();
    if p.is_empty() {
        return false;
    }
    let ends_word = |rest: &str| rest.chars().next().is_none_or(|c| !c.is_alphanumeric());
    let mut start = 0;
    while let Some(pos) = h[start..].find(&p) {
        let at = start + pos;
        let before_ok = h[..at].chars().next_back().is_none_or(|c| !c.is_alphanumeric());
        let after = &h[at + p.len()..];
        let after_ok = ends_word(after)
            || after.strip_prefix("es").is_some_and(ends_word)
            || after.strip_prefix('s').is_some_and(ends_word);
        if before_ok && after_ok {
            return true;
        }
        start = at + p.chars().next().map_or(1, char::len_utf8);
    }
    false
}

/// Like [`has_words`], but a plural phrase also matches its singular ("cats" matches "Cat Charm").
pub fn has_words_loose(hay: &str, phrase: &str) -> bool {
    let p = phrase.trim();
    if has_words(hay, p) {
        return true;
    }
    let lower = p.to_lowercase();
    let stem = lower.strip_suffix("es").filter(|s| s.ends_with(['s', 'x', 'z']) || s.ends_with("ch") || s.ends_with("sh")).or_else(|| lower.strip_suffix('s'));
    stem.is_some_and(|s| s.chars().count() >= 3 && has_words(hay, s))
}

/// Escapes `%`, `_` and `\` for a SQL `LIKE … ESCAPE '\'` pattern.
pub fn like_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_tags_scripts_and_decodes_entities() {
        let html = "<p>Sterling&nbsp;silver <b>heart</b></p><script>alert('x')</script><style>p{}</style><ul><li>Fits &amp; clips</li></ul><!-- hidden -->&lt;3";
        assert_eq!(strip_html(html), "Sterling silver heart Fits & clips <3");
        assert_eq!(strip_html("5 < 6 and 7 > 2"), "5 < 6 and 7 > 2");
        assert_eq!(strip_html("<SCRIPT type=x>bad()</SCRIPT>ok"), "ok");
        assert_eq!(strip_html("broken <b"), "broken");
        assert_eq!(strip_html("&#39;quoted&#x27; &bogus; &#0;"), "'quoted' &bogus; &#0;");
    }

    #[test]
    fn whole_word_matching() {
        assert!(has_words("Pandora Moments Snake Chain Bracelet", "bracelet"));
        assert!(has_words("Two Bracelets", "Bracelet"));
        assert!(has_words("Safety Chain Clip Charm", "safety chain"));
        assert!(!has_words("Charming", "charm"));
        assert!(!has_words("Earrings", "ring"));
        assert!(has_words("Hoop Earrings", "earring"));
        assert!(!has_words("anything", ""));
        assert!(has_words_loose("Lucky Cat Charm", "cats"));
        assert!(has_words_loose("Wine Glass Charm", "glasses"));
        assert!(!has_words_loose("Catalog", "cats"));
        assert!(!has_words_loose("Cat", "is"));
    }

    #[test]
    fn cleaning_and_truncating() {
        assert_eq!(clean("  a\u{0}\tb\n\n c "), "a b c");
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("abc", 4), "abc");
        assert_eq!(like_escape("50%_off\\"), "50\\%\\_off\\\\");
    }
}
