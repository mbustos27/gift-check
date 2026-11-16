"use strict";
// gift-check bridge. Opens allowlisted store pages (keeping the opener, so the store tab can
// post back), accepts a message only when it comes from a window this page opened AND from that
// store's origin, and hands it to the gift-check server, which validates it like `import`.
// Store text is shown with textContent only.

const $ = (sel) => document.querySelector(sel);

function el(tag, attrs = {}, ...kids) {
  const n = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (v == null || v === false) continue;
    if (k === "class") n.className = v;
    else if (k.startsWith("on")) n.addEventListener(k.slice(2), v);
    else n.setAttribute(k, v === true ? "" : v);
  }
  for (const kid of kids.flat()) {
    if (kid == null || kid === false) continue;
    n.append(kid instanceof Node ? kid : document.createTextNode(String(kid)));
  }
  return n;
}

async function api(path, body) {
  const opts = body === undefined
    ? { headers: { "X-Gift-Check": "1" } }
    : { method: "POST", headers: { "X-Gift-Check": "1", "Content-Type": "application/json" }, body: JSON.stringify(body) };
  const r = await fetch(path, opts);
  let data;
  try { data = await r.json(); } catch { throw new Error(`The gift-check server replied ${r.status}.`); }
  if (!r.ok) throw new Error(data.error || `Error ${r.status}`);
  return data;
}

/** Windows this page opened: { win, origin, store }. */
const opened = [];
/** One progress row per capture id. */
const rows = new Map();
let ignored = 0;
/** Messages are sent to the server one at a time, in the order they arrived. */
let queue = Promise.resolve();

function banner(msg) {
  const b = $("#banner");
  b.textContent = msg || "";
  b.hidden = !msg;
}

function openStore(store, url) {
  // No window features: the store tab needs window.opener to post back to this tab.
  const win = window.open(url, "_blank");
  if (!win) {
    banner("The browser blocked the new tab. Allow pop-ups for this page, or click the button again.");
    return;
  }
  banner("");
  opened.push({ win, origin: store.origin, store: store.id });
}

function progressRow(id) {
  $("#waiting")?.remove();
  if (!rows.has(id)) {
    const li = el("li", {});
    $("#progress").prepend(li);
    rows.set(id, li);
  }
  return rows.get(id);
}

function show(id, cls, text) {
  const li = progressRow(id);
  li.className = cls || "";
  li.textContent = text;
}

function ack(entry, source, msg) {
  try { source?.postMessage({ type: "gift-check-ack", ...msg }, entry.origin); } catch { /* the store tab was closed */ }
}

async function deliver(d, entry, source) {
  const id = typeof d.capture_id === "string" ? d.capture_id : "single";
  const part = d.chunks ? `chunk ${d.chunk} of ${d.chunks}` : "one message";
  show(id, "", `${entry.store}: sending ${part}…`);
  try {
    const r = await api("/api/capture", d);
    const text = r.done
      ? `${entry.store}: done. ${r.stored ?? "?"} products stored${r.complete ? " (complete capture" + (r.discontinued ? `, ${r.discontinued} marked gone` : "") + ")" : " (partial capture)"}${r.merged ? `, ${r.merged} duplicates merged` : ""}.`
      : `${entry.store}: ${r.received} of ${r.chunks} chunks received, ${r.stored} products stored so far${r.duplicate ? " (a repeated chunk was ignored)" : ""}.`;
    show(id, r.done ? "done" : "", text);
    ack(entry, source, { capture_id: d.capture_id, chunk: d.chunk, ok: true, done: !!r.done, stored: r.stored ?? null });
  } catch (e) {
    show(id, "bad", `${entry.store}: ${part} refused: ${e.message}`);
    ack(entry, source, { capture_id: d.capture_id, chunk: d.chunk, ok: false, error: e.message });
  }
}

window.addEventListener("message", (e) => {
  // Only windows this page opened, and only from the store's own origin.
  const entry = opened.find((o) => o.win === e.source);
  if (!entry || e.origin !== entry.origin) {
    ignored += 1;
    const p = $("#ignored");
    p.hidden = false;
    p.textContent = `${ignored} message(s) ignored: not from a store tab this page opened.`;
    return;
  }
  const d = e.data;
  if (!d || typeof d !== "object" || d.type !== "gift-check-capture") return;
  if (d.store !== entry.store) {
    ack(entry, e.source, { capture_id: d.capture_id, chunk: d.chunk, ok: false, error: `this tab was opened for ${entry.store}` });
    return;
  }
  const source = e.source;
  queue = queue.then(() => deliver(d, entry, source));
});

document.addEventListener("DOMContentLoaded", async () => {
  const expected = "http://127.0.0.1:8781";
  $("#target").textContent = location.origin === expected
    ? `Capture scripts post to ${expected}, which is this page.`
    : `This bridge runs at ${location.origin}, but capture scripts post to ${expected}. Open the bridge at ${expected}/bridge (or change the script's target).`;
  try {
    const st = await api("/api/state");
    const box = $("#stores");
    const browserStores = st.stores.filter((s) => s.source === "browser-capture" && s.start_urls.length);
    if (!browserStores.length) box.append(el("p", { class: "hint" }, "No browser-capture stores in stores.toml."));
    for (const s of browserStores) {
      box.append(el("div", { class: "bridge-store" },
        el("strong", {}, s.name),
        s.note ? el("div", { class: "hint" }, s.note) : null,
        el("div", { class: "btns" }, s.start_urls.map((u) => {
          const label = new URL(u).pathname.replace(/^\/(en\/)?|\/$/g, "") || "home";
          return el("button", { type: "button", class: "primary small", onclick: () => openStore(s, u) }, `Open ${label}`);
        }))));
    }
  } catch (e) {
    banner(`Can't reach the gift-check server: ${e.message}`);
  }
});
