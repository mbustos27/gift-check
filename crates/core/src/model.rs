//! Products, recipients and gift plans as gift-check holds them.

use serde::{Deserialize, Serialize};

/// The kinds a product can be. Captures may state one; otherwise the rules infer it.
pub const KINDS: &[&str] = &["charm", "bracelet", "clip", "spacer", "safety-chain", "necklace", "ring", "earring", "gift-set", "other"];

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Variant {
    pub id: String,
    pub title: Option<String>,
    pub sku: Option<String>,
    pub price: Option<f64>,
    pub list_price: Option<f64>,
    pub available: Option<bool>,
    #[serde(default)]
    pub options: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Product {
    pub store: String,
    pub id: String,
    pub name: String,
    pub url: String,
    pub category: Option<String>,
    /// The listing page the product was read from (a browser capture); scopes `complete_listings`.
    #[serde(default)]
    pub listing: Option<String>,
    pub kind: Option<String>,
    /// The kind came from the store's data, not from a rule.
    #[serde(default)]
    pub kind_given: bool,
    /// The compatibility system (from the rules), such as "pandora-moments".
    pub system: Option<String>,
    /// The system came from a fallback rule (nothing on the listing named it), so fit is a guess.
    #[serde(default)]
    pub system_inferred: bool,
    pub metal: Option<String>,
    pub metal_group: Option<String>,
    #[serde(default)]
    pub metal_group_given: bool,
    pub collection: Option<String>,
    /// `None` when the source didn't say; `Some(vec![])` when it said "none".
    pub themes: Option<Vec<String>>,
    pub badges: Option<Vec<String>>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub vendor: Option<String>,
    pub description: Option<String>,
    pub engravable: Option<bool>,
    pub in_stock: Option<bool>,
    pub currency: String,
    /// The current price shown (promotions included); `None` when unknown.
    pub price: Option<f64>,
    pub list_price: Option<f64>,
    /// When the price was observed.
    pub price_at: Option<String>,
    pub image: Option<String>,
    #[serde(default)]
    pub variants: Vec<Variant>,
    #[serde(default)]
    pub discontinued: bool,
    pub first_seen: Option<String>,
    pub last_seen: Option<String>,
}

impl Product {
    pub fn key(&self) -> String {
        format!("{}:{}", self.store, self.id)
    }

    /// What it costs now: the shown price, or the list price when no other price is known.
    pub fn current(&self) -> Option<f64> {
        self.price.or(self.list_price)
    }

    /// The regular price: the higher of the list price and the current price (a list price below
    /// the price shown is a misread, never a negative saving), or whichever is known.
    pub fn regular(&self) -> Option<f64> {
        match (self.list_price, self.current()) {
            (Some(l), Some(c)) => Some(l.max(c)),
            (l, c) => l.or(c),
        }
    }

    /// Percent below the list price, when there is a discount.
    pub fn pct_off(&self) -> Option<f64> {
        match (self.current(), self.list_price) {
            (Some(p), Some(l)) if l > 0.0 && l - p >= 0.005 => Some(((l - p) / l * 100.0 * 10.0).round() / 10.0),
            _ => None,
        }
    }

    pub fn on_sale(&self) -> bool {
        self.pct_off().is_some()
    }

    /// Lower-case text for matching likes and themes against.
    pub fn haystack(&self) -> String {
        let mut parts = vec![self.name.clone()];
        parts.extend(self.collection.clone());
        parts.extend(self.category.clone());
        parts.extend(self.themes.iter().flatten().cloned());
        parts.extend(self.tags.iter().cloned());
        parts.join(" | ")
    }
}

/// Someone a gift is for. A nickname is all gift-check needs; no real names.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recipient {
    pub nick: String,
    #[serde(default)]
    pub wrist_cm: Option<f64>,
    /// Metal group they prefer, such as "Silver" or "Rose gold".
    #[serde(default)]
    pub metal: Option<String>,
    /// Systems or brands they already own, such as "pandora-moments".
    #[serde(default)]
    pub systems: Vec<String>,
    /// Products they already own, as "store:id".
    #[serde(default)]
    pub owned: Vec<String>,
    /// Likes and themes, such as "cats", "travel".
    #[serde(default)]
    pub likes: Vec<String>,
    #[serde(default)]
    pub budget: Option<f64>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

impl Recipient {
    pub fn validate(&mut self) -> Result<(), String> {
        self.nick = crate::text::clean(&self.nick);
        if self.nick.is_empty() || self.nick.chars().count() > 40 {
            return Err("the nickname must be 1-40 characters".into());
        }
        if !self.nick.chars().all(|c| c.is_alphanumeric() || " _.-'".contains(c)) {
            return Err("the nickname may use letters, digits, spaces and _ . - ' only".into());
        }
        if let Some(w) = self.wrist_cm {
            if !w.is_finite() || !(8.0..=30.0).contains(&w) {
                return Err("wrist size must be 8 to 30 cm".into());
            }
        }
        if let Some(b) = self.budget {
            if !b.is_finite() || !(0.0..=100_000.0).contains(&b) {
                return Err("budget must be 0 to 100000".into());
            }
        }
        let tidy = |list: &mut Vec<String>, what: &str| -> Result<(), String> {
            let mut out: Vec<String> = Vec::new();
            for v in list.iter().map(|v| crate::text::clean(v)).filter(|v| !v.is_empty()) {
                if v.chars().count() > 80 {
                    return Err(format!("each {what} entry must be at most 80 characters"));
                }
                if !out.iter().any(|o| o.eq_ignore_ascii_case(&v)) {
                    out.push(v);
                }
            }
            if out.len() > 200 {
                return Err(format!("at most 200 {what} entries"));
            }
            *list = out;
            Ok(())
        };
        tidy(&mut self.systems, "systems")?;
        tidy(&mut self.owned, "owned")?;
        tidy(&mut self.likes, "likes")?;
        if let Some(bad) = self.owned.iter().find(|o| !o.split_once(':').is_some_and(|(s, i)| crate::stores::valid_id(s) && crate::capture::valid_product_id(i))) {
            return Err(format!("owned items are written store:id (like pandora-us:794948C01); {bad:?} isn't"));
        }
        self.metal = self.metal.as_deref().map(crate::text::clean).filter(|m| !m.is_empty());
        if self.metal.as_ref().is_some_and(|m| m.chars().count() > 40) {
            return Err("metal preference must be at most 40 characters".into());
        }
        self.notes = self.notes.as_deref().map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
        if self.notes.as_ref().is_some_and(|n| n.chars().count() > 2000) {
            return Err("notes must be at most 2000 characters".into());
        }
        Ok(())
    }

    pub fn owns(&self, p: &Product) -> bool {
        let key = p.key();
        self.owned.iter().any(|o| o.eq_ignore_ascii_case(&key))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanItem {
    pub store: String,
    pub id: String,
    #[serde(default = "one")]
    pub qty: u32,
    /// The price when the plan was saved, and when that price was observed (filled on save).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved_price: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved_price_at: Option<String>,
}

fn one() -> u32 {
    1
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GiftPlan {
    #[serde(default)]
    pub id: Option<i64>,
    pub name: String,
    #[serde(default)]
    pub recipient: Option<String>,
    #[serde(default)]
    pub budget: Option<f64>,
    pub items: Vec<PlanItem>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

impl GiftPlan {
    pub fn validate(&mut self) -> Result<(), String> {
        self.name = crate::text::clean(&self.name);
        if self.name.is_empty() || self.name.chars().count() > 80 {
            return Err("give the plan a name of 1-80 characters".into());
        }
        if self.items.is_empty() || self.items.len() > 60 {
            return Err("a plan holds 1 to 60 items".into());
        }
        if let Some(b) = self.budget {
            if !b.is_finite() || !(0.0..=100_000.0).contains(&b) {
                return Err("budget must be 0 to 100000".into());
            }
        }
        for it in &self.items {
            if !crate::stores::valid_id(&it.store) || !crate::capture::valid_product_id(&it.id) || !(1..=20).contains(&it.qty) {
                return Err(format!("bad plan item {}:{}", it.store, it.id));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prices_and_discounts() {
        let p = Product { price: Some(68.0), list_price: Some(98.0), ..Default::default() };
        assert_eq!(p.pct_off(), Some(30.6));
        let q = Product { price: None, list_price: Some(98.0), ..Default::default() };
        assert_eq!(q.current(), Some(98.0));
        assert!(!q.on_sale());
        let r = Product { price: Some(98.0), list_price: Some(60.0), ..Default::default() };
        assert!(!r.on_sale(), "a list price below the price isn't a sale");
    }

    #[test]
    fn recipients_are_checked() {
        let mut r = Recipient { nick: " Mom ".into(), owned: vec!["pandora-us:794948C01".into(), "".into()], likes: vec!["cats".into(), "Cats".into()], ..Default::default() };
        r.validate().unwrap();
        assert_eq!(r.nick, "Mom");
        assert_eq!(r.likes, vec!["cats"]);
        assert_eq!(r.owned, vec!["pandora-us:794948C01"]);
        for bad in [
            Recipient { nick: "".into(), ..Default::default() },
            Recipient { nick: "<b>x</b>".into(), ..Default::default() },
            Recipient { nick: "A".into(), wrist_cm: Some(f64::NAN), ..Default::default() },
            Recipient { nick: "A".into(), owned: vec!["794948C01".into()], ..Default::default() },
        ] {
            let mut b = bad;
            assert!(b.validate().is_err(), "{b:?}");
        }
    }
}
