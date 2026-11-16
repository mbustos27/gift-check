//! Maps a Shopify storefront `/products.json` page to products. Pure data; the fetching is in the
//! fetch crate.

use crate::model::{Product, Variant};
use crate::stores::StoreCfg;
use crate::text::{clean, strip_html, truncate};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
struct Page {
    products: Option<Vec<Value>>,
}

#[derive(Deserialize)]
struct RawProduct {
    id: u64,
    title: String,
    handle: String,
    #[serde(default)]
    body_html: Option<String>,
    #[serde(default)]
    vendor: Option<String>,
    #[serde(default)]
    product_type: Option<String>,
    #[serde(default)]
    tags: Option<Value>,
    #[serde(default)]
    variants: Vec<RawVariant>,
    #[serde(default)]
    images: Vec<RawImage>,
}

#[derive(Deserialize)]
struct RawVariant {
    id: u64,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    sku: Option<String>,
    #[serde(default)]
    price: Option<Value>,
    #[serde(default)]
    compare_at_price: Option<Value>,
    #[serde(default)]
    available: Option<bool>,
    #[serde(default)]
    option1: Option<String>,
    #[serde(default)]
    option2: Option<String>,
    #[serde(default)]
    option3: Option<String>,
}

#[derive(Deserialize)]
struct RawImage {
    src: String,
}

/// Shopify writes prices as strings ("68.00"); older themes as numbers.
fn money(v: &Option<Value>) -> Option<f64> {
    let n = match v.as_ref()? {
        Value::String(s) => s.trim().parse::<f64>().ok()?,
        Value::Number(n) => n.as_f64()?,
        _ => return None,
    };
    (n.is_finite() && (0.0..=crate::capture::MAX_PRICE).contains(&n)).then(|| (n * 100.0).round() / 100.0)
}

/// Handles are lower-case letters, digits and '-'; anything else is percent-encoded.
fn path_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.' {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The products on one page. An empty list means the catalog has ended.
pub fn map_page(body: &str, store: &StoreCfg) -> Result<Vec<Product>, String> {
    let page: Page = serde_json::from_str(body).map_err(|e| format!("not a Shopify products.json reply ({e})"))?;
    let list = page.products.ok_or("not a Shopify products.json reply (no \"products\" list)")?;
    let mut out = Vec::with_capacity(list.len());
    for (i, v) in list.into_iter().enumerate() {
        let r: RawProduct = serde_json::from_value(v).map_err(|e| format!("product #{} on the page: {e}", i + 1))?;
        out.push(map_product(r, store));
    }
    Ok(out)
}

