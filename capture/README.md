# Capture scripts

A capture script runs **in a store tab**, reads the product listings on the page (the same text
and prices the person sees), and hands them to gift-check: through the bridge, or as a file the
person imports. It is for stores that answer plain HTTP with a bot check; gift-check never tries
to get past those.

Each store has its own script here (for example `pandora.js`). This file is the contract every
script must follow.

## What a script may do

- Read the page it runs in: product tiles, names, prices, links, image URLs, badges.
- Scroll or click the store's own "next page" / "load more" controls, at a human pace.
- Read the store's `/robots.txt` once per run (same site) and obey it for user-agent `*`
  (RFC 9309). If it can't be read (a 5xx, a network error, a 401/403/429 or a web page instead of a
  robots file), read nothing.
- Load the store's own listing pages (same site only, allowed by robots.txt) one at a time, with a
  pause of a few seconds between pages, in a frame the person can see (`pandora.js` shows it in a
  corner panel), so the store's own scripts show the current prices.
- Stop the whole run at once on the first page that isn't a normal listing: an HTTP status other
  than 200 (403, 429, ...), a bot check, a page without the listing data or without product tiles,
  or a frame that can't be read. No retries, no waiting it out.
- Post the listings to the bridge as described below, or save them as a capture file.

## The shipped script

| Script | Store | How it reads | Use |
|---|---|---|---|
| `pandora.js` | pandora-us | Reads `/robots.txt` once, then renders each allowed listing page (charms, bracelets, necklaces, rings, earrings, gift sets) in a same-origin frame shown in a corner panel ("gift-check is reading Pandora listings — page N"), and merges the tiles the page draws (shown sale prices) with the page's structured data (metal, category, collection, regular price). Stops the run on the first page that isn't a normal listing. Keeps results in the site's IndexedDB, each record tagged with its run id; only the latest run is sent (records from before run ids are the run `legacy-2026-11-15`). | Load in a us.pandora.net tab, `gcPandora.both()` (reads every page, then re-reads each listing's first page and pages with few sale prices, because sale prices sometimes arrive late). Then either `await gcPandora.download()` and import the file, or, in the tab the bridge opened, load the script again and `await gcPandora.send()`. A run is sent once; `send({again: true})` resends, `send({run: "<id>"})` / `download({run: "<id>"})` pick another run. |

Shopify stores are not captured in the browser: they go through `gift-check fetch shopify` only.

## What a script must never do

- Sign in, read or change an account, touch the cart, checkout, wishlist or saved payment details.
- Send anything anywhere except `window.opener.postMessage(…)` to the bridge, or a file the person
  saves (no `fetch` to other sites, no beacons, no storage of personal data).
- Solve, skip or hide a bot check. If the store shows one, the person deals with it in the tab
  like any visitor; the script stops.
- Work around a rate limit or a block. A capture script is never a way around a 429, a
  Retry-After or a refusal that `gift-check fetch` (or the script itself) hit; when the store says
  stop, everything stops and the person tries again later.
- Put anything but product listing data in the message.

## Two ways to hand the capture over

