# gift-check

Shop for gifts across store websites, on this computer. gift-check pulls whole store catalogs
into a local database, lets you search and compare them, keeps a price history, and helps plan
a gift, such as a charm bracelet plus charms within a budget, with the store's own compatibility
and sizing rules checked for you.

A sibling of `buy-check`: same Rust workspace layout, same local-window security model.

## What it never does

- Sign in anywhere, read accounts, or touch carts, checkout or wishlists.
- Send personal data anywhere. Recipients (a nickname, wrist size, likes, budget) and plans stay
  in the local database.
- Get around bot protection. A Cloudflare/Akamai/DataDome challenge stops the fetch at once with
  advice to use browser capture. No header spoofing, no TLS or fingerprint tricks, no headless
  browser, no challenge solving, no proxy rotation.

## Two ways data gets in

**1. Direct Shopify fetch** (`source = "shopify"` in `data/stores.toml`). Many small jewelry
brands run on Shopify, which publishes the public catalog at `/products.json`. gift-check:

- reads `robots.txt` first and obeys it (RFC 9309: consecutive `user-agent` lines form one
  group, groups naming `gift-check` are combined, longest match wins, `*` and `$` wildcards,
  allow beats disallow on a tie, any line ending, a leading BOM ignored; a 5xx robots.txt means
  "everything disallowed"). Redirects of robots.txt are followed up to 5 times on the same site
  (the host with or without `www.`); a redirect that can't be followed (no or a bad `Location`,
  https to http, another site) counts as unreachable, so nothing is fetched,
- identifies itself honestly as `gift-check/<version> (personal gift price lookup)`,
- waits at least 3 s between requests to one host (longer if robots.txt sets a crawl-delay; a
  crawl-delay above 60 s stops the run instead). The last request time and any Retry-After are
  kept per host in the database, so the next run (command line or window) can't start inside the
  spacing or before the time the store asked for,
- pages through `/products.json?limit=250&page=N` (or `/collections/<handle>/products.json`)
  until an empty page, and stops if a page repeats the previous page's products (the store
  ignores `page`),
- on a 429 waits for `Retry-After` once per run (at most 120 s), tries once more, and stops on any
  further 429, keeping everything fetched so far.

A browser capture script is never used to get around a rate limit or block the fetcher hit.
Shopify stores go through `gift-check fetch shopify` only.

**2. Browser capture** (`source = "browser-capture"`). Stores behind a bot check (Pandora US
answers plain HTTP with a Cloudflare challenge) are read in your own browser, where you are a
normal visitor. A capture script in the store tab reads the listings on the page and posts them
to the **bridge**, a page of the gift-check window. Why a bridge: a public site can't send
requests to `127.0.0.1` (Local Network Access blocks it), but two windows can talk with
`postMessage`. So the bridge opens the store (keeping the opener), accepts a message only from
a window it opened and only from that store's origin, and hands it to the local server, which
validates it exactly like a capture file. Captures can also be saved as a file and imported.
The script can also save the capture as a file (`gcPandora.download()`), imported with `gift-check
import` or the window's Captures tab; use that in Claude's built-in browser, which opens the store
in the same tab instead of a new one, so the bridge can't be reached there.
Format: [docs/CAPTURE.md](docs/CAPTURE.md). Script contract: [capture/README.md](capture/README.md).

Every capture or fetch appends a price observation per product and variant, so history builds
up; a capture older than what is stored never overwrites newer values. A browser capture marks
products as "not in the latest capture" only within the listings it read completely
(`complete_listings`), and never more than 20% of a listing at once; a full Shopify read that
reached the empty page marks what the catalog no longer has. Nothing is deleted.

## Commands