fn map_product(r: RawProduct, store: &StoreCfg) -> Product {
    let tags: Vec<String> = match r.tags {
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).map(clean).filter(|t| !t.is_empty()).collect(),
        Some(Value::String(s)) => s.split(',').map(clean).filter(|t| !t.is_empty()).collect(),
        _ => vec![],
    };
    let variants: Vec<Variant> = r
        .variants
        .iter()
        .map(|v| {
            let price = money(&v.price);
            // A compare-at price only counts as a list price when it is above the price.
            let list_price = money(&v.compare_at_price).filter(|c| price.is_none_or(|p| *c > p)).or(price);
            Variant {
                id: v.id.to_string(),
                title: v.title.as_deref().map(clean).filter(|t| !t.is_empty() && t != "Default Title"),
                sku: v.sku.as_deref().map(clean).filter(|s| !s.is_empty()).map(|s| truncate(&s, 64)),
                price,
                list_price,
                available: v.available,
                options: [&v.option1, &v.option2, &v.option3].into_iter().flatten().map(|o| clean(o)).filter(|o| !o.is_empty() && o != "Default Title").collect(),
            }
        })
        .collect();
    // The product's price: the cheapest variant you can buy (or the cheapest at all when none can be).
    let buyable: Vec<&Variant> = variants.iter().filter(|v| v.available != Some(false) && v.price.is_some()).collect();
    let pool: Vec<&Variant> = if buyable.is_empty() { variants.iter().filter(|v| v.price.is_some()).collect() } else { buyable };
    let cheapest = pool.into_iter().min_by(|a, b| a.price.unwrap_or(f64::MAX).total_cmp(&b.price.unwrap_or(f64::MAX)));
    let in_stock = if variants.is_empty() || variants.iter().all(|v| v.available.is_none()) { None } else { Some(variants.iter().any(|v| v.available == Some(true))) };
    let image = r.images.first().map(|i| if i.src.starts_with("//") { format!("https:{}", i.src) } else { i.src.clone() }).filter(|u| store.allows_image(u));
    let description = r.body_html.as_deref().map(strip_html).filter(|d| !d.is_empty()).map(|d| truncate(&d, 2000));
    Product {
        store: store.id.clone(),
        id: r.id.to_string(),
        name: truncate(&clean(&r.title), 300),
        url: format!("{}/products/{}", store.origin, path_segment(&r.handle)),
        category: r.product_type.as_deref().map(clean).filter(|t| !t.is_empty()).map(|t| truncate(&t, 64)),
        tags,
        vendor: r.vendor.as_deref().map(clean).filter(|v| !v.is_empty()),
        description,
        in_stock,
        currency: store.currency.clone(),
        price: cheapest.and_then(|v| v.price),
        list_price: cheapest.and_then(|v| v.list_price),
        image,
        variants,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> StoreCfg {
        StoreCfg { image_hosts: vec!["cdn.shopify.com".into()], ..StoreCfg::ad_hoc_shopify("shop.example", "USD") }
    }

    // Hand-made, shaped like a real storefront products.json page.
    const PAGE: &str = r#"{"products":[
      {"id":101,"title":"Tiny Star Charm","handle":"tiny-star-charm","body_html":"<p>Solid <b>14k</b> gold &amp; love.</p><script>x()</script>",
       "vendor":"Shop","product_type":"Charm","tags":["gold","star"],
       "variants":[
         {"id":1001,"title":"Gold","sku":"TS-G","price":"120.00","compare_at_price":"150.00","available":false,"option1":"Gold","option2":null,"option3":null},
         {"id":1002,"title":"Silver","sku":"TS-S","price":"60.00","compare_at_price":null,"available":true,"option1":"Silver","option2":null,"option3":null}],
       "images":[{"src":"https://cdn.shopify.com/s/files/1/star.jpg"}]},
      {"id":102,"title":"Link Bracelet","handle":"link bracelet","body_html":null,"vendor":"Shop","product_type":"","tags":"bracelet, chain",
       "variants":[{"id":1003,"title":"Default Title","price":"80.00","compare_at_price":"80.00","available":false,"option1":"Default Title"}],
       "images":[{"src":"https://tracker.example/i.jpg"}]}
    ]}"#;

    #[test]
    fn maps_products_variants_and_prices() {
        let ps = map_page(PAGE, &store()).unwrap();
        assert_eq!(ps.len(), 2);
        let star = &ps[0];
        assert_eq!(star.id, "101");
        assert_eq!(star.url, "https://shop.example/products/tiny-star-charm");
        assert_eq!(star.description.as_deref(), Some("Solid 14k gold & love."));
        assert_eq!(star.category.as_deref(), Some("Charm"));
        // The gold variant is sold out, so the product's price is the silver one, with no discount
        // (its compare_at_price is null).
        assert_eq!((star.price, star.list_price), (Some(60.0), Some(60.0)));
        assert_eq!(star.in_stock, Some(true));
        assert_eq!(star.variants[0].list_price, Some(150.0));
        assert_eq!(star.variants[0].available, Some(false));
        assert_eq!(star.variants[1].options, vec!["Silver"]);
        assert_eq!(star.image.as_deref(), Some("https://cdn.shopify.com/s/files/1/star.jpg"));

        let link = &ps[1];
        assert_eq!(link.url, "https://shop.example/products/link%20bracelet");
        assert_eq!(link.tags, vec!["bracelet", "chain"]);
        assert_eq!(link.category, None);
        assert_eq!(link.in_stock, Some(false));
        assert_eq!((link.price, link.list_price), (Some(80.0), Some(80.0)), "compare_at equal to the price is no discount");
        assert_eq!(link.variants[0].title, None);
        assert_eq!(link.image, None, "images from unknown hosts are dropped");
    }

    #[test]
    fn empty_page_and_non_shopify_replies() {
        assert!(map_page(r#"{"products":[]}"#, &store()).unwrap().is_empty());
        assert!(map_page("<html>Just a moment...</html>", &store()).is_err());
        assert!(map_page(r#"{"items":[]}"#, &store()).is_err());
    }
}