**A file (works in every browser).** `gcPandora.download()` saves the capture as
`gift-check-pandora-<YYYY-MM-DD>.json`. Import it in the gift-check window (Captures tab, "Import
a capture file") or with `gift-check import <file>`. Same format, same validation.

**The bridge (a normal browser such as Chrome, Edge, Firefox or Opera GX).** The bridge opens the
store in a new tab and keeps the opener, so the script can post straight back. Described below.

### Claude's built-in browser

In the Claude desktop app's built-in browser, the bridge's `window.open` doesn't open a new tab:
the store opens **in the same tab**, replacing the bridge, so `window.opener` is `null` and
nothing can be posted back. There, use `gcPandora.download()` and import the file. `send()` says
so when `window.opener` is missing.

## Talking to the bridge

1. The person opens `http://127.0.0.1:8781/bridge` (gift-check ui, Captures tab, "Open the
   bridge") and clicks "Open …" for the store. The bridge opens the store with `window.open`,
   so in the store tab `window.opener` is the bridge.
2. The script builds a capture in the format of `docs/CAPTURE.md` (schema 1) and sends it:

```js
const BRIDGE = "http://127.0.0.1:8781";          // the bridge's exact origin
const captureId = "pandora-" + Date.now().toString(36) + "-" + Math.random().toString(36).slice(2, 8);
const header = {
  type: "gift-check-capture", schema: 1, capture_id: captureId,
  store: "pandora-us", origin: location.origin,
  captured_at: firstPageReadAt,                  // when the first page of this run was read
  complete: false, complete_listings: [],        // see "complete and listings" below
};

function send(chunk, chunks, products, sources) {
  if (!window.opener) throw new Error("window.opener is missing: use a capture file instead.");
  window.opener.postMessage({ ...header, chunk, chunks, sources, products }, BRIDGE);
}
```

   - Always pass the bridge origin as the second argument of `postMessage`, never `"*"`, and
     refuse any target that isn't exactly `http://127.0.0.1:<port>`. If the bridge tab was closed
     or replaced, the browser then drops the message instead of handing it to whatever page now
     sits there.
   - Split big captures into chunks of at most 5,000 products (and under 8 MB); every chunk
     repeats the header fields unchanged. Number them `1..chunks`.
   - Keep every field inside the limits of `docs/CAPTURE.md` (drop over-long badges, themes and
     categories, cut the name to 300 characters) and never send `NaN` or `Infinity`: the bridge
     refuses a message that carries one.

3. The bridge accepts a message only if it comes from a window **it opened** (`event.source`)
   and from **that store's origin** (`event.origin` must equal `origin` in `data/stores.toml`).
   It then posts the message to the gift-check server, which validates it exactly like
   `gift-check import` and stores it.

4. The bridge answers every message it accepted with an acknowledgement posted back to the
   store tab (target origin = the store's origin):

```js
{ type: "gift-check-ack", capture_id, chunk, ok: true,  done: false, stored: 240 }
{ type: "gift-check-ack", capture_id, chunk, ok: false, error: "product #3 (id \"x\"): url … must be an https page on us.pandora.net" }
```

   A script should wait for the ack of one chunk before sending the next, check
   `event.origin === BRIDGE` and `event.source === window.opener` before trusting an ack, and
   resend a chunk whose ack never came (resending is safe: a repeated chunk is ignored).

```js
function waitAck(chunk) {
  return new Promise((resolve, reject) => {
    const t = setTimeout(() => { removeEventListener("message", on); reject(new Error("no answer from the bridge")); }, 30000);
    function on(e) {
      if (e.origin !== BRIDGE || e.source !== window.opener) return;
      const d = e.data;
      if (!d || d.type !== "gift-check-ack" || d.capture_id !== captureId || d.chunk !== chunk) return;
      clearTimeout(t); removeEventListener("message", on);
      d.ok ? resolve(d) : reject(new Error(d.error));
    }
    addEventListener("message", on);
  });
}
```

## When `window.opener` is missing

Besides Claude's built-in browser (above), some sites cut the link to the opener (a
`Cross-Origin-Opener-Policy` header, or a redirect through another site). Then the bridge can't
be reached from the tab: use the capture file (`gcPandora.download()`, then import it).

## The store tab can navigate the bridge tab

Because the bridge opens the store with the opener kept, the store page holds `window.opener`
and could send the bridge tab somewhere else (`opener.location = "https://…"`). Browsers allow
that for tabs a page opened, and the bridge can't block it. What limits it:

- The bridge page holds nothing worth taking, and a store page can't read it anyway (another
  origin).
- The gift-check server only takes captures through its own API: same origin, the
  `X-Gift-Check: 1` header and a JSON body, validated like `gift-check import`. A page the bridge
  tab was sent to can't post there.
- Scripts post with the exact bridge origin as `targetOrigin`, so after such a navigation their
  messages are dropped by the browser, not delivered to the new page.
- The bridge refuses messages that carry `NaN` or `Infinity` (they would silently become `null`
  in JSON).

If the bridge tab ever shows another address, close it and open the bridge again from gift-check.

## The rules a capture should help

- Fill `kind` when the page states it (the category page or a product type label); otherwise
  `data/rules.toml` infers it from the name and style number.
- Copy compatibility labels such as "Compatible with ME & Moments" into `badges`, and the
  product's `collection` when the page states one; the rules use them to put a product in the
  right system.
- `complete` and listings: give every product the `listing` it was read from (for `pandora.js`
  the listing key, such as `charms`), and list in the header's `complete_listings` only the
  listings read completely in this run (the distinct products read reach the total the listing
  reports, and no page of it was skipped or failed). Only products whose stored `listing` is in
  `complete_listings` and that the capture didn't include are marked "not in the latest capture".
  `complete: true` only when every listing the script reads is complete. A partial capture is
  still useful: prices and history are kept, nothing is marked discontinued.
