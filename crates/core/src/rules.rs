//! Rules as data (`data/rules.toml`): how to tell a product's kind, metal group and compatibility
//! system, which add-on systems fit which bracelet systems, sizing, and store policies. Nothing
//! about a store is hard-coded here; every rule carries a `source` and an `as_of` date.

use crate::model::{Product, KINDS};
use crate::text::has_words;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Meta {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub about: String,
}

fn any_store() -> String {
    "*".into()
}

/// Puts a product in a kind. A rule matches when ANY of its lists matches; the first matching
/// rule in file order wins (store-specific rules are tried before `store = "*"` ones).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KindRule {
    #[serde(default = "any_store")]
    pub store: String,
    pub kind: String,
    /// Whole words in the product name ("Safety Chain" matches "Safety Chain Clip Charm").
    #[serde(default)]
    pub name_words: Vec<String>,
    /// Whole words in the store's product type (Shopify).
    #[serde(default)]
    pub type_words: Vec<String>,
    #[serde(default)]
    pub tag_words: Vec<String>,
    /// The capture's category, exactly (case-insensitive).
    #[serde(default)]
    pub category: Vec<String>,
    /// Start of the product id; `?` matches any one character.
    #[serde(default)]
    pub id_prefix: Vec<String>,
    #[serde(default)]
    pub note: String,
    pub source: String,
    pub as_of: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetalRule {
    #[serde(default = "any_store")]
    pub store: String,
    pub group: String,
    /// Whole words in the metal text (or the name when the metal isn't given).
    #[serde(default)]
    pub words: Vec<String>,
    #[serde(default)]
    pub id_prefix: Vec<String>,
    #[serde(default)]
    pub note: String,
    pub source: String,
    pub as_of: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SystemRule {
    pub id: String,
    pub label: String,
    #[serde(default = "any_store")]
    pub store: String,
    /// Kinds this rule applies to; empty means all.
    #[serde(default)]
    pub kinds: Vec<String>,
    #[serde(default)]
    pub name_words: Vec<String>,
    #[serde(default)]
    pub collection_words: Vec<String>,
    #[serde(default)]
    pub badge_words: Vec<String>,
    #[serde(default)]
    pub id_prefix: Vec<String>,
    /// When set, a match also needs one of these words in the name, collection or category
    /// ("Bangle" only names a Moments bangle when the name also says "Charm" or "Moments").
    #[serde(default)]
    pub context_words: Vec<String>,
    /// Applies to the store's products of these kinds when no other system rule matched. Never to
    /// base kinds (bracelets): a bracelet is a charm-bracelet base only when something names its
    /// system. A system from a fallback is marked inferred.
    #[serde(default)]
    pub fallback: bool,
    #[serde(default)]
    pub note: String,
    pub source: String,
    pub as_of: String,
}

/// Add-ons (charms, clips, ...) of `addon_system` fit bracelets of `base_systems`, and no others.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FitRule {
    pub addon_system: String,
    pub base_systems: Vec<String>,
    #[serde(default)]
    pub note: String,
    pub source: String,
    pub as_of: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SizingRule {
    #[serde(default = "any_store")]
    pub store: String,
    pub system: String,
    /// Rows of [wrist cm, bracelet cm]; between rows, size up.
    #[serde(default)]
    pub chart: Vec<[f64; 2]>,
    /// Used when there is no chart: bracelet = wrist + this.
    #[serde(default)]
    pub add_cm: Option<f64>,
    #[serde(default)]
    pub text: String,
    pub source: String,
    pub as_of: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyRule {
    #[serde(default = "any_store")]
    pub store: String,
    /// "engravable" or "badge".
    pub when: String,
    #[serde(default)]
    pub badge: Option<String>,
    pub text: String,
    pub source: String,
    pub as_of: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoteRule {
    #[serde(default = "any_store")]
    pub store: String,
    pub topic: String,
    pub text: String,
    pub source: String,
    pub as_of: String,
}

fn base_kinds() -> Vec<String> {
    vec!["bracelet".into()]
}
fn addon_kinds() -> Vec<String> {
    ["charm", "clip", "spacer", "safety-chain"].iter().map(|s| s.to_string()).collect()
}
fn max_addons() -> u32 {
    8
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Build {
    #[serde(default = "base_kinds")]
    pub base_kinds: Vec<String>,
    #[serde(default = "addon_kinds")]
    pub addon_kinds: Vec<String>,
    /// Most of one add-on kind on one bracelet, such as clip = 2.
    #[serde(default)]
    pub limits: BTreeMap<String, u32>,
    #[serde(default)]
    pub limits_source: String,
    /// Most add-ons the planner suggests at once.
    #[serde(default = "max_addons")]
    pub max_addons: u32,
}

impl Default for Build {
    fn default() -> Self {
        Build { base_kinds: base_kinds(), addon_kinds: addon_kinds(), limits: BTreeMap::new(), limits_source: String::new(), max_addons: max_addons() }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rules {
    #[serde(default)]
    pub meta: Meta,
    #[serde(default, rename = "kind")]
    pub kinds: Vec<KindRule>,
    #[serde(default, rename = "metal_group")]
    pub metal_groups: Vec<MetalRule>,
    #[serde(default, rename = "system")]
    pub systems: Vec<SystemRule>,
    #[serde(default, rename = "fit")]
    pub fits: Vec<FitRule>,
    #[serde(default, rename = "sizing")]
    pub sizing: Vec<SizingRule>,
    #[serde(default, rename = "policy")]
    pub policies: Vec<PolicyRule>,
    #[serde(default, rename = "note")]
    pub notes: Vec<NoteRule>,
    #[serde(default)]
    pub build: Build,
}

/// Does an add-on fit a bracelet?
#[derive(Debug, Clone, PartialEq)]
pub enum Fit {
    Yes { source: String },
    No { source: String, note: String },
    /// A system is unknown, or no rule speaks about this add-on system.
    Unknown,
}

fn store_matches(rule_store: &str, store: &str) -> bool {
    rule_store == "*" || rule_store == store
}

/// `?` in a prefix matches any one character.
pub fn id_has_prefix(id: &str, prefix: &str) -> bool {
    let id: Vec<char> = id.chars().collect();
    let p: Vec<char> = prefix.chars().collect();
    p.len() <= id.len() && p.iter().zip(&id).all(|(a, b)| *a == '?' || a.eq_ignore_ascii_case(b))
}

fn any_words(hay: &str, words: &[String]) -> bool {
    words.iter().any(|w| has_words(hay, w))
}

impl Rules {
    pub fn parse(text: &str) -> Result<Rules, String> {
        let r: Rules = toml::from_str(text).map_err(|e| format!("rules.toml: {e}"))?;
        r.validate()?;
        Ok(r)
    }

    fn validate(&self) -> Result<(), String> {
        let sourced = |what: &str, source: &str, as_of: &str| -> Result<(), String> {
            if source.trim().is_empty() || as_of.trim().is_empty() {
                return Err(format!("rules.toml: {what} needs a source and an as_of (write \"TODO\" if not known yet)"));
            }
            Ok(())
        };
        let kind_ok = |k: &str, what: &str| -> Result<(), String> {
            if KINDS.contains(&k) {
                Ok(())
            } else {
                Err(format!("rules.toml: {what}: unknown kind {k:?} (kinds: {})", KINDS.join(", ")))
            }
        };
        for r in &self.kinds {
            sourced(&format!("kind rule {:?}", r.kind), &r.source, &r.as_of)?;
            kind_ok(&r.kind, "kind rule")?;
        }
        for r in &self.metal_groups {
            sourced(&format!("metal_group {:?}", r.group), &r.source, &r.as_of)?;
        }
        let mut ids = std::collections::HashSet::new();
        for r in &self.systems {
            sourced(&format!("system {:?}", r.id), &r.source, &r.as_of)?;
            for k in &r.kinds {
                kind_ok(k, &format!("system {:?}", r.id))?;
            }
            ids.insert(r.id.as_str());
        }
        for r in &self.fits {
            sourced(&format!("fit for {:?}", r.addon_system), &r.source, &r.as_of)?;
            for s in std::iter::once(&r.addon_system).chain(&r.base_systems) {
                if !ids.contains(s.as_str()) {
                    return Err(format!("rules.toml: fit names system {s:?}, which no [[system]] defines"));
                }
            }
        }
        for r in &self.sizing {
            sourced(&format!("sizing for {:?}", r.system), &r.source, &r.as_of)?;
            let bad = r.chart.iter().flatten().any(|v| !v.is_finite() || *v <= 0.0) || r.add_cm.is_some_and(|a| !a.is_finite() || a < 0.0);
            if bad || (r.chart.is_empty() && r.add_cm.is_none()) {
                return Err(format!("rules.toml: sizing for {:?} needs a chart of positive [wrist, bracelet] rows or add_cm", r.system));
            }
        }
        for r in &self.policies {
            sourced(&format!("policy {:?}", r.when), &r.source, &r.as_of)?;
            match (r.when.as_str(), &r.badge) {
                ("engravable", _) | ("badge", Some(_)) => {}
                _ => return Err("rules.toml: policy `when` must be \"engravable\" or \"badge\" (with badge = \"...\")".into()),
            }
        }
        for r in &self.notes {
            sourced(&format!("note {:?}", r.topic), &r.source, &r.as_of)?;
        }
        for k in self.build.base_kinds.iter().chain(&self.build.addon_kinds).chain(self.build.limits.keys()) {
            kind_ok(k, "build")?;
        }
        Ok(())
    }

    /// Rules whose source still says TODO.
    pub fn todo_count(&self) -> usize {
        let todo = |s: &str| s.trim().to_ascii_uppercase().starts_with("TODO");
        self.kinds.iter().filter(|r| todo(&r.source)).count()
            + self.metal_groups.iter().filter(|r| todo(&r.source)).count()
            + self.systems.iter().filter(|r| todo(&r.source)).count()
            + self.fits.iter().filter(|r| todo(&r.source)).count()
            + self.sizing.iter().filter(|r| todo(&r.source)).count()
            + self.policies.iter().filter(|r| todo(&r.source)).count()
            + self.notes.iter().filter(|r| todo(&r.source)).count()
    }

    pub fn rule_count(&self) -> usize {
        self.kinds.len() + self.metal_groups.len() + self.systems.len() + self.fits.len() + self.sizing.len() + self.policies.len() + self.notes.len()
    }

    /// Store-specific rules first, then the `*` ones, each in file order.
    fn ordered<'a, T>(list: &'a [T], store: &str, rule_store: impl Fn(&T) -> &str) -> Vec<&'a T> {
        let mut out: Vec<&T> = list.iter().filter(|r| rule_store(r) == store).collect();
        out.extend(list.iter().filter(|r| rule_store(r) == "*"));
        out
    }

    pub fn classify_kind(&self, p: &Product) -> Option<String> {
        for r in Self::ordered(&self.kinds, &p.store, |r| &r.store) {
            let hit = any_words(&p.name, &r.name_words)
                || p.category.as_deref().is_some_and(|c| any_words(c, &r.type_words) || r.category.iter().any(|x| x.eq_ignore_ascii_case(c)))
                || p.tags.iter().any(|t| any_words(t, &r.tag_words))
                || r.id_prefix.iter().any(|x| id_has_prefix(&p.id, x));
            if hit {
                return Some(r.kind.clone());
            }
        }
        None
    }

    pub fn metal_group_of(&self, p: &Product) -> Option<String> {
        let text = p.metal.as_deref().unwrap_or(&p.name);
        for r in Self::ordered(&self.metal_groups, &p.store, |r| &r.store) {
            if any_words(text, &r.words) || r.id_prefix.iter().any(|x| id_has_prefix(&p.id, x)) {
                return Some(r.group.clone());
            }
        }
        None
    }

    /// The product's compatibility system, and whether it was only inferred (from a fallback).
    pub fn system_of(&self, p: &Product) -> Option<(String, bool)> {
        let kind = p.kind.as_deref()?;
        let applies = |r: &SystemRule| store_matches(&r.store, &p.store) && (r.kinds.is_empty() || r.kinds.iter().any(|k| k == kind));
        let badges = p.badges.as_deref().unwrap_or_default();
        let context = [Some(p.name.as_str()), p.collection.as_deref(), p.category.as_deref()];
        for r in self.systems.iter().filter(|r| !r.fallback && applies(r)) {
            let hit = any_words(&p.name, &r.name_words)
                || p.collection.as_deref().is_some_and(|c| any_words(c, &r.collection_words))
                || badges.iter().any(|b| any_words(b, &r.badge_words))
                || r.id_prefix.iter().any(|x| id_has_prefix(&p.id, x));
            let in_context = r.context_words.is_empty() || context.iter().flatten().any(|t| any_words(t, &r.context_words));
            if hit && in_context {
                return Some((r.id.clone(), false));
            }
        }
        if self.is_base(Some(kind)) {
            return None;
        }
        self.systems.iter().find(|r| r.fallback && applies(r)).map(|r| (r.id.clone(), true))
    }

    /// Kind, metal group and system from the rules, leaving what the store said alone.
    pub fn classify(&self, p: &mut Product) {
        if !p.kind_given {
            p.kind = self.classify_kind(p);
        }
        if !p.metal_group_given {
            p.metal_group = self.metal_group_of(p);
        }
        let system = self.system_of(p);
        p.system_inferred = system.as_ref().is_some_and(|(_, inferred)| *inferred);
        p.system = system.map(|(s, _)| s);
    }

    pub fn fit(&self, addon_system: Option<&str>, base_system: Option<&str>) -> Fit {
        let (Some(a), Some(b)) = (addon_system, base_system) else { return Fit::Unknown };
        let rules: Vec<&FitRule> = self.fits.iter().filter(|f| f.addon_system == a).collect();
        if rules.is_empty() {
            return Fit::Unknown;
        }
        match rules.iter().find(|f| f.base_systems.iter().any(|s| s == b)) {
            Some(f) => Fit::Yes { source: f.source.clone() },
            None => Fit::No { source: rules[0].source.clone(), note: rules[0].note.clone() },
        }
    }

    pub fn system_label(&self, id: &str) -> String {
        self.systems.iter().find(|s| s.id == id).map_or_else(|| id.to_string(), |s| s.label.clone())
    }

    pub fn is_base(&self, kind: Option<&str>) -> bool {
        kind.is_some_and(|k| self.build.base_kinds.iter().any(|b| b == k))
    }

    pub fn is_addon(&self, kind: Option<&str>) -> bool {
        kind.is_some_and(|k| self.build.addon_kinds.iter().any(|b| b == k))
    }

    /// The bracelet length for a wrist, from the first matching sizing rule.
    pub fn bracelet_size(&self, store: &str, system: &str, wrist_cm: f64) -> Option<SizeAdvice> {
        let r = self.sizing.iter().find(|r| store_matches(&r.store, store) && r.system == system)?;
        let size = if r.chart.is_empty() {
            r.add_cm.map(|a| wrist_cm + a)
        } else {
            let mut rows = r.chart.clone();
            rows.sort_by(|a, b| a[0].total_cmp(&b[0]));
            // Between sizes, size up.
            rows.iter().find(|row| row[0] + 1e-9 >= wrist_cm).map(|row| row[1])
        };
        Some(SizeAdvice { bracelet_cm: size, text: r.text.clone(), source: r.source.clone() })
    }

    /// Policy warnings for a product (engraving, final-sale badges).
    pub fn policy_warnings(&self, p: &Product) -> Vec<(String, String)> {
        let badges = p.badges.as_deref().unwrap_or_default();
        self.policies
            .iter()
            .filter(|r| store_matches(&r.store, &p.store))
            .filter(|r| match r.when.as_str() {
                "engravable" => p.engravable == Some(true),
                "badge" => r.badge.as_deref().is_some_and(|want| badges.iter().any(|b| has_words(b, want))),
                _ => false,
            })
            .map(|r| (r.text.clone(), r.source.clone()))
            .collect()
    }

    pub fn notes_for(&self, store: Option<&str>) -> Vec<&NoteRule> {
        self.notes.iter().filter(|n| store.is_none_or(|s| store_matches(&n.store, s))).collect()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SizeAdvice {
    /// `None` when the wrist is beyond the chart.
    pub bracelet_cm: Option<f64>,
    pub text: String,
    pub source: String,
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub const TEST_RULES: &str = r#"
[[kind]]
store = "pandora-us"
kind = "safety-chain"
name_words = ["Safety Chain"]
source = "test"
as_of = "2026-11-15"

[[kind]]
store = "pandora-us"
kind = "clip"
name_words = ["Clip"]
source = "test"
as_of = "2026-11-15"

[[kind]]
store = "pandora-us"
kind = "bracelet"
name_words = ["Bracelet", "Bangle"]
source = "test"
as_of = "2026-11-15"

[[kind]]
store = "pandora-us"
kind = "charm"
id_prefix = ["7"]
source = "test"
as_of = "2026-11-15"

[[kind]]
kind = "charm"
name_words = ["Charm"]
source = "test"
as_of = "2026-11-15"

[[metal_group]]
store = "pandora-us"
group = "Silver"
id_prefix = ["?9"]
source = "test"
as_of = "2026-11-15"

[[metal_group]]
group = "Rose gold"
words = ["rose gold"]
source = "test"
as_of = "2026-11-15"

[[system]]
id = "pandora-reflexions"
label = "Pandora Reflexions"
store = "pandora-us"
name_words = ["Reflexions"]
source = "test"
as_of = "2026-11-15"

[[system]]
id = "pandora-me"
label = "Pandora ME"
store = "pandora-us"
name_words = ["Pandora ME"]
source = "test"
as_of = "2026-11-15"

[[system]]
id = "pandora-moments"
label = "Pandora Moments"
store = "pandora-us"
name_words = ["Moments", "Snake Chain"]
source = "test"
as_of = "2026-11-15"

[[system]]
id = "pandora-moments"
label = "Pandora Moments"
store = "pandora-us"
kinds = ["bracelet"]
name_words = ["Bangle", "T-Bar", "Slider"]
context_words = ["Charm", "Moments"]
source = "test"
as_of = "2026-11-15"

[[system]]
id = "pandora-moments"
label = "Pandora Moments"
store = "pandora-us"
kinds = ["charm", "bracelet", "clip", "spacer", "safety-chain"]
fallback = true
source = "test"
as_of = "2026-11-15"

[[fit]]
addon_system = "pandora-moments"
base_systems = ["pandora-moments"]
source = "test-moments"
as_of = "2026-11-15"

[[fit]]
addon_system = "pandora-reflexions"
base_systems = ["pandora-reflexions"]
note = "flat mesh"
source = "test-reflexions"
as_of = "2026-11-15"

[[sizing]]
store = "pandora-us"
system = "pandora-moments"
chart = [[13, 15], [14, 16], [15, 17], [16, 18], [17, 19], [18, 20], [19, 21], [20, 23]]
text = "size up between sizes"
source = "test"
as_of = "2026-11-15"

[[policy]]
store = "pandora-us"
when = "engravable"
text = "Engraved items are final sale."
source = "test"
as_of = "2026-11-15"

[[policy]]
store = "pandora-us"
when = "badge"
badge = "Last Chance"
text = "Last Chance items are final sale."
source = "TODO"
as_of = "TODO"

[build]
limits = { clip = 2, safety-chain = 1 }
"#;

    pub fn rules() -> Rules {
        Rules::parse(TEST_RULES).unwrap()
    }

    fn pandora(id: &str, name: &str) -> Product {
        Product { store: "pandora-us".into(), id: id.into(), name: name.into(), ..Default::default() }
    }

    #[test]
    fn kinds_follow_rule_order() {
        let r = rules();
        let kind = |id: &str, name: &str| r.classify_kind(&pandora(id, name));
        assert_eq!(kind("791088", "Band of Hearts Safety Chain Clip Charm").as_deref(), Some("safety-chain"));
        assert_eq!(kind("797602", "Sparkling Heart Clip Charm").as_deref(), Some("clip"));
        assert_eq!(kind("590702", "Pandora Moments Snake Chain Bracelet").as_deref(), Some("bracelet"));
        assert_eq!(kind("794948C01", "Lucky Cat").as_deref(), Some("charm"), "falls back to the id prefix");
        let other = Product { store: "hardjewelry".into(), id: "1".into(), name: "Tiny Star Charm".into(), ..Default::default() };
        assert_eq!(r.classify_kind(&other).as_deref(), Some("charm"), "the * rules apply to every store");
        let none = Product { store: "hardjewelry".into(), id: "7".into(), name: "Gift card".into(), ..Default::default() };
        assert_eq!(r.classify_kind(&none), None, "store-specific id rules stay with their store");
    }

    #[test]
    fn systems_metal_and_fit() {
        let r = rules();
        let mut refl = pandora("597602", "Pandora Reflexions Mesh Bracelet");
        r.classify(&mut refl);
        assert_eq!(refl.kind.as_deref(), Some("bracelet"));
        assert_eq!(refl.system.as_deref(), Some("pandora-reflexions"));
        assert_eq!(refl.metal_group.as_deref(), Some("Silver"));
        let mut charm = pandora("788000C01", "Rose Heart Charm");
        charm.metal = Some("14k rose gold-plated".into());
        r.classify(&mut charm);
        assert_eq!(charm.system.as_deref(), Some("pandora-moments"), "fallback system");
        assert_eq!(charm.metal_group.as_deref(), Some("Rose gold"));
        assert!(matches!(r.fit(Some("pandora-moments"), Some("pandora-moments")), Fit::Yes { .. }));
        assert!(matches!(r.fit(Some("pandora-reflexions"), Some("pandora-moments")), Fit::No { .. }));
        assert_eq!(r.fit(Some("pandora-me"), Some("pandora-moments")), Fit::Unknown, "no rule about ME add-ons");
        assert_eq!(r.fit(None, Some("pandora-moments")), Fit::Unknown);
        // A kind the store gave is kept.
        let mut given = pandora("590000", "Mystery Bracelet");
        given.kind = Some("necklace".into());
        given.kind_given = true;
        r.classify(&mut given);
        assert_eq!(given.kind.as_deref(), Some("necklace"));
    }

    #[test]
    fn bracelets_need_a_named_system_and_fallbacks_are_marked_inferred() {
        let r = rules();
        let sys = |name: &str| {
            let mut p = pandora("590000", name);
            r.classify(&mut p);
            (p.kind.unwrap_or_default(), p.system, p.system_inferred)
        };
        // The fallback system never makes a bracelet a charm-bracelet base.
        assert_eq!(sys("Timeless Pave Single-row Tennis Bracelet"), ("bracelet".into(), None, false));
        assert_eq!(sys("Sparkling Slider Tennis Bracelet"), ("bracelet".into(), None, false), "Slider needs a charm context");
        assert_eq!(sys("Signature Bangle"), ("bracelet".into(), None, false));
        assert_eq!(sys("Pandora Moments Snake Chain Bracelet"), ("bracelet".into(), Some("pandora-moments".into()), false));
        assert_eq!(sys("Heart Clasp Charm Bangle"), ("bracelet".into(), Some("pandora-moments".into()), false));
        // Add-ons still get the fallback, marked as a guess.
        let mut charm = pandora("790001", "Lucky Cat Dangle Charm");
        r.classify(&mut charm);
        assert_eq!((charm.system.as_deref(), charm.system_inferred), (Some("pandora-moments"), true));
        let mut named = pandora("790002", "Pandora Moments Disney Charm");
        r.classify(&mut named);
        assert_eq!((named.system.as_deref(), named.system_inferred), (Some("pandora-moments"), false));
    }

    #[test]
    fn sizing_sizes_up_between_rows() {
        let r = rules();
        let s = |w| r.bracelet_size("pandora-us", "pandora-moments", w).unwrap().bracelet_cm;
        assert_eq!(s(16.0), Some(18.0));
        assert_eq!(s(16.5), Some(19.0));
        assert_eq!(s(20.0), Some(23.0));
        assert_eq!(s(21.0), None);
        assert!(r.bracelet_size("pandora-us", "pandora-me", 16.0).is_none());
    }

    #[test]
    fn policies_and_todo_counting() {
        let r = rules();
        let mut p = pandora("1", "Engravable Bar");
        p.engravable = Some(true);
        p.badges = Some(vec!["LAST CHANCE".into()]);
        let w = r.policy_warnings(&p);
        assert_eq!(w.len(), 2);
        assert_eq!(r.todo_count(), 1);
    }

    #[test]
    fn rules_need_sources_and_known_systems() {
        let unsourced = "[[kind]]\nkind = \"charm\"\nname_words = [\"Charm\"]\nsource = \"\"\nas_of = \"2026-11-15\"\n";
        assert!(Rules::parse(unsourced).unwrap_err().contains("source"));
        let bad_kind = "[[kind]]\nkind = \"brooch\"\nsource = \"x\"\nas_of = \"y\"\n";
        assert!(Rules::parse(bad_kind).is_err());
        let bad_fit = "[[fit]]\naddon_system = \"a\"\nbase_systems = [\"b\"]\nsource = \"x\"\nas_of = \"y\"\n";
        assert!(Rules::parse(bad_fit).unwrap_err().contains("no [[system]]"));
        assert!(Rules::parse("[[kinds]]\n").is_err(), "typos in table names are refused");
    }

    #[test]
    fn id_prefix_wildcards() {
        assert!(id_has_prefix("594595C00", "?9"));
        assert!(!id_has_prefix("563050C00", "?9"));
        assert!(id_has_prefix("794948C01", "7"));
        assert!(!id_has_prefix("7", "79"));
    }
}
