# Capture scripts

A capture script runs **in a store tab** that the bridge opened, reads the product listings on
the page (the same text and prices the person sees), and sends them back to gift-check. It is
for stores that answer plain HTTP with a bot check; gift-check never tries to get past those.

Each store has its own script here (for example `pandora.js`). This file is the contract every
script must follow.

## What a script may do

- Read the page it runs in: product tiles, names, prices, links, image URLs, badges.
- Scroll or click the store's own "next page" / "load more" controls, at a human pace.
- Post the listings to the bridge as described below.

## What a script must never do

- Sign in, read or change an account, touch the cart, checkout, wishlist or saved payment details.
- Send anything anywhere except `window.opener.postMessage(…)` to the bridge (no `fetch` to
  other sites, no beacons, no storage of personal data).
- Solve, skip or hide a bot check. If the store shows one, the person deals with it in the tab
  like any visitor; the script waits or stops.
- Put anything but product listing data in the message.

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
  captured_at: new Date().toISOString(), complete: false,   // true only if every product in each category was read
};

function send(chunk, chunks, products, sources) {
  if (!window.opener) throw new Error("Open this tab from the gift-check bridge (window.opener is missing).");
  window.opener.postMessage({ ...header, chunk, chunks, sources, products }, BRIDGE);
}
```

   - Always pass the bridge origin as the second argument of `postMessage`, never `"*"`. If the
     bridge tab was closed or replaced, the browser then drops the message instead of handing
     it to whatever page now sits there.
   - Split big captures into chunks of at most 5,000 products (and under 8 MB); every chunk
     repeats the header fields unchanged. Number them `1..chunks`.

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

Some sites cut the link to the opener (a `Cross-Origin-Opener-Policy` header, or a redirect
through another site). Then the bridge can't be reached from the tab. Fall back to a file: have
the script offer the capture as a download (`capture-pandora-us-<date>.json`) and import it with
`gift-check import <file>`. Same format, same validation.

## The rules a capture should help

- Fill `kind` when the page states it (the category page or a product type label); otherwise
  `data/rules.toml` infers it from the name and style number.
- Copy compatibility labels such as "Compatible with ME & Moments" into `badges`; the rules
  use them to put a charm in the right system.
- Set `complete: true` only when every product of each category in the capture was read (all
  pages of "load more"). A partial capture is still useful: prices and history are kept, nothing
  is marked discontinued.