```
gift-check stores
gift-check fetch shopify www.hardjewelry.com [--collection <handle>]
gift-check fetch shopify shop.example.com --currency USD   # a store not in stores.toml
gift-check import capture.json [--force]           # --force: older than 30 days
gift-check search lucky cat [--store pandora-us] [--kind charm] [--max 60] [--on-sale] [--json]
gift-check product pandora-us 794948C01            # details and price history
gift-check recipient add Mom --likes cats,stars --budget 150 --wrist 16.5 --metal Silver
gift-check recipient edit Mom --systems pandora-moments --owns pandora-us:794948C01
gift-check recipient list
gift-check plan --recipient Mom --budget 150 [--store pandora-us] [--json]
gift-check export --json                           # everything, for agents
gift-check ui [--no-open] [--port 8781]
```

`--data-dir DIR` (anywhere on the line) uses another data folder; the default is
`%APPDATA%\gift-check` (`gift.db` inside).

## The window

`gift-check ui` serves `http://127.0.0.1:8781/` and opens it in the default browser.

- **Catalog**: search all stores; filter by store, kind, metal, price range, on sale, in stock;
  sort by price, % off, newest captured. Rows link to the product page (new tab, no referrer)
  and open a detail view with variants and price history.
- **Build a gift**: pick a recipient, budget and store; pick a bracelet; add charms, clips and
  safety chains (only ones the rules say fit are listed). Totals at current and list price,
  savings, and warnings: system mismatch, already owned, over budget, out of stock, too many
  clips, engraving or Last Chance final sale, bracelet size for the recipient's wrist, a fit that
  rests on an inferred system, a bracelet not recognised as a charm bracelet, prices seen more
  than 7 days ago, mixed currencies (refused). Each line says which variant was priced and when.
  "Suggest gifts" proposes combinations within the budget. Saved plans keep the prices they were
  saved at and are re-priced from the latest observations; the list shows the price date.
- **Compare**: store vs store or category vs category, per kind: count, min, median, max, share
  on sale.
- **Recipients**: nickname, wrist size, metal preference, systems/brands they have, items they
  own (`store:id`), likes, budget, notes.
- **Captures**: per store, "Fetch now" (Shopify) or "Capture in the browser" (bridge); "Import a
  capture file" (same checks as `gift-check import`); every capture and fetch with counts and age.

Security (the buy-check model): binds to 127.0.0.1 only; refuses any Host but its own (blocks
DNS rebinding); API reads refuse other origins; every POST needs the `X-Gift-Check: 1` header
and a JSON body (impossible cross-site without a preflight the server never approves); strict
CSP with `img-src` limited to the stores' image hosts; store text is only ever set with
`textContent`.

## Rules as data

`data/rules.toml` holds every store fact: how to tell a product's kind (names, Pandora style
numbers), metal group, compatibility system (Pandora Moments, ME, Reflexions, Essence), which
add-on systems fit which bracelets, sizing charts, clip limits, policies (engraving, Last
Chance, "FINAL SALE - " names and pre-engraved ENG ids are final sale: the planner suggests a
final-sale item only when nothing returnable fits, and says so) and notes. Each rule has a `source` and an `as_of` date; the shipped
Pandora facts cite the research report `2026-11-15-pandora-buying-guide.md`. A bracelet is a
charm-bracelet base only when something names its system (Moments, a snake chain, a charm
bangle, ME, Reflexions, or its collection): tennis, chain and cord bracelets are not. Add-ons no
rule names fall back to Moments, marked inferred, and the planner says "check the product page". Edit the file and
products are re-classified the next time gift-check starts. A copy in the data folder overrides
the repository's.

`data/stores.toml` is the store allowlist: origin, image hosts, source kind and the start
URLs the bridge opens.

## Build

Windows, GNU toolchain (no MSVC): PATH needs `~/.cargo/bin` and
`%LOCALAPPDATA%\Programs\winlibs\mingw64\bin`.

```
cargo test
cargo clippy --all-targets -- -D warnings
node --test "tests/js/*.test.js"   # capture script and bridge filter
cargo build --release        # target\release\gift-check.exe
```

Workspace: `crates/core` (storage, validation, rules, planner; no network), `crates/fetch` (the
only network code), `crates/ui` (window and bridge), `crates/cli` (the `gift-check` binary).
Tests never reach a real store: the fetcher is tested against a local mock server.
