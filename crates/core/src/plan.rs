//! Building a gift: checking a chosen set of items against the rules and the recipient
//! ([`evaluate`]), and suggesting a bracelet plus add-ons within a budget ([`suggest`]).

use crate::model::{Product, Recipient};
use crate::rules::{Fit, Rules, SizeAdvice};
use crate::text::{has_words, has_words_loose};
use serde::Serialize;

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
    pub metal_group: Option<String>,
    pub url: String,
    pub image: Option<String>,
    pub qty: u32,
    pub price: Option<f64>,
    pub list_price: Option<f64>,
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

fn line(p: &Product, qty: u32, rules: &Rules) -> Line {
    Line {
        store: p.store.clone(),
        id: p.id.clone(),
        name: p.name.clone(),
        kind: p.kind.clone(),
        system: p.system.clone(),
        system_label: p.system.as_deref().map(|s| rules.system_label(s)),
        metal_group: p.metal_group.clone(),
        url: p.url.clone(),
        image: p.image.clone(),
        qty,
        price: p.current(),
        list_price: p.regular(),
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
    let mut w: Vec<Warning> = Vec::new();
    let mut add = |level: &'static str, text: String, p: Option<&Product>| w.push(Warning { level, text, item: p.map(Product::key) });
    let (mut total, mut list_total, mut unknown) = (0.0, 0.0, 0);
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
    pub price: f64,
    pub list_price: f64,
    pub url: String,
    pub image: Option<String>,
    pub why: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Suggestion {
    pub title: String,
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

fn pick(p: &Product, why: Vec<String>) -> Pick {
    Pick {
        store: p.store.clone(),
        id: p.id.clone(),
        name: p.name.clone(),
        kind: p.kind.clone(),
        system: p.system.clone(),
        metal_group: p.metal_group.clone(),
        price: p.current().unwrap_or(0.0),
        list_price: p.regular().unwrap_or(0.0),
        url: p.url.clone(),
        image: p.image.clone(),
        why: why.join("; "),
    }
}

/// Suggests up to `max` gifts within `budget`: a bracelet (or, when the recipient already has a
/// bracelet system, none) plus add-ons the rules say fit it. Only confirmed fits are suggested;
/// owned, out-of-stock and discontinued items are left out.
pub fn suggest(catalog: &[Product], r: &Recipient, budget: f64, store: Option<&str>, rules: &Rules, max: usize) -> Vec<Suggestion> {
    let eligible = |p: &&Product| {
        !p.discontinued && p.in_stock != Some(false) && p.current().is_some() && store.is_none_or(|s| p.store == s) && !r.owns(p)
    };
    let pool: Vec<&Product> = catalog.iter().filter(eligible).collect();
    let cheaper = |a: &&Product, b: &&Product| a.current().unwrap_or(f64::MAX).total_cmp(&b.current().unwrap_or(f64::MAX));

    // Bases: per system, the best liked one, else the cheapest; matching metal first.
    let mut bases: Vec<&Product> = pool.iter().copied().filter(|p| rules.is_base(p.kind.as_deref()) && p.system.is_some() && p.current().is_some_and(|c| c <= budget)).collect();
    if bases.iter().any(|p| metal_ok(p, r)) {
        bases.retain(|p| metal_ok(p, r));
    }
    bases.sort_by(|a, b| liked(b, r).len().cmp(&liked(a, r).len()).then_with(|| cheaper(a, b)));
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
        let mut per_kind: std::collections::BTreeMap<String, u32> = Default::default();
        for (p, _, likes, source) in candidates {
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
            why.push(format!("fits {label} (source: {source})"));
            addons.push((p, why));
        }

        let mut notes = Vec::new();
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
            pick(b, why)
        });
        let title = match base {
            Some(b) => format!("{} + {} add-on{}", b.name, addons.len(), if addons.len() == 1 { "" } else { "s" }),
            None => format!("Add-ons for the {label} bracelet {} has", r.nick),
        };
        if base.is_none() && addons.is_empty() {
            continue;
        }
        let picks: Vec<Pick> = addons.iter().map(|(p, why)| pick(p, why.clone())).collect();
        let total: f64 = base_pick.iter().map(|b| b.price).sum::<f64>() + picks.iter().map(|p| p.price).sum::<f64>();
        let list_total: f64 = base_pick.iter().map(|b| b.list_price).sum::<f64>() + picks.iter().map(|p| p.list_price).sum::<f64>();
        let liked_count = addons.iter().filter(|(p, _)| !liked(p, r).is_empty()).count() + base.map_or(0, |b| usize::from(!liked(b, r).is_empty()));
        out.push((
            liked_count,
            Suggestion { title, base: base_pick, addons: picks, total: round2(total), list_total: round2(list_total), savings: round2(list_total - total), left: round2(budget - total), notes },
        ));
    }
    // More liked picks first, then the one that uses the budget best.
    out.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.left.total_cmp(&b.1.left)));
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
}
