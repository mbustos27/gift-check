# Capture format (schema 1)

Some stores (Pandora US among them) answer plain HTTP requests with a bot check. gift-check
never tries to get past one. Instead, the listings are read in a real browser, by the person
shopping, and handed to gift-check as a **capture**: a JSON document in the format below.

A capture reaches gift-check in one of three ways, all validated by the same code
(`crates/core/src/capture.rs`):

1. **File, command line**: `gift-check import capture.json` (`--force` takes one older than 30 days).
2. **File, window**: the Captures tab's "Import a capture file" (same checks as `import`, no
   `--force`). `pandora.js` saves one with `gcPandora.download()`.
3. **Bridge**: a capture script in the store tab posts it to the bridge page
   (`http://127.0.0.1:8781/bridge`), in one or more chunks. See `capture/README.md`. In Claude's
   built-in browser the bridge opens the store in the same tab, so there is no `window.opener`
   and only the file routes work.

## The document

```json
{
  "schema": 1,
  "store": "pandora-us",
  "origin": "https://us.pandora.net",
  "captured_at": "2026-11-15T16:00:00Z",
  "complete": false,
  "complete_listings": ["charms"],
  "sources": ["https://us.pandora.net/en/charms/?page=2"],
  "products": [
    {
      "id": "794948C01",
      "name": "Lucky Cat Dangle Charm",
      "url": "https://us.pandora.net/en/charms/....html",
      "category": "charms",
      "listing": "charms",
      "kind": "charm",
      "metal": "Sterling silver",
      "metal_group": "Silver",
      "collection": "Pandora Moments",
      "themes": ["cats"],
      "badges": ["NEW", "TOP GIFT"],
      "engravable": false,
      "in_stock": true,
      "currency": "USD",
      "list_price": 98.0,
      "price": 68.0,
      "image": "https://us.pandora.net/dw/image/v2/....jpg",
      "variants": [
        { "id": "794948C01-17", "title": "17 cm", "sku": "794948C01-17", "price": 68.0, "list_price": 98.0, "available": true }
      ]
    }
  ]
}
```

### Top level

| Field | Required | Rules |
|---|---|---|
| `schema` | yes | Exactly `1`. Anything else is refused ("schema 2 isn't supported"). |
| `store` | yes | A store `id` from `data/stores.toml`. |
| `origin` | yes | Must equal that store's `origin` (a trailing `/` is tolerated). |
| `captured_at` | yes | RFC 3339 time (`2026-11-15T16:00:00Z`, fractions and `+01:00` offsets allowed): when the first page of the capture was read. Stored as UTC. Not more than a day in the future, and not more than 30 days ago (only `gift-check import --force` takes an older one). |
| `complete` | no (default `false`) | `true` only when **every** listing the script reads was read completely. Shown in the window; it marks nothing by itself (see below). |
| `complete_listings` | no | Up to 100 listing names (each at most 64 characters) that were read completely: every product the listing reports was read, no page of it skipped or failed. Only these listings can mark products gone. |
| `sources` | no | Up to 1000 page URLs the capture read; each must be https on the store's host. |
| `products` | yes | Up to 50,000 per file (5,000 per bridge chunk). |
| `type` | bridge only | If present it must be `"gift-check-capture"`. |
| `capture_id`, `chunk`, `chunks` | bridge only | See "Chunks". A file whose `chunks` is above 1 is refused by `import`. |

Unknown fields anywhere are ignored, so capture scripts can carry extra notes.

### A product

