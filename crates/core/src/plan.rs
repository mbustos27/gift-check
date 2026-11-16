//! Building a gift: checking a chosen set of items against the rules and the recipient
//! ([`evaluate`]), and suggesting a bracelet plus add-ons within a budget ([`suggest`]).

use crate::model::{Product, Recipient};
use crate::rules::{Fit, Rules, SizeAdvice};
use crate::text::{has_words, has_words_loose};
use serde::Serialize;

/// A price seen longer ago than this gets a "re-check before buying" warning.
pub const STALE_PRICE_DAYS: i64 = 7;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Warning {
    /// "stop" (don't buy as is), "warn" (check first) or "note".
    pub level: &'static str,
    pub text: String,
    /// "store:id" of the item it is about.
    pub item: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Line {
    pub store: String,
    pub id: String,
    pub name: String,
    pub kind: Option<String>,
    pub system: Option<String>,
    pub system_label: Option<String>,
    /// The system is a guess from a fallback rule, not named on the listing.
    pub system_inferred: bool,
    pub metal_group: Option<String>,
    pub url: String,
    pub image: Option<String>,
    pub qty: u32,
    pub currency: String,
    pub price: Option<f64>,
    pub list_price: Option<f64>,
    /// When the price was observed.
    pub price_at: Option<String>,
    /// Which variant the price is for ("17 cm", "any size", ...).
    pub priced: String,
    pub pct_off: Option<f64>,
    pub in_stock: Option<bool>,
    pub discontinued: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Evaluation {
    pub lines: Vec<Line>,
    /// At current prices.
    pub total: f64,
    /// At list prices.
    pub list_total: f64,
    pub savings: f64,
    pub unknown_prices: usize,
    pub budget: Option<f64>,
    pub left: Option<f64>,
    /// The plan's currency (the first item's; a plan mixing currencies gets a "stop").
    pub currency: Option<String>,
    /// The oldest price observation in the plan.
    pub oldest_price_at: Option<String>,
    pub warnings: Vec<Warning>,
    pub size: Option<SizeAdvice>,
    pub notes: Vec<String>,
}

fn money(v: f64) -> String {
    format!("${v:.2}")
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// Which variant a product's price is for. Shopify products carry the cheapest buyable variant's
/// price; browser captures usually have no variants (the listing shows one price).
pub fn priced_variant(p: &Product) -> String {
    let Some(cur) = p.current() else { return "no price known".into() };
    let priced: Vec<_> = p.variants.iter().filter(|v| v.price.is_some()).collect();
    if priced.is_empty() {
        return "the price on the listing".into();
    }
    let label = |v: &crate::model::Variant| v.title.clone().or_else(|| v.sku.clone()).unwrap_or_else(|| format!("variant {}", v.id));
    if priced.iter().all(|v| v.price.is_some_and(|x| (x - cur).abs() < 0.005)) {
        return if priced.len() == 1 { label(priced[0]) } else { format!("any of its {} variants (same price)", priced.len()) };
    }
    match priced.iter().find(|v| v.available != Some(false) && v.price.is_some_and(|x| (x - cur).abs() < 0.005)) {
        Some(v) => format!("{} (variants differ in price; check the one you pick)", label(v)),
        None => "the product price (no variant has exactly this price; check the page)".into(),
    }
}

fn line(p: &Product, qty: u32, rules: &Rules) -> Line {
    Line {
        store: p.store.clone(),
        id: p.id.clone(),
        name: p.name.clone(),
        kind: p.kind.clone(),
        system: p.system.clone(),
        system_label: p.system.as_deref().map(|s| rules.system_label(s)),
        system_inferred: p.system_inferred,
        metal_group: p.metal_group.clone(),
        url: p.url.clone(),
        image: p.image.clone(),
        qty,
        currency: p.currency.clone(),
        price: p.current(),
        list_price: p.regular(),
        price_at: p.price_at.clone(),
        priced: priced_variant(p),
        pct_off: p.pct_off(),
        in_stock: p.in_stock,
        discontinued: p.discontinued,
    }
}

/// Systems named in a recipient's profile that the rules know as bracelet systems.
fn owned_base_systems<'a>(r: &'a Recipient, rules: &Rules) -> Vec<&'a str> {
    r.systems.iter().map(String::as_str).filter(|s| rules.fits.iter().any(|f| f.base_systems.iter().any(|b| b == s))).collect()
}

/// Checks a chosen set of items: totals at current and list prices, and every warning the rules
/// and the recipient's profile give.
pub fn evaluate(items: &[(Product, u32)], recipient: Option<&Recipient>, budget: Option<f64>, rules: &Rules) -> Evaluation {
    evaluate_at(items, recipient, budget, rules, crate::time::now())
}

/// [`evaluate`] as of `now` (Unix seconds), which decides what counts as an old price.
pub fn evaluate_at(items: &[(Product, u32)], recipient: Option<&Recipient>, budget: Option<f64>, rules: &Rules, now: i64) -> Evaluation {
    let mut w: Vec<Warning> = Vec::new();
    let mut add = |level: &'static str, text: String, p: Option<&Product>| w.push(Warning { level, text, item: p.map(Product::key) });
    let currency = items.first().map(|(p, _)| p.currency.clone());
    let mut currencies: Vec<&str> = items.iter().map(|(p, _)| p.currency.as_str()).collect();
    currencies.sort_unstable();
    currencies.dedup();
    if currencies.len() > 1 {
        add("stop", format!("The plan mixes currencies ({}); the totals mean nothing. Keep one store's currency per plan.", currencies.join(" and ")), None);
    }
    let (mut total, mut list_total, mut unknown) = (0.0, 0.0, 0);
    let mut oldest: Option<String> = None;
    for (p, qty) in items {
        match p.current() {
            Some(c) => {
                total += c * f64::from(*qty);
                list_total += p.regular().unwrap_or(c) * f64::from(*qty);
            }
            None => {
                unknown += 1;
                add("warn", format!("{} has no known price; the totals leave it out.", p.name), Some(p));
            }
        }
        if let Some(at) = p.price_at.as_deref() {
            if oldest.as_deref().is_none_or(|o| at < o) {
                oldest = Some(at.to_string());
            }
            if let Some(t) = crate::time::parse_rfc3339(at).filter(|t| now - t > STALE_PRICE_DAYS * 86_400) {
                add("warn", format!("{}'s price was seen {} ago ({}); capture the store again before buying.", p.name, crate::time::ago(now - t), &at[..10]), Some(p));
            }
        }
    }
    let budget = budget.or_else(|| recipient.and_then(|r| r.budget));
    if let Some(b) = budget {
        if total > b + 0.004 {
            add("stop", format!("Over budget by {} ({} for a {} budget).", money(total - b), money(total), money(b)), None);
        }
    }

    let bases: Vec<&Product> = items.iter().map(|(p, _)| p).filter(|p| rules.is_base(p.kind.as_deref())).collect();
    if bases.len() > 1 {
        add("warn", format!("{} bracelets in one plan; add-ons are checked against the first, {}.", bases.len(), bases[0].name), None);
    }
    let base = bases.first().copied();
    let owned_systems: Vec<&str> = recipient.map(|r| owned_base_systems(r, rules)).unwrap_or_default();
    let nick = recipient.map_or("they", |r| r.nick.as_str());
    if let Some(b) = base.filter(|b| b.system.is_none()) {
        if items.iter().any(|(p, _)| rules.is_addon(p.kind.as_deref())) {
            add("warn", format!("{} isn't recognised as a charm bracelet (nothing names its charm system); charms may not fit it. Check the product page.", b.name), Some(b));
        }
    }
    let inferred = |p: &Product, label: String| format!("{}: fit inferred ({label} is a guess, the listing names no system); check the product page.", p.name);

    let mut seen: Vec<String> = Vec::new();
    for (p, _) in items {
        let key = p.key();
        if seen.contains(&key) {
            add("warn", format!("{} is in the plan twice.", p.name), Some(p));
        }
        seen.push(key);
        if p.in_stock == Some(false) {
            add("stop", format!("{} is out of stock.", p.name), Some(p));
        }
        if p.discontinued {
            add("warn", format!("{} wasn't in the latest complete capture; it may be discontinued.", p.name), Some(p));
        }
        if recipient.is_some_and(|r| r.owns(p)) {
            add("warn", format!("{nick} already owns {}.", p.name), Some(p));
        }
        for (text, source) in rules.policy_warnings(p) {
            add("warn", format!("{}: {text} (source: {source})", p.name), Some(p));
        }
        if let (Some(want), Some(have)) = (recipient.and_then(|r| r.metal.as_deref()), p.metal_group.as_deref()) {
            if !want.eq_ignore_ascii_case(have) {
                add("note", format!("{} is {have}; {nick} prefers {want}.", p.name), Some(p));
            }
        }
        if !rules.is_addon(p.kind.as_deref()) {
            continue;
        }
        let label = |s: Option<&str>| s.map_or_else(|| "unknown system".to_string(), |s| rules.system_label(s));
        match base {
            Some(b) => match rules.fit(p.system.as_deref(), b.system.as_deref()) {
                Fit::Yes { .. } if p.system_inferred => add("warn", inferred(p, label(p.system.as_deref())), Some(p)),
                Fit::Yes { .. } => {}
                Fit::No { source, note } => {
                    let why = if note.is_empty() { String::new() } else { format!(" {note}") };
                    add("stop", format!("{} ({}) doesn't fit the {} bracelet.{why} (source: {source})", p.name, label(p.system.as_deref()), label(b.system.as_deref())), Some(p));
                }
                Fit::Unknown => add("warn", format!("Can't confirm {} ({}) fits the {} bracelet: no rule covers it.", p.name, label(p.system.as_deref()), label(b.system.as_deref())), Some(p)),
            },
            None if !owned_systems.is_empty() => {
                let fits: Vec<Fit> = owned_systems.iter().map(|s| rules.fit(p.system.as_deref(), Some(s))).collect();
                let any_yes = fits.iter().any(|f| matches!(f, Fit::Yes { .. }));
                if fits.iter().all(|f| matches!(f, Fit::No { .. })) {
                    let names: Vec<String> = owned_systems.iter().map(|s| rules.system_label(s)).collect();
                    add("stop", format!("{} ({}) doesn't fit the bracelet {nick} has ({}).", p.name, label(p.system.as_deref()), names.join(", ")), Some(p));
                } else if !any_yes {
                    add("warn", format!("Can't confirm {} ({}) fits the bracelet {nick} has.", p.name, label(p.system.as_deref())), Some(p));
                } else if p.system_inferred {
                    add("warn", inferred(p, label(p.system.as_deref())), Some(p));
                }
            }
            None => add("note", format!("No bracelet in the plan and none recorded for {nick}, so {} can't be checked for fit.", p.name), Some(p)),
        }
    }

    // Per-kind limits (two clips on a snake chain, one safety chain).
    for (kind, max) in &rules.build.limits {
        let n: u32 = items.iter().filter(|(p, _)| p.kind.as_deref() == Some(kind)).map(|(_, q)| *q).sum();
        if n > *max {
            let src = if rules.build.limits_source.is_empty() { String::new() } else { format!(" (source: {})", rules.build.limits_source) };
            add("warn", format!("{n} × {kind}; one bracelet takes {max}{src}."), None);
        }
    }

    let size = match (base, recipient.and_then(|r| r.wrist_cm)) {
        (Some(b), Some(wrist)) => b.system.as_deref().and_then(|s| rules.bracelet_size(&b.store, s, wrist)),
        _ => None,
    };
    let store = base.map(|b| b.store.as_str()).or_else(|| items.first().map(|(p, _)| p.store.as_str()));
    let notes = rules.notes_for(store).into_iter().filter(|n| matches!(n.topic.as_str(), "clips" | "sizing" | "fit")).map(|n| format!("{} (source: {})", n.text, n.source)).collect();

    let total = round2(total);
    let list_total = round2(list_total);
    Evaluation {
        lines: items.iter().map(|(p, q)| line(p, *q, rules)).collect(),
        total,
        list_total,
        savings: round2(list_total - total),
        unknown_prices: unknown,
        currency,
        oldest_price_at: oldest,
        budget,
        left: budget.map(|b| round2(b - total)),
        warnings: w,
        size,
        notes,
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Pick {
    pub store: String,
    pub id: String,
    pub name: String,
    pub kind: Option<String>,
    pub system: Option<String>,
    pub metal_group: Option<String>,
    pub currency: String,
    pub price: f64,
    pub list_price: f64,
    pub url: String,
    pub image: Option<String>,
    /// Which variant the price is for.
    pub priced: String,
    /// The fit rests on a system guessed by a fallback rule.
    pub fit_inferred: bool,
    /// Store policies that apply (final sale, engraving), as text.
    pub policies: Vec<String>,
    /// Can't be returned: suggested only when nothing returnable fits.
    pub final_sale: bool,
    pub why: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Suggestion {
    pub title: String,
    pub currency: String,
    pub base: Option<Pick>,
    pub addons: Vec<Pick>,
    pub total: f64,
    pub list_total: f64,
    pub savings: f64,
    pub left: f64,
    pub notes: Vec<String>,
}

fn liked<'a>(p: &Product, r: &'a Recipient) -> Vec<&'a str> {
    let hay = p.haystack();
    r.likes.iter().map(String::as_str).filter(|l| has_words_loose(&hay, l)).collect()
}

fn metal_ok(p: &Product, r: &Recipient) -> bool {
    match (r.metal.as_deref(), p.metal_group.as_deref()) {
        (Some(want), Some(have)) => want.eq_ignore_ascii_case(have),
        _ => true,
    }
}

fn pick(p: &Product, why: Vec<String>, rules: &Rules) -> Pick {
    Pick {
        store: p.store.clone(),
        id: p.id.clone(),
        name: p.name.clone(),
        kind: p.kind.clone(),
        system: p.system.clone(),
        metal_group: p.metal_group.clone(),
        currency: p.currency.clone(),
        price: p.current().unwrap_or(0.0),
        list_price: p.regular().unwrap_or(0.0),
        url: p.url.clone(),
        image: p.image.clone(),
        priced: priced_variant(p),
        fit_inferred: p.system_inferred,
        policies: rules.policy_warnings(p).into_iter().map(|(text, _)| text).collect(),
        final_sale: rules.is_final_sale(p),
        why: why.join("; "),
    }
}

/// Suggests up to `max` gifts within `budget`: a bracelet (or, when the recipient already has a
/// bracelet system, none) plus add-ons the rules say fit it. Only confirmed fits are suggested;
/// owned, out-of-stock and discontinued items are left out. Final-sale items (a policy marked
/// `final_sale`) come last: one is used only when nothing returnable fits, and the notes say so.
/// A bracelet with no add-ons is only offered when no option has add-ons.
pub fn suggest(catalog: &[Product], r: &Recipient, budget: f64, store: Option<&str>, rules: &Rules, max: usize) -> Vec<Suggestion> {
    let eligible = |p: &&Product| {
        !p.discontinued && p.in_stock != Some(false) && p.current().is_some() && store.is_none_or(|s| p.store == s) && !r.owns(p)
    };
    let pool: Vec<&Product> = catalog.iter().filter(eligible).collect();
    let cheaper = |a: &&Product, b: &&Product| a.current().unwrap_or(f64::MAX).total_cmp(&b.current().unwrap_or(f64::MAX));

    // Bases: per system, the best liked one, else the cheapest; matching metal first.
    let mut bases: Vec<&Product> = pool.iter().copied().filter(|p| rules.is_base(p.kind.as_deref()) && p.system.is_some() && !p.system_inferred && p.current().is_some_and(|c| c <= budget)).collect();
    if bases.iter().any(|p| metal_ok(p, r)) {
        bases.retain(|p| metal_ok(p, r));
    }
    bases.sort_by(|a, b| rules.is_final_sale(a).cmp(&rules.is_final_sale(b)).then_with(|| liked(b, r).len().cmp(&liked(a, r).len())).then_with(|| cheaper(a, b)));
    let mut per_system: Vec<&Product> = Vec::new();
    for b in bases {
        if !per_system.iter().any(|x| x.system == b.system) {
            per_system.push(b);
        }
    }

    let mut targets: Vec<(Option<&Product>, String)> = owned_base_systems(r, rules).into_iter().map(|s| (None, s.to_string())).collect();
    targets.extend(per_system.into_iter().map(|b| (Some(b), b.system.clone().unwrap_or_default())));

    let mut out: Vec<(usize, Suggestion)> = Vec::new();
    for (base, system) in targets {
        let label = rules.system_label(&system);
        let mut left = budget - base.and_then(|b| b.current()).unwrap_or(0.0);
        let mut addons: Vec<(&Product, Vec<String>)> = Vec::new();
        let mut candidates: Vec<(&Product, f64, Vec<&str>, String)> = pool
            .iter()
            .copied()
            .filter(|p| rules.is_addon(p.kind.as_deref()))
            .filter_map(|p| match rules.fit(p.system.as_deref(), Some(&system)) {
                Fit::Yes { source } => {
                    let likes = liked(p, r);
                    let score = 10.0 * likes.len() as f64
                        + p.pct_off().unwrap_or(0.0) / 10.0
                        + if metal_ok(p, r) { 3.0 } else { 0.0 }
                        + if p.badges.iter().flatten().any(|b| has_words(b, "top gift")) { 1.0 } else { 0.0 };
                    Some((p, score, likes, source))
                }
                _ => None,
            })
            .collect();
        candidates.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| cheaper(&a.0, &b.0)));
        // One currency per gift: the bracelet's, or (add-ons only) the best candidate's.
        let Some(currency) = base.map(|b| b.currency.clone()).or_else(|| candidates.first().map(|c| c.0.currency.clone())) else { continue };
        candidates.retain(|c| c.0.currency == currency);
        let mut per_kind: std::collections::BTreeMap<String, u32> = Default::default();
        let mut fit_sources: Vec<String> = Vec::new();
        // Returnable add-ons first; final-sale ones only when none of those fits.
        let (returnable, final_sale): (Vec<_>, Vec<_>) = candidates.into_iter().partition(|c| !rules.is_final_sale(c.0));
        for (round, group) in [returnable, final_sale].into_iter().enumerate() {
            if round == 1 && !addons.is_empty() {
                break;
            }
            for (p, _, likes, source) in group {
                if addons.len() as u32 >= rules.build.max_addons {
                    break;
                }
                let price = p.current().unwrap_or(f64::MAX);
                if price > left + 0.004 {
                    continue;
                }
                let kind = p.kind.clone().unwrap_or_default();
                if rules.build.limits.get(&kind).is_some_and(|m| per_kind.get(&kind).copied().unwrap_or(0) >= *m) {
                    continue;
                }
                *per_kind.entry(kind).or_default() += 1;
                left -= price;
                let mut why = Vec::new();
                if !likes.is_empty() {
                    why.push(format!("matches {}", likes.join(", ")));
                }
                if let Some(off) = p.pct_off() {
                    why.push(format!("{off:.0}% off"));
                }
                if let Some(m) = &p.metal_group {
                    why.push(m.clone());
                }
                why.push(format!("fits {label}"));
                if p.system_inferred {
                    why.push("fit inferred, check the product page".into());
                }
                if !fit_sources.contains(&source) {
                    fit_sources.push(source);
                }
                addons.push((p, why));
            }
        }

        let mut notes = Vec::new();
        if base.is_some_and(|b| rules.is_final_sale(b)) {
            notes.push(format!("No returnable {label} bracelet fits the budget; this one is final sale (can't be returned)."));
        }
        if addons.iter().any(|(p, _)| rules.is_final_sale(p)) {
            notes.push(format!("No returnable add-on for the {label} bracelet fits the budget, so these add-ons are final sale (can't be returned)."));
        }
        for s in &fit_sources {
            notes.push(format!("Fit rule for the {label} bracelet: {s}"));
        }
        let base_pick = base.map(|b| {
            let mut why = Vec::new();
            let likes = liked(b, r);
            if !likes.is_empty() {
                why.push(format!("matches {}", likes.join(", ")));
            } else {
                why.push(format!("the cheapest {label} bracelet{}", r.metal.as_deref().map(|m| format!(" in {m}")).unwrap_or_default()));
            }
            if let Some(off) = b.pct_off() {
                why.push(format!("{off:.0}% off"));
            }
            if let Some(adv) = r.wrist_cm.and_then(|w| rules.bracelet_size(&b.store, &system, w)) {
                match adv.bracelet_cm {
                    Some(cm) => notes.push(format!("Size: for a {} cm wrist choose {cm} cm. {} (source: {})", r.wrist_cm.unwrap_or_default(), adv.text, adv.source)),
                    None => notes.push(format!("Size: the wrist is beyond the size chart. {} (source: {})", adv.text, adv.source)),
                }
            }
            pick(b, why, rules)
        });
        let title = match base {
            Some(b) => format!("{} + {} add-on{}", b.name, addons.len(), if addons.len() == 1 { "" } else { "s" }),
            None => format!("Add-ons for the {label} bracelet {} has", r.nick),
        };
        if base.is_none() && addons.is_empty() {
            continue;
        }
        let picks: Vec<Pick> = addons.iter().map(|(p, why)| pick(p, why.clone(), rules)).collect();
        let total: f64 = base_pick.iter().map(|b| b.price).sum::<f64>() + picks.iter().map(|p| p.price).sum::<f64>();
        let list_total: f64 = base_pick.iter().map(|b| b.list_price).sum::<f64>() + picks.iter().map(|p| p.list_price).sum::<f64>();
        let liked_count = addons.iter().filter(|(p, _)| !liked(p, r).is_empty()).count() + base.map_or(0, |b| usize::from(!liked(b, r).is_empty()));
        out.push((
            liked_count,
            Suggestion { title, currency, base: base_pick, addons: picks, total: round2(total), list_total: round2(list_total), savings: round2(list_total - total), left: round2(budget - total), notes },
        ));
    }
    // A bracelet alone is no gift plan when other options come with add-ons.
    if out.iter().any(|(_, s)| !s.addons.is_empty()) {
        out.retain(|(_, s)| !s.addons.is_empty());
    }
    // Returnable gifts first, then more liked picks, then the one that uses the budget best.
    let has_final = |s: &Suggestion| s.base.iter().chain(&s.addons).any(|p| p.final_sale);
    out.sort_by(|a, b| has_final(&a.1).cmp(&has_final(&b.1)).then_with(|| b.0.cmp(&a.0)).then_with(|| a.1.left.total_cmp(&b.1.left)));
    out.into_iter().map(|(_, s)| s).take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::tests::rules;

    fn item(id: &str, name: &str, price: f64) -> Product {
        let mut p = Product {
            store: "pandora-us".into(),
            id: id.into(),
            name: name.into(),
            url: format!("https://us.pandora.net/en/{id}.html"),
            currency: "USD".into(),
            price: Some(price),
            list_price: Some(price),
            ..Default::default()
        };
        rules().classify(&mut p);
        p
    }

    fn catalog() -> Vec<Product> {
        let mut cat_charm = item("790001", "Cat Charm", 50.0);
        cat_charm.list_price = Some(70.0);
        let mut out = item("790007", "Sold Out Cat Charm", 20.0);
        out.in_stock = Some(false);
        vec![
            item("590702", "Pandora Moments Snake Chain Bracelet", 70.0),
            item("597602", "Pandora Reflexions Mesh Bracelet", 65.0),
            cat_charm,
            item("790002", "Star Charm", 40.0),
            item("790003", "Heart Clip Charm", 30.0),
            item("790004", "Flower Clip Charm", 25.0),
            item("790005", "Bow Clip Charm", 20.0),
            item("797000", "Pandora Reflexions Cat Clip Charm", 45.0),
            item("790006", "Very Expensive Cat Charm", 500.0),
            out,
        ]
    }

    fn mom() -> Recipient {
        Recipient { nick: "Mom".into(), likes: vec!["cats".into()], owned: vec!["pandora-us:790002".into()], ..Default::default() }
    }

    #[test]
    fn suggestions_respect_budget_fit_owned_and_limits() {
        let r = rules();
        let s = suggest(&catalog(), &mom(), 150.0, None, &r, 5);
        assert!(!s.is_empty());
        for sug in &s {
            assert!(sug.total <= 150.0, "{sug:?}");
            let base = sug.base.as_ref().unwrap();
            for a in &sug.addons {
                assert_ne!(a.id, "790002", "Mom already owns the star charm");
                assert_ne!(a.id, "790007", "out of stock");
                assert!(matches!(r.fit(a.system.as_deref(), base.system.as_deref()), Fit::Yes { .. }), "{} on {}", a.name, base.name);
            }
            assert!(sug.addons.iter().filter(|a| a.kind.as_deref() == Some("clip")).count() <= 2);
        }
        let moments = s.iter().find(|x| x.base.as_ref().unwrap().id == "590702").unwrap();
        assert_eq!(moments.addons[0].id, "790001", "the liked cat charm comes first");
        assert!(moments.addons[0].why.contains("matches cats") && moments.addons[0].why.contains("29% off"), "{}", moments.addons[0].why);
        assert!(moments.notes.iter().any(|n| n.contains("test-moments")), "the fit rule source is cited once: {:?}", moments.notes);
        let refl = s.iter().find(|x| x.base.as_ref().unwrap().id == "597602").unwrap();
        assert_eq!(refl.addons.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(), vec!["797000"], "only Reflexions clips on a Reflexions bracelet");
    }

    #[test]
    fn a_tight_budget_and_unknown_fit_give_no_add_ons() {
        let r = rules();
        let s = suggest(&catalog(), &mom(), 66.0, None, &r, 5);
        assert!(s.iter().all(|x| x.base.as_ref().unwrap().id == "597602" && x.addons.is_empty()), "{s:?}");
        let no_fit_rules = crate::rules::Rules { fits: vec![], ..rules() };
        let s = suggest(&catalog(), &mom(), 500.0, None, &no_fit_rules, 5);
        assert!(s.iter().all(|x| x.addons.is_empty()), "without fit rules nothing is suggested as fitting");
    }

    #[test]
    fn charms_for_a_bracelet_they_already_have() {
        let mut r = mom();
        r.systems = vec!["pandora-moments".into()];
        let s = suggest(&catalog(), &r, 60.0, None, &rules(), 5);
        let own = s.iter().find(|x| x.base.is_none()).expect("an add-ons-only suggestion");
        assert_eq!(own.addons[0].id, "790001");
        assert!(own.total <= 60.0);
    }

    #[test]
    fn evaluate_warns_about_fit_owned_budget_stock_and_limits() {
        let r = rules();
        let c = catalog();
        let get = |id: &str| c.iter().find(|p| p.id == id).unwrap().clone();
        let mut engr = get("790001");
        engr.engravable = Some(true);
        let items = vec![
            (get("590702"), 1),
            (engr, 1),
            (get("790002"), 1),
            (get("797000"), 1),
            (get("790003"), 1),
            (get("790004"), 1),
            (get("790005"), 1),
            (get("790007"), 1),
        ];
        let mut who = mom();
        who.wrist_cm = Some(16.5);
        let ev = evaluate(&items, Some(&who), Some(150.0), &r);
        let has = |level: &str, needle: &str| ev.warnings.iter().any(|w| w.level == level && w.text.contains(needle));
        assert!(has("stop", "Over budget"), "{:?}", ev.warnings);
        assert!(has("stop", "doesn't fit the Pandora Moments bracelet"));
        assert!(has("warn", "already owns Star Charm"));
        assert!(has("stop", "Sold Out Cat Charm is out of stock"));
        assert!(has("warn", "Engraved items are final sale"));
        assert!(has("warn", "4 × clip"), "{:?}", ev.warnings);
        assert_eq!(ev.total, 70.0 + 50.0 + 40.0 + 45.0 + 30.0 + 25.0 + 20.0 + 20.0);
        assert_eq!(ev.list_total, ev.total + 20.0);
        assert_eq!(ev.savings, 20.0);
        assert_eq!(ev.size.as_ref().and_then(|s| s.bracelet_cm), Some(19.0));
        let ok = evaluate(&[(get("590702"), 1), (get("790001"), 1)], Some(&mom()), Some(150.0), &r);
        assert!(ok.warnings.iter().all(|w| w.level != "stop"), "{:?}", ok.warnings);
        assert_eq!(ok.left, Some(30.0));
    }

    /// The shipped data/rules.toml, on realistic Pandora names.
    fn shipped(id: &str, name: &str, price: f64, badges: &[&str]) -> Product {
        let rules = crate::Config::built_in().unwrap().rules;
        let mut p = Product {
            store: "pandora-us".into(),
            id: id.into(),
            name: name.into(),
            url: format!("https://us.pandora.net/en/{id}.html"),
            currency: "USD".into(),
            price: Some(price),
            list_price: Some(price),
            in_stock: Some(true),
            badges: (!badges.is_empty()).then(|| badges.iter().map(|b| b.to_string()).collect()),
            ..Default::default()
        };
        rules.classify(&mut p);
        p
    }

    #[test]
    fn suggestions_with_the_shipped_rules_keep_tennis_bracelets_and_me_charms_off_moments() {
        let rules = crate::Config::built_in().unwrap().rules;
        let tennis = shipped("590041C01", "Pandora Timeless Pave Single-row Tennis Bracelet", 60.0, &[]);
        assert_eq!((tennis.kind.as_deref(), tennis.system.as_deref()), (Some("bracelet"), None), "a tennis bracelet takes no charms");
        let catalog = vec![
            tennis,
            shipped("599652C01", "Sparkling Slider Tennis Bracelet", 50.0, &[]),
            shipped("590702", "Pandora Moments Snake Chain Bracelet", 75.0, &[]),
            shipped("763369C01", "Pandora ME Cat Mini Dangle Charm", 30.0, &[]),
            shipped("793448C01", "Gradient Planet Cat Mini Dangle Charm", 35.0, &["NEW - COMPATIBLE WITH ME & MOMENTS"]),
            shipped("791001", "Lucky Cat Dangle Charm", 50.0, &[]),
        ];
        let mom = Recipient { nick: "Mom".into(), likes: vec!["cats".into()], ..Default::default() };
        let s = suggest(&catalog, &mom, 200.0, None, &rules, 4);
        assert!(!s.is_empty());
        for sug in &s {
            let base = sug.base.as_ref().expect("a bracelet");
            assert_eq!(base.id, "590702", "only the Moments bracelet is a base: {sug:?}");
            let ids: Vec<&str> = sug.addons.iter().map(|a| a.id.as_str()).collect();
            assert!(!ids.contains(&"763369C01"), "an ME-only charm doesn't go on a Moments bracelet: {ids:?}");
            assert!(ids.contains(&"793448C01"), "the charm labelled ME & Moments does: {ids:?}");
            let lucky = sug.addons.iter().find(|a| a.id == "791001").unwrap();
            assert!(lucky.fit_inferred && lucky.why.contains("fit inferred"), "{lucky:?}");
            assert_eq!(sug.currency, "USD");
        }
        // Evaluating a tennis bracelet with a charm says it isn't a charm bracelet.
        let ev = evaluate(&[(catalog[0].clone(), 1), (catalog[5].clone(), 1)], Some(&mom), None, &rules);
        assert!(ev.warnings.iter().any(|w| w.text.contains("isn't recognised as a charm bracelet")), "{:?}", ev.warnings);
        // A Moments bracelet with a charm whose system is only inferred: fit is flagged.
        let ev = evaluate(&[(catalog[2].clone(), 1), (catalog[5].clone(), 1)], Some(&mom), None, &rules);
        assert!(ev.warnings.iter().any(|w| w.level == "warn" && w.text.contains("fit inferred")), "{:?}", ev.warnings);
        assert!(ev.lines[1].system_inferred);
    }

    #[test]
    fn old_prices_mixed_currencies_and_misread_list_prices_are_flagged() {
        let r = rules();
        let mut old = item("790001", "Cat Charm", 50.0);
        old.price_at = Some("2026-11-01T10:00:00Z".into());
        let mut fresh = item("590702", "Pandora Moments Snake Chain Bracelet", 70.0);
        fresh.price_at = Some("2026-11-14T10:00:00Z".into());
        let now = crate::time::parse_rfc3339("2026-11-15T10:00:00Z").unwrap();
        let ev = evaluate_at(&[(fresh.clone(), 1), (old.clone(), 1)], None, None, &r, now);
        let stale: Vec<&Warning> = ev.warnings.iter().filter(|w| w.text.contains("capture the store again")).collect();
        assert_eq!(stale.len(), 1, "{:?}", ev.warnings);
        assert!(stale[0].text.contains("Cat Charm") && stale[0].text.contains("2026-11-01"));
        assert_eq!(ev.oldest_price_at.as_deref(), Some("2026-11-01T10:00:00Z"));
        assert_eq!(ev.lines[1].price_at.as_deref(), Some("2026-11-01T10:00:00Z"));

        let mut euro = item("790009", "Euro Charm", 20.0);
        euro.currency = "EUR".into();
        let ev = evaluate_at(&[(fresh.clone(), 1), (euro, 1)], None, None, &r, now);
        assert!(ev.warnings.iter().any(|w| w.level == "stop" && w.text.contains("mixes currencies (EUR and USD)")), "{:?}", ev.warnings);

        // "$812 VALUE" read as the price: the list price can't make the saving negative.
        let mut set = item("B802000", "Holiday Gift Set", 812.0);
        set.list_price = Some(399.0);
        let ev = evaluate_at(&[(set, 1)], None, None, &r, now);
        assert_eq!((ev.total, ev.list_total, ev.savings), (812.0, 812.0, 0.0));
    }

    #[test]
    fn lines_say_which_variant_was_priced() {
        let r = rules();
        let v = |id: &str, title: &str, price: f64, available: bool| crate::model::Variant { id: id.into(), title: Some(title.into()), price: Some(price), available: Some(available), ..Default::default() };
        let mut p = item("590702", "Pandora Moments Snake Chain Bracelet", 70.0);
        assert_eq!(evaluate(&[(p.clone(), 1)], None, None, &r).lines[0].priced, "the price on the listing");
        p.variants = vec![v("a", "17 cm", 70.0, true), v("b", "19 cm", 70.0, true)];
        assert_eq!(evaluate(&[(p.clone(), 1)], None, None, &r).lines[0].priced, "any of its 2 variants (same price)");
        p.variants = vec![v("a", "17 cm", 80.0, true), v("b", "19 cm", 70.0, true)];
        let priced = evaluate(&[(p, 1)], None, None, &r).lines[0].priced.clone();
        assert!(priced.starts_with("19 cm") && priced.contains("differ"), "{priced}");
    }

    #[test]
    fn final_sale_and_engraved_to_order_items_are_flagged_and_come_last() {
        let rules = crate::Config::built_in().unwrap().rules;
        // The shipped policies see "FINAL SALE - " names and ENG ids (engravable is false there).
        let fs = shipped("793388C01", "FINAL SALE - Cat Dangle Charm", 20.0, &[]);
        let eng = shipped("ENG791169_424", "Engraved Cat Heart Dangle Charm", 25.0, &[]);
        assert!(rules.policy_warnings(&fs).iter().any(|(t, _)| t.starts_with("Final sale")), "{:?}", rules.policy_warnings(&fs));
        assert!(rules.policy_warnings(&eng).iter().any(|(t, _)| t.starts_with("Engraved to order")), "{:?}", rules.policy_warnings(&eng));
        assert!(rules.is_final_sale(&fs) && rules.is_final_sale(&eng));
        let ok = shipped("791001", "Lucky Cat Dangle Charm", 50.0, &[]);
        assert!(!rules.is_final_sale(&ok) && rules.policy_warnings(&ok).is_empty());

        let bracelet = shipped("590702", "Pandora Moments Snake Chain Bracelet", 75.0, &[]);
        let catalog = vec![bracelet.clone(), fs.clone(), eng.clone(), ok.clone()];
        let mom = Recipient { nick: "Mom".into(), likes: vec!["cats".into()], ..Default::default() };
        // A returnable charm fits: no final-sale item is used, however liked or cheap.
        let s = suggest(&catalog, &mom, 200.0, None, &rules, 4);
        let ids: Vec<&str> = s[0].addons.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, vec!["791001"], "{s:?}");
        assert!(s[0].addons.iter().all(|a| !a.final_sale && a.policies.is_empty()));
        // Only final-sale charms fit what is left: they are used, flagged and explained.
        let s = suggest(&catalog, &mom, 120.0, None, &rules, 4);
        let picked = &s[0].addons;
        assert!(!picked.is_empty() && picked.iter().all(|a| a.final_sale), "{s:?}");
        let eng_pick = picked.iter().find(|a| a.id == "ENG791169_424").unwrap();
        assert!(eng_pick.policies.iter().any(|t| t.contains("Engraved to order")), "{eng_pick:?}");
        assert!(s[0].notes.iter().any(|n| n.contains("No returnable add-on")), "{:?}", s[0].notes);
    }

    #[test]
    fn a_bracelet_without_add_ons_is_offered_only_when_nothing_has_add_ons() {
        let r = rules();
        // The Reflexions bracelet has nothing that fits it here; the Moments one does.
        let catalog = vec![item("590702", "Pandora Moments Snake Chain Bracelet", 70.0), item("597602", "Pandora Reflexions Mesh Bracelet", 65.0), item("790001", "Cat Charm", 50.0)];
        let s = suggest(&catalog, &mom(), 150.0, None, &r, 5);
        assert_eq!(s.len(), 1, "{s:?}");
        assert_eq!(s[0].base.as_ref().unwrap().id, "590702");
        // With nothing that fits either bracelet, the bracelets alone are still offered.
        let s = suggest(&catalog[..2], &mom(), 150.0, None, &r, 5);
        assert_eq!(s.len(), 2);
    }
}
