# Capture format (schema 1)

Some stores (Pandora US among them) answer plain HTTP requests with a bot check. gift-check
never tries to get past one. Instead, the listings are read in a real browser, by the person
shopping, and handed to gift-check as a **capture**: a JSON document in the format below.

A capture reaches gift-check in one of two ways, and both are validated by the same code
(`crates/core/src/capture.rs`):

1. **File**: `gift-check import capture.json`.
2. **Bridge**: a capture script in the store tab posts it to the bridge page
   (`http://127.0.0.1:8781/bridge`), in one or more chunks. See `capture/README.md`.

## The document

```json
{
  "schema": 1,
  "store": "pandora-us",
  "origin": "https://us.pandora.net",
  "captured_at": "2026-11-15T16:00:00Z",
  "complete": true,
  "sources": ["https://us.pandora.net/en/charms/?page=2"],
  "products": [
    {
      "id": "794948C01",
      "name": "Lucky Cat Dangle Charm",
      "url": "https://us.pandora.net/en/charms/....html",
      "category": "charms",
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
| `captured_at` | yes | RFC 3339 time (`2026-11-15T16:00:00Z`, fractions and `+01:00` offsets allowed). Stored as UTC. Not more than a day in the future. |
| `complete` | no (default `false`) | `true` only when **every** product in each category the capture contains was captured. See below. |
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
| `category` | no | At most 64 characters. Used to scope `complete` (below). |
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
| `price` | no | The price shown now, promotions included; `null` when unknown. When `price` is missing, the list price counts as the current price. |
| `image` | no | https on the store's host or one of its `image_hosts` in `stores.toml`. |
| `variants` | no | Up to 200, each `{ id (required), title, sku, price, list_price, available }` with the same limits. |

`NaN`, `Infinity` and comments are not JSON, so a file containing them is refused as a whole.
Text fields lose control characters and runs of whitespace; nothing is ever parsed as HTML.

### Duplicates

The same `id` may appear more than once (a product listed on two pages, or in two chunks). The
**first** sighting's values win; later sightings only fill in what the first left unknown, and
their themes, badges and variants are added. If two sightings disagree on price, the capture is
still accepted and a note is kept on the capture ("appeared twice with different prices").

### `complete` and discontinued products

Every capture appends one price observation per product (and per variant); that is the price
history. A product the capture doesn't mention is never deleted.

- `complete: false` (the default): nothing is marked discontinued.
- `complete: true`: products of that store whose `category` is one of the categories **in this
  capture**, and which this capture didn't include, are marked "not in the latest capture"
  (hidden from search by default, still in the database, back as soon as a capture sees them).
  Products in other categories are untouched, so a complete capture of `charms` says nothing
  about `bracelets`.
- A complete capture with no products at all is refused, because it would mark everything gone.

## Chunks (bridge only)

A large capture is sent as several messages that share a `capture_id`:

```json
{ "type": "gift-check-capture", "schema": 1, "capture_id": "pandora-20261115-a1b2c3",
  "chunk": 1, "chunks": 3,
  "store": "pandora-us", "origin": "https://us.pandora.net",
  "captured_at": "2026-11-15T16:00:00Z", "complete": true,
  "sources": ["https://us.pandora.net/en/charms/"],
  "products": [ ...this chunk's products... ] }
```

- `capture_id`: 8-64 characters of `A-Z a-z 0-9 - _`, unique per capture run.
- `chunk`: 1..`chunks`; `chunks`: 1..500. Keep each message under 8 MB and 5,000 products.
- Every chunk repeats `store`, `origin`, `captured_at`, `complete` and `chunks` unchanged; a chunk
  that disagrees with the first one is refused.
- `sources` may differ per chunk; they are combined.
- Chunks may arrive in any order. Each chunk is validated and stored as soon as it arrives, so the
  window shows progress and nothing is lost if the tab closes halfway.
- A chunk number already received is acknowledged and ignored (safe to resend).
- When all chunks have arrived the capture is closed; only then does `complete: true` mark
  missing products (scoped to the categories of all chunks together).
- At most 4 captures can be half-received at once; when a fifth starts, the oldest is closed as
  incomplete (its products are kept, nothing is marked discontinued).