| Field | Required | Rules |
|---|---|---|
| `id` | yes | 1-64 characters of `A-Z a-z 0-9 . _ -` (a number is accepted and turned into text). |
| `name` | yes | Plain text, at most 300 characters after whitespace is collapsed. It is **never** treated as HTML: a name like `<script>…` is stored and shown as those characters. |
| `url` | yes | https on the store's own host. |
| `category` | no | At most 64 characters (the store's own category, e.g. Pandora's primary category). Never used to decide what is gone. |
| `listing` | no | At most 64 characters: the listing page the product was read from (for `pandora.js` the listing key, such as `charms`). Scopes `complete_listings`. |
| `kind` | no | One of `charm bracelet clip spacer safety-chain necklace ring earring gift-set other`. When missing, `data/rules.toml` infers it. A stated kind always wins over the rules. |
| `metal` | no | At most 100 characters, e.g. "Sterling silver", "14k rose gold-plated". |
| `metal_group` | no | At most 64 characters, e.g. "Silver", "Gold", "Rose gold", "Two Tone". Inferred from the rules when missing. |
| `collection` | no | At most 120 characters. |
| `themes` | no | Up to 30 strings of at most 60 characters. Missing means "unknown"; `[]` means "none". |
| `badges` | no | Up to 12 strings of at most 40 characters ("NEW", "TOP GIFT", "Last Chance", "COMPATIBLE WITH ME & MOMENTS"). Missing means "unknown". |
| `engravable` | no | `true`/`false`. |
| `in_stock` | no | `true`, `false` or `null` (unknown). |
| `currency` | no | Must equal the store's currency (USD for both shipped stores). |
| `list_price` | no | The regular price. A finite number from 0 to 1,000,000; rounded to cents. |
| `price` | no | The price shown now, promotions included; `null` when unknown. When `price` is missing, the list price counts as the current price. A price above the list price is kept with a note on the capture (often a label such as "$812 VALUE" read as the price); the planner then counts the higher of the two as the regular price, so a saving is never negative. |
| `image` | no | https on the store's host or one of its `image_hosts` in `stores.toml`. An image anywhere else is dropped with a note; the product is kept. |
| `variants` | no | Up to 200, each `{ id (required), title, sku, price, list_price, available }` with the same limits. |

`NaN`, `Infinity` and comments are not JSON, so a file containing them is refused as a whole.
Text fields lose control characters and runs of whitespace; nothing is ever parsed as HTML.

### Duplicates

The same `id` may appear more than once (a product listed on two pages, or in two chunks). The
**first** sighting's values win; later sightings only fill in what the first left unknown, and
their themes, badges and variants are added. If two sightings disagree on price, the capture is
still accepted and a note is kept on the capture ("appeared twice with different prices").

### Listings, `complete` and discontinued products

Every capture appends one price observation per product (and per variant); that is the price
history. A product the capture doesn't mention is never deleted.

- Only `complete_listings` marks anything: products of that store whose stored `listing` is one of
  them, and which this capture didn't include, are marked "not in the latest capture" (hidden from
  search by default, still in the database, back as soon as a capture sees them). Products from
  other listings are untouched, so a complete read of `bracelets` says nothing about `charms`,
  whatever the products' categories say.
- `complete: true` without `complete_listings` marks nothing (a note on the capture says so).
- Safety limit: if marking would hide more than 20% of a listing's stored products, nothing at all
  is marked and the capture gets a note instead (a listing that drew half its tiles, or was
  renamed, should not wipe the catalog).
- A capture with `complete_listings` (or `complete: true`) and no products at all is refused.
- A capture older than what is stored for a product (its last sighting or price time) adds to the
  history but never overwrites the newer values; it only fills in what is unknown.

## Chunks (bridge only)

A large capture is sent as several messages that share a `capture_id`:

```json
{ "type": "gift-check-capture", "schema": 1, "capture_id": "pandora-20261115-a1b2c3",
  "chunk": 1, "chunks": 3,
  "store": "pandora-us", "origin": "https://us.pandora.net",
  "captured_at": "2026-11-15T16:00:00Z", "complete": false, "complete_listings": ["charms"],
  "sources": ["https://us.pandora.net/en/charms/"],
  "products": [ ...this chunk's products... ] }
```

- `capture_id`: 8-64 characters of `A-Z a-z 0-9 - _`, unique per capture run.
- `chunk`: 1..`chunks`; `chunks`: 1..500. Keep each message under 8 MB and 5,000 products.
- Every chunk repeats `store`, `origin`, `captured_at`, `complete`, `complete_listings` and `chunks`
  unchanged; a chunk that disagrees with the first one is refused.
- `sources` may differ per chunk; they are combined.
- Chunks may arrive in any order. Each chunk is validated and stored as soon as it arrives, so the
  window shows progress and nothing is lost if the tab closes halfway.
- A chunk number already received is acknowledged and ignored (safe to resend).
- When all chunks have arrived the capture is closed; only then do `complete_listings` mark
  missing products (a product in any chunk counts as seen).
- At most 4 captures can be half-received at once; when a fifth starts, the oldest is closed as
  incomplete (its products are kept, nothing is marked discontinued). A late chunk of that capture
  is refused with an error ack; send the capture again under a new `capture_id`.
- Captures still half-received when gift-check stops are closed as incomplete when it starts again.
- The bridge refuses a message holding `NaN` or `Infinity` (JSON would turn them into `null`).
