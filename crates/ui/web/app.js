"use strict";
// gift-check window. Everything shown comes from this computer's gift-check server. Text from
// stores is data: it is always set with textContent (through el()), never parsed as HTML.

const $ = (sel, root = document) => root.querySelector(sel);

function el(tag, attrs = {}, ...kids) {
  const n = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (v == null || v === false) continue;
    if (k === "class") n.className = v;
    else if (k === "text") n.textContent = v;
    else if (k.startsWith("on")) n.addEventListener(k.slice(2), v);
    else n.setAttribute(k, v === true ? "" : v);
  }
  for (const kid of kids.flat()) {
    if (kid == null || kid === false) continue;
    n.append(kid instanceof Node ? kid : document.createTextNode(String(kid)));
  }
  return n;
}

let STATE = null;

/** A link only to an https page, opened in a new tab without opener or referrer. */
function webLink(url, text) {
  const ok = typeof url === "string" && /^https:\/\//i.test(url);
  return ok ? el("a", { href: url, target: "_blank", rel: "noopener noreferrer" }, text) : document.createTextNode(text);
}

/** Pandora's images are 1500 px packshots with wide transparent margins: ask its own image
 *  service (same host, so the CSP is unchanged) for a 200 px copy on a light background. */
const PANDORA_THUMB = { sw: "200", sh: "200", sm: "fit", sfrm: "png", bgcolor: "F5F5F5" };

/** A thumbnail only from a known store image host (the page's CSP enforces the same). */
function thumb(url) {
  let src = null;
  try {
    const u = new URL(url);
    if (u.protocol === "https:" && (STATE?.image_hosts || []).includes(u.hostname)) {
      if (u.hostname === "us.pandora.net") for (const [k, v] of Object.entries(PANDORA_THUMB)) u.searchParams.set(k, v);
      src = u.href;
    }
  } catch { src = null; }
  return src
    ? el("img", { class: "thumb", src, alt: "", loading: "lazy", referrerpolicy: "no-referrer", decoding: "async" })
    : el("span", { class: "thumb", "aria-hidden": "true" });
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

const money = (v) => (v == null ? "?" : `$${Number(v).toFixed(2)}`);
const debounce = (fn, ms = 250) => { let t; return (...a) => { clearTimeout(t); t = setTimeout(() => fn(...a), ms); }; };
const storeName = (id) => STATE?.stores.find((s) => s.id === id)?.name || id;
const qs = (o) => Object.entries(o).filter(([, v]) => v !== "" && v != null && v !== false).map(([k, v]) => `${encodeURIComponent(k)}=${encodeURIComponent(v === true ? "1" : v)}`).join("&");

function fillSelect(sel, options, keepFirst = true) {
  const first = keepFirst ? sel.options[0] : null;
  const current = sel.value;
  sel.replaceChildren(...(first ? [first] : []));
  for (const o of options) {
    const [value, label] = Array.isArray(o) ? o : [o, o];
    sel.append(el("option", { value }, label));
  }
  if ([...sel.options].some((o) => o.value === current)) sel.value = current;
}

function banner(msg) {
  const b = $("#banner");
  b.textContent = msg || "";
  b.hidden = !msg;
}

/* ---------- Tabs ---------- */
const TABS = ["catalog", "build", "compare", "recipients", "captures"];
function showTab(name, focus = false) {
  if (!TABS.includes(name)) name = "catalog";
  for (const t of TABS) {
    const on = t === name;
    const tab = $(`#tab-${t}`);
    tab.setAttribute("aria-selected", on ? "true" : "false");
    tab.tabIndex = on ? 0 : -1;
    $(`#panel-${t}`).hidden = !on;
  }
  if (focus) $(`#tab-${name}`).focus();
  history.replaceState(null, "", `#${name}`);
  if (name === "compare") loadCompare();
  if (name === "build") { loadPlans(); searchBases(); searchAddons(); }
}
function setupTabs() {
  for (const t of TABS) $(`#tab-${t}`).addEventListener("click", () => showTab(t));
  $(".tabs").addEventListener("keydown", (e) => {
    const i = TABS.findIndex((t) => $(`#tab-${t}`).getAttribute("aria-selected") === "true");
    if (e.key === "ArrowRight") showTab(TABS[(i + 1) % TABS.length], true);
    else if (e.key === "ArrowLeft") showTab(TABS[(i + TABS.length - 1) % TABS.length], true);
  });
}

/* ---------- State ---------- */
async function loadState() {
  STATE = await api("/api/state");
  $("#version").textContent = `v${STATE.version}`;
  const stores = STATE.stores.map((s) => [s.id, s.name]);
  fillSelect($("#f-store"), stores);
  fillSelect($("#b-store"), stores);
  fillSelect($("#f-kind"), STATE.kinds);
  fillSelect($("#f-metal"), STATE.metal_groups);
  fillSelect($("#r-metal"), STATE.metal_groups);
  fillSelect($("#b-recipient"), STATE.recipients.map((r) => r.nick));
  const total = STATE.stores.reduce((n, s) => n + s.products, 0);
  banner(total === 0 ? "No products yet. Open the Captures tab to fetch a store or capture one in your browser." : "");
  renderRecipients();
  renderStores();
  renderCaptures();
}

/* ---------- Product rows ---------- */
function priceBlock(r) {
  return el("div", { class: "price" },
    el("span", { class: "now" }, money(r.price)),
    r.pct_off ? el("span", { class: "was" }, money(r.list_price)) : null,
    r.pct_off ? el("div", { class: "off" }, `${Math.round(r.pct_off)}% off`) : null);
}

function productRow(r, { compact = false, actions = [] } = {}) {
  const facts = [storeName(r.store), r.kind, r.system_label, r.metal_group || r.metal].filter(Boolean).join(" · ");
  const chips = el("span", { class: "chips" },
    (r.badges || []).map((b) => el("span", { class: "chip" }, b)),
    r.in_stock === false ? el("span", { class: "chip gone" }, "out of stock") : null,
    r.discontinued ? el("span", { class: "chip gone" }, "not in latest capture") : null);
  const btns = el("div", { class: "btns" },
    actions.map((a) => el("button", { type: "button", class: a.primary ? "primary small" : "secondary small", onclick: () => a.run(r) }, a.label)),
    el("button", { type: "button", class: "secondary small", onclick: () => openDetail(r.store, r.id) }, "History"),
    !compact ? webLink(r.url, "Product page ↗") : null);
  return el("li", { class: "prod" },
    thumb(r.image),
    el("div", {}, el("div", { class: "name" }, r.name, chips), el("div", { class: "meta" }, facts)),
    el("div", { class: "side" }, priceBlock(r), btns));
}

/* ---------- Catalog ---------- */
let OFFSET = 0;
function filters() {
  return {
    q: $("#f-q").value.trim(), store: $("#f-store").value, kind: $("#f-kind").value, metal: $("#f-metal").value,
    min: $("#f-min").value, max: $("#f-max").value, sale: $("#f-sale").checked, stock: $("#f-stock").checked, sort: $("#f-sort").value,
  };
}
async function search(more = false) {
  OFFSET = more ? OFFSET + 100 : 0;
  try {
    const d = await api(`/api/search?${qs({ ...filters(), limit: 100, offset: OFFSET })}`);
    const list = $("#results");
    if (!more) list.replaceChildren();
    for (const r of d.rows) list.append(productRow(r));
    const shown = list.children.length;
    $("#results-count").textContent = d.total === 0 ? "Nothing matches." : `${d.total} match${d.total === 1 ? "" : "es"}${shown < d.total ? `, showing ${shown}` : ""}.`;
    $("#more").hidden = shown >= d.total;
  } catch (e) {
    $("#results-count").textContent = e.message;
  }
}

/* ---------- Product detail ---------- */
async function openDetail(store, id) {
  const dlg = $("#detail");
  $("#d-name").textContent = "Loading…";
  $("#d-body").replaceChildren();
  if (!dlg.open) dlg.showModal();
  try {
    const d = await api(`/api/product?${qs({ store, id })}`);
    const p = d.product;
    $("#d-name").textContent = p.name;
    const facts = [
      ["Store", storeName(p.store)], ["Id (for owned lists)", `${p.store}:${p.id}`], ["Kind", p.kind], ["System", p.system_label],
      ["Metal", [p.metal, p.metal_group].filter(Boolean).join(" · ")], ["Collection", p.collection], ["Category", p.category],
      ["Now", `${money(p.price)}${p.pct_off ? ` (list ${money(p.list_price)}, ${Math.round(p.pct_off)}% off)` : ""}`],
      ["Price seen", p.price_at], ["In stock", p.in_stock == null ? "unknown" : p.in_stock ? "yes" : "no"],
      ["First seen", p.first_seen], ["Last seen", p.last_seen],
    ].filter(([, v]) => v);
    const dl = el("dl", { class: "facts" }, facts.map(([k, v]) => [el("dt", {}, k), el("dd", {}, v)]));
    const body = $("#d-body");
    body.append(el("div", { class: "detail-top" }, thumb(p.image), el("div", {}, dl, el("p", {}, webLink(p.url, "Open the product page ↗")))));
    for (const pol of d.policies) body.append(el("p", { class: "hint" }, `${pol.text} (source: ${pol.source})`));
    if (p.description) body.append(el("p", { class: "small-text" }, p.description));
    if (p.variants?.length) {
      body.append(el("h3", { class: "sub" }, "Variants"), el("div", { class: "tablewrap" }, el("table", {},
        el("thead", {}, el("tr", {}, el("th", {}, "Variant"), el("th", {}, "SKU"), el("th", { class: "num" }, "Price"), el("th", { class: "num" }, "List"), el("th", {}, "Available"))),
        el("tbody", {}, p.variants.map((v) => el("tr", {},
          el("td", {}, v.title || v.options?.join(" / ") || v.id), el("td", {}, v.sku || ""),
          el("td", { class: "num" }, money(v.price)), el("td", { class: "num" }, money(v.list_price)),
          el("td", {}, v.available == null ? "?" : v.available ? "yes" : "no")))))));
    }
    body.append(el("h3", { class: "sub" }, "Price history"), el("div", { class: "tablewrap" }, el("table", {},
      el("thead", {}, el("tr", {}, el("th", {}, "Seen"), el("th", { class: "num" }, "Price"), el("th", { class: "num" }, "List"), el("th", {}, "From"))),
      el("tbody", {}, [...d.history].reverse().map((h) => el("tr", {},
        el("td", {}, h.at.replace("T", " ").replace("Z", " UTC")), el("td", { class: "num" }, money(h.price)),
        el("td", { class: "num" }, money(h.list_price)), el("td", {}, `${h.source || ""} #${h.capture_id}`)))))));
  } catch (e) {
    $("#d-name").textContent = e.message;
  }
}

/* ---------- Build a gift ---------- */
let PLAN = { id: null, items: [] };
let EVAL = null;
const isBase = (kind) => (STATE?.rules.base_kinds || []).includes(kind);
const recipientNick = () => $("#b-recipient").value;
const recipientObj = () => STATE?.recipients.find((r) => r.nick === recipientNick());
const budget = () => { const v = parseFloat($("#b-budget").value); return Number.isFinite(v) ? v : null; };

function baseLine() {
  return EVAL?.lines.find((l) => isBase(l.kind)) || null;
}

/** The bracelet system add-ons must fit: the chosen bracelet's, else one the recipient already has. */
function fitSystem() {
  const b = baseLine();
  if (b?.system) return { system: b.system, label: b.system_label, why: `fit the ${b.system_label} bracelet in the gift` };
  const known = (recipientObj()?.systems || []).find((s) => STATE.systems.some((x) => x.id === s));
  if (known) {
    const label = STATE.systems.find((x) => x.id === known).label;
    return { system: known, label, why: `fit the ${label} bracelet ${recipientNick()} has` };
  }
  return null;
}

const searchBases = debounce(async () => {
  try {
    const d = await api(`/api/search?${qs({ q: $("#b-base-q").value.trim(), kind: "bracelet", store: $("#b-store").value, stock: true, limit: 30, metal: recipientObj()?.metal || "" })}`);
    const list = $("#b-bases");
    list.replaceChildren(...d.rows.map((r) => productRow(r, { compact: true, actions: [{ label: "Use", primary: true, run: setBase }] })));
    if (!d.rows.length) list.append(el("li", { class: "hint" }, "No bracelets match."));
  } catch (e) { $("#b-bases").replaceChildren(el("li", { class: "hint" }, e.message)); }
});

const searchAddons = debounce(async () => {
  const fit = fitSystem();
  $("#b-fits").textContent = fit ? `Showing only add-ons that ${fit.why}.` : "Pick a bracelet first (or set the bracelet the recipient has) to see only what fits.";
  try {
    const q = { q: $("#b-addon-q").value.trim(), store: $("#b-store").value, stock: true, limit: 40 };
    if (fit) q.fits = fit.system; else q.addons = true;
    const d = await api(`/api/search?${qs(q)}`);
    const list = $("#b-addons");
    list.replaceChildren(...d.rows.map((r) => productRow(r, { compact: true, actions: [{ label: "Add", primary: true, run: addItem }] })));
    if (!d.rows.length) list.append(el("li", { class: "hint" }, "Nothing matches."));
  } catch (e) { $("#b-addons").replaceChildren(el("li", { class: "hint" }, e.message)); }
});

function setBase(r) {
  const baseKeys = new Set((EVAL?.lines || []).filter((l) => isBase(l.kind)).map((l) => `${l.store}:${l.id}`));
  PLAN.items = PLAN.items.filter((i) => !baseKeys.has(`${i.store}:${i.id}`));
  PLAN.items.unshift({ store: r.store, id: r.id, qty: 1 });
  evaluate().then(searchAddons);
}

function addItem(r) {
  PLAN.items.push({ store: r.store, id: r.id, qty: 1 });
  evaluate();
}

function removeItem(index) {
  PLAN.items.splice(index, 1);
  evaluate().then(searchAddons);
}

async function evaluate() {
  $("#p-saved").textContent = "";
  if (!PLAN.items.length) { EVAL = null; renderPlan(); return; }
  try {
    const d = await api("/api/plan/evaluate", { recipient: recipientNick() || null, budget: budget(), items: PLAN.items });
    EVAL = d.evaluation;
    EVAL.missing = d.missing;
  } catch (e) {
    EVAL = null;
    $("#p-saved").textContent = e.message;
  }
  renderPlan();
}

function renderPlan() {
  const lines = $("#p-lines"), totals = $("#p-totals"), warn = $("#p-warnings"), size = $("#p-size");
  lines.replaceChildren(); totals.replaceChildren(); warn.replaceChildren(); size.replaceChildren();
  $("#p-notes").replaceChildren();
  $("#p-notes-box").hidden = true;
  if (!EVAL) {
    lines.append(el("li", { class: "hint" }, "Nothing yet. Pick a bracelet, add charms, or ask for suggestions."));
    return;
  }
  EVAL.lines.forEach((l, i) => lines.append(el("li", {},
    el("div", {}, el("div", { class: isBase(l.kind) ? "base" : "" }, webLink(l.url, l.name)),
      el("div", { class: "meta" }, [l.kind, l.system_label ? `${l.system_label}${l.system_inferred ? " (inferred)" : ""}` : null, l.metal_group].filter(Boolean).join(" · ")),
      el("div", { class: "meta" }, `price ${l.price_at ? `seen ${l.price_at.slice(0, 10)}` : "date unknown"} · for ${l.priced}`)),
    priceBlock(l),
    el("button", { type: "button", class: "x", "aria-label": `Remove ${l.name}`, onclick: () => removeItem(i) }, "✕"))));
  for (const m of EVAL.missing || []) lines.append(el("li", { class: "hint" }, `${m} is no longer in the catalog.`));
  const row = (k, v, big) => [el("span", {}, k), el("span", { class: big ? "big" : "" }, v)];
  totals.append(...row("Total now", money(EVAL.total), true), ...row("At list price", money(EVAL.list_total)));
  if (EVAL.savings > 0) totals.append(...row("You save", money(EVAL.savings)));
  if (EVAL.budget != null) totals.append(...row(EVAL.left < 0 ? "Over budget" : "Left in budget", money(Math.abs(EVAL.left))));
  if (EVAL.unknown_prices) totals.append(...row("Unknown prices", String(EVAL.unknown_prices)));
  if (EVAL.size) {
    const s = EVAL.size;
    size.append(el("p", { class: "size" }, s.bracelet_cm ? `Bracelet size: ${s.bracelet_cm} cm. ` : "The wrist is beyond the size chart. ", s.text, el("span", { class: "hint" }, ` (source: ${s.source})`)));
  }
  const order = { stop: 0, warn: 1, note: 2 };
  [...EVAL.warnings].sort((a, b) => order[a.level] - order[b.level]).forEach((w) => warn.append(el("li", { class: w.level }, w.text)));
  if (!EVAL.warnings.some((w) => w.level !== "note")) warn.append(el("li", { class: "ok-line" }, "No problems found by the rules."));
  if (EVAL.notes.length) {
    $("#p-notes-box").hidden = false;
    $("#p-notes").replaceChildren(...EVAL.notes.map((n) => el("li", {}, n)));
  }
}

async function savePlan() {
  const name = $("#p-name").value.trim();
  if (!name) { $("#p-saved").textContent = "Give the plan a name first."; $("#p-name").focus(); return; }
  if (!PLAN.items.length) { $("#p-saved").textContent = "The plan is empty."; return; }
  try {
    const d = await api("/api/plan/save", { id: PLAN.id, name, recipient: recipientNick() || null, budget: budget(), items: PLAN.items });
    PLAN.id = d.id;
    $("#p-saved").textContent = "Saved.";
    loadPlans();
  } catch (e) { $("#p-saved").textContent = e.message; }
}

async function loadPlans() {
  try {
    const d = await api("/api/plans");
    const list = $("#plans");
    list.replaceChildren();
    if (!d.plans.length) list.append(el("li", { class: "hint" }, "No saved plans yet."));
    for (const x of d.plans) {
      const p = x.plan;
      const flags = [x.stops ? `${x.stops} problem${x.stops === 1 ? "" : "s"}` : null, x.warns ? `${x.warns} to check` : null, x.missing.length ? `${x.missing.length} gone` : null, x.stale ? "prices older than 7 days: capture again before buying" : null].filter(Boolean).join(", ");
      const dated = x.oldest_price_at ? ` · prices from ${x.oldest_price_at.slice(0, 10)}` : "";
      const saved = x.saved_total != null && Math.abs(x.saved_total - x.total) >= 0.005 ? ` · ${money(x.saved_total)} when saved` : "";
      list.append(el("li", {},
        el("div", {}, el("strong", {}, p.name), p.recipient ? ` for ${p.recipient}` : "",
          el("div", { class: "meta" }, `${p.items.length} item${p.items.length === 1 ? "" : "s"} · now ${money(x.total)} (list ${money(x.list_total)})${saved}${dated}${p.budget != null ? ` · budget ${money(p.budget)}` : ""}${flags ? ` · ${flags}` : ""}`)),
        el("div", { class: "btns" },
          el("button", { type: "button", class: "secondary small", onclick: () => loadPlan(p) }, "Open"),
          el("button", { type: "button", class: "secondary small", onclick: () => deletePlan(p) }, "Delete"))));
    }
  } catch (e) { $("#plans").replaceChildren(el("li", { class: "hint" }, e.message)); }
}

function loadPlan(p) {
  PLAN = { id: p.id, items: p.items.map((i) => ({ ...i })) };
  $("#p-name").value = p.name;
  $("#b-recipient").value = p.recipient || "";
  $("#b-budget").value = p.budget ?? "";
  evaluate().then(searchAddons);
}

async function deletePlan(p) {
  if (!confirm(`Delete the saved plan "${p.name}"?`)) return;
  try {
    await api("/api/plan/delete", { id: p.id });
    if (PLAN.id === p.id) PLAN.id = null;
    loadPlans();
  } catch (e) { $("#p-saved").textContent = e.message; }
}

async function suggest() {
  const out = $("#b-suggestions"), note = $("#b-suggest-note");
  out.replaceChildren();
  const b = budget() ?? recipientObj()?.budget;
  if (!b) { note.textContent = "Set a budget first."; return; }
  if ($("#b-budget").value === "") $("#b-budget").value = b;
  note.textContent = "Thinking…";
  try {
    const d = await api("/api/plan/suggest", { recipient: recipientNick() || null, budget: b, store: $("#b-store").value || null });
    note.textContent = d.suggestions.length ? "" : `Nothing fits that budget among ${d.catalog_size} products.`;
    for (const s of d.suggestions) {
      const picks = [s.base, ...s.addons].filter(Boolean);
      out.append(el("div", { class: "sugg" },
        el("div", { class: "head" }, el("span", {}, s.title), el("span", {}, `${money(s.total)}${s.savings > 0 ? ` (save ${money(s.savings)})` : ""}`)),
        el("ul", {}, picks.map((p) => el("li", {}, webLink(p.url, p.name), ` ${money(p.price)}`, el("div", { class: "why" }, p.why),
          (p.policies || []).map((w) => el("div", { class: "policy" }, w))))),
        s.notes.map((n) => el("p", { class: "hint" }, n)),
        el("div", { class: "btns" }, el("button", { type: "button", class: "secondary small", onclick: () => useSuggestion(s) }, "Use this"))));
    }
  } catch (e) { note.textContent = e.message; }
}

function useSuggestion(s) {
  PLAN = { id: null, items: [s.base, ...s.addons].filter(Boolean).map((p) => ({ store: p.store, id: p.id, qty: 1 })) };
  evaluate().then(searchAddons);
  $("#plan-card").scrollIntoView({ behavior: "smooth", block: "start" });
}

/* ---------- Compare ---------- */
let COMPARE = null;
async function loadCompare() {
  try {
    COMPARE = await api(`/api/compare?by=${encodeURIComponent($("#c-by").value)}`);
    const a = $("#c-a"), b = $("#c-b");
    fillSelect(a, COMPARE.groups, false);
    fillSelect(b, COMPARE.groups, false);
    if (b.value === a.value && COMPARE.groups.length > 1) b.value = COMPARE.groups.find((g) => g !== a.value);
    renderCompare();
  } catch (e) { $("#c-table").replaceChildren(el("caption", {}, e.message)); }
}
function renderCompare() {
  const t = $("#c-table");
  t.replaceChildren();
  if (!COMPARE || !COMPARE.groups.length) { t.append(el("caption", { class: "hint" }, "No prices yet.")); return; }
  const groups = [...new Set([$("#c-a").value, $("#c-b").value])].filter(Boolean);
  const cell = (g, k) => COMPARE.stats.find((s) => s.group === g && s.kind === k);
  const label = (g) => ($("#c-by").value === "store" ? storeName(g) : g);
  t.append(el("thead", {},
    el("tr", {}, el("th", {}, "Kind"), groups.map((g) => el("th", { colspan: 5 }, label(g)))),
    el("tr", {}, el("th", {}), groups.map(() => ["Count", "Min", "Median", "Max", "On sale"].map((h) => el("th", { class: "num" }, h))))));
  const kinds = COMPARE.kinds.filter((k) => groups.some((g) => cell(g, k)));
  t.append(el("tbody", {}, kinds.map((k) => el("tr", {}, el("td", {}, k), groups.map((g) => {
    const s = cell(g, k);
    return s
      ? [el("td", { class: "num" }, s.count), el("td", { class: "num" }, money(s.min)), el("td", { class: "num" }, money(s.median)), el("td", { class: "num" }, money(s.max)), el("td", { class: "num" }, `${Math.round(s.share_on_sale * 100)}%`)]
      : [1, 2, 3, 4, 5].map(() => el("td", { class: "num" }, "–"));
  })))));
}

/* ---------- Recipients ---------- */
let EDITING = null;
const splitList = (s) => s.split(/[,\n]/).map((x) => x.trim()).filter(Boolean);

function renderRecipients() {
  const list = $("#r-list");
  list.replaceChildren();
  if (!STATE.recipients.length) list.append(el("li", { class: "hint" }, "No one yet."));
  for (const r of STATE.recipients) {
    const bits = [r.budget != null ? `budget ${money(r.budget)}` : null, r.metal, r.wrist_cm ? `wrist ${r.wrist_cm} cm` : null, r.likes.length ? `likes ${r.likes.join(", ")}` : null].filter(Boolean).join(" · ");
    list.append(el("li", {}, el("div", {}, el("strong", {}, r.nick), el("div", { class: "meta" }, bits || "no details yet")),
      el("button", { type: "button", class: "secondary small", onclick: () => editRecipient(r) }, "Edit")));
  }
  const box = $("#r-systems");
  box.replaceChildren(...STATE.systems.map((s) => el("label", { class: "check", title: s.note }, el("input", { type: "checkbox", value: s.id }), s.label)));
  if (EDITING) editRecipient(STATE.recipients.find((r) => r.nick === EDITING) || null);
}

function editRecipient(r) {
  EDITING = r?.nick || null;
  $("#r-title").textContent = r ? `Edit ${r.nick}` : "Add someone";
  $("#r-nick").value = r?.nick || "";
  $("#r-nick").readOnly = !!r;
  $("#r-wrist").value = r?.wrist_cm ?? "";
  $("#r-metal").value = r?.metal || "";
  $("#r-budget").value = r?.budget ?? "";
  $("#r-likes").value = (r?.likes || []).join(", ");
  const known = new Set(STATE.systems.map((s) => s.id));
  for (const box of document.querySelectorAll("#r-systems input")) box.checked = !!r?.systems.includes(box.value);
  $("#r-systems-other").value = (r?.systems || []).filter((s) => !known.has(s)).join(", ");
  $("#r-owned").value = (r?.owned || []).join("\n");
  $("#r-notes").value = r?.notes || "";
  $("#r-msg").textContent = "";
}

async function saveRecipient(e) {
  e.preventDefault();
  const num = (id) => { const v = parseFloat($(id).value); return Number.isFinite(v) ? v : null; };
  const systems = [...document.querySelectorAll("#r-systems input:checked")].map((b) => b.value).concat(splitList($("#r-systems-other").value));
  const body = {
    nick: $("#r-nick").value.trim(), wrist_cm: num("#r-wrist"), metal: $("#r-metal").value || null, budget: num("#r-budget"),
    likes: splitList($("#r-likes").value), systems, owned: splitList($("#r-owned").value), notes: $("#r-notes").value.trim() || null,
  };
  try {
    await api("/api/recipient", body);
    EDITING = body.nick;
    await loadState();
    $("#r-msg").textContent = "Saved.";
  } catch (err) { $("#r-msg").textContent = err.message; }
}

/* ---------- Stores and captures ---------- */
function ageOf(iso) {
  const t = Date.parse(iso || "");
  if (!Number.isFinite(t)) return "never";
  const s = Math.max(0, (Date.now() - t) / 1000);
  if (s < 3600) return `${Math.round(s / 60)} min ago`;
  if (s < 172800) return `${Math.round(s / 3600)} h ago`;
  return `${Math.round(s / 86400)} days ago`;
}

function renderStores() {
  const list = $("#s-list");
  list.replaceChildren();
  for (const s of STATE.stores) {
    const action = s.source === "shopify"
      ? el("button", { type: "button", class: "primary small", disabled: STATE.fetch.running, onclick: () => startFetch(s) }, `Fetch ${s.name} now`)
      : el("a", { class: "button primary small", href: "/bridge", target: "_blank" }, "Capture in the browser");
    list.append(el("li", {},
      el("div", {}, el("strong", {}, s.name), " ", webLink(`${s.origin}/`, s.origin.replace("https://", "")),
        el("div", { class: "meta" }, `${s.source === "shopify" ? "Shopify fetch" : "browser capture"} · ${s.products} products${s.on_sale ? `, ${s.on_sale} on sale` : ""}${s.discontinued ? `, ${s.discontinued} not in the latest capture` : ""} · last ${ageOf(s.last_capture)}`)),
      action));
  }
  renderFetch(STATE.fetch);
}

function renderFetch(f) {
  const box = $("#fetch-status");
  if (!f || (!f.running && !f.result)) { box.hidden = true; return; }
  box.hidden = false;
  const lines = [`${f.running ? "Fetching" : "Fetched"} ${storeName(f.store)} (started ${f.started_at || "?"})`, ...f.lines];
  if (f.result) {
    if (f.result.error) lines.push(`Error: ${f.result.error}`);
    else {
      lines.push(`${f.result.products} products in ${f.result.pages} page(s), ${f.result.requests} requests${f.result.complete ? ", complete" : ""}${f.result.discontinued ? `, ${f.result.discontinued} marked gone` : ""}.`);
      for (const l of f.result.log || []) lines.push(l);
      if (f.result.stop) lines.push(`Stopped: ${f.result.stop}`);
    }
  }
  box.textContent = lines.join("\n");
}

async function startFetch(s) {
  try {
    await api("/api/fetch", { store: s.id });
    pollFetch();
  } catch (e) { renderFetch({ store: s.id, lines: [], result: { error: e.message } }); }
}

async function pollFetch() {
  try {
    const f = await api("/api/fetch");
    renderFetch(f);
    if (f.running) { setTimeout(pollFetch, 1500); return; }
    await loadState();
    search();
  } catch (e) { banner(e.message); }
}

function renderCaptures() {
  const t = $("#cap-table");
  t.replaceChildren(el("thead", {}, el("tr", {}, ["#", "Store", "How", "When", "Products", "Complete", "Gone", "Note"].map((h) => el("th", {}, h)))));
  const body = el("tbody");
  for (const { capture: c, age } of STATE.captures) {
    body.append(el("tr", {},
      el("td", {}, c.id), el("td", {}, storeName(c.store)), el("td", {}, c.source),
      el("td", { title: c.finished_at || c.started_at }, age ? `${age} ago` : ""), el("td", { class: "num" }, c.product_count),
      el("td", {}, c.finished_at ? (c.complete ? "yes" : "no") : "in progress"), el("td", { class: "num" }, c.discontinued || ""),
      el("td", { class: "small-text" }, c.note || "")));
  }
  if (!STATE.captures.length) body.append(el("tr", {}, el("td", { colspan: 8, class: "hint" }, "Nothing captured yet.")));
  t.append(body);
  $("#paths").textContent = `Data: ${STATE.data_dir} · stores from ${STATE.stores_from} · rules from ${STATE.rules_from} (${STATE.rules.count} rules${STATE.rules.todo ? `, ${STATE.rules.todo} not yet sourced` : ""})`;
}

/* ---------- Import a capture file ---------- */
const MAX_IMPORT_BYTES = 50 * 1024 * 1024;

async function importFile(e) {
  e.preventDefault();
  const out = $("#imp-result");
  const f = $("#imp-file").files[0];
  out.hidden = false;
  if (!f) { out.textContent = "Choose a capture file first."; return; }
  if (f.size > MAX_IMPORT_BYTES) { out.textContent = `${f.name} is ${(f.size / 1048576).toFixed(1)} MB; the limit is 50 MB.`; return; }
  out.textContent = `Importing ${f.name}…`;
  try {
    // The file's own text goes to the server unchanged, so it is checked exactly like `gift-check import`.
    const r = await fetch("/api/import", { method: "POST", headers: { "X-Gift-Check": "1", "Content-Type": "application/json" }, body: await f.text() });
    let d;
    try { d = await r.json(); } catch { throw new Error(`The gift-check server replied ${r.status}.`); }
    if (!r.ok) throw new Error(d.error || `Error ${r.status}`);
    const s = d.imported;
    out.replaceChildren(
      el("p", { class: "ok-line" }, `${f.name}: ${s.products} products stored for ${storeName(s.store)} (capture #${s.capture_id}, read ${String(d.captured_at).slice(0, 10)}), ${s.discontinued} marked gone${s.merged ? `, ${s.merged} duplicates merged` : ""}.`),
      s.warnings.length ? el("ul", { class: "small-text" }, s.warnings.map((w) => el("li", {}, w))) : null);
    $("#imp-file").value = "";
    await loadState();
    search();
  } catch (err) {
    out.replaceChildren(el("p", { class: "bad" }, `${f.name} was refused: ${err.message}`));
  }
}

/* ---------- Start ---------- */
document.addEventListener("DOMContentLoaded", async () => {
  setupTabs();
  try {
    await loadState();
  } catch (e) {
    banner(`Can't reach the gift-check server: ${e.message}`);
    return;
  }
  for (const id of ["#f-store", "#f-kind", "#f-metal", "#f-sort", "#f-sale", "#f-stock"]) $(id).addEventListener("change", () => search());
  const typed = debounce(() => search(), 300);
  for (const id of ["#f-q", "#f-min", "#f-max"]) $(id).addEventListener("input", typed);
  $("#filters").addEventListener("submit", (e) => { e.preventDefault(); search(); });
  $("#more").addEventListener("click", () => search(true));
  $("#b-base-q").addEventListener("input", searchBases);
  $("#b-addon-q").addEventListener("input", searchAddons);
  $("#b-store").addEventListener("change", () => { searchBases(); searchAddons(); });
  $("#b-recipient").addEventListener("change", () => {
    const r = recipientObj();
    if (r?.budget != null && $("#b-budget").value === "") $("#b-budget").value = r.budget;
    evaluate(); searchBases(); searchAddons();
  });
  $("#b-budget").addEventListener("input", debounce(evaluate, 300));
  $("#b-suggest").addEventListener("click", suggest);
  $("#p-save").addEventListener("click", savePlan);
  $("#p-clear").addEventListener("click", () => { PLAN = { id: null, items: [] }; $("#p-name").value = ""; evaluate().then(searchAddons); });
  $("#c-by").addEventListener("change", loadCompare);
  $("#c-a").addEventListener("change", renderCompare);
  $("#c-b").addEventListener("change", renderCompare);
  $("#r-form").addEventListener("submit", saveRecipient);
  $("#r-new").addEventListener("click", () => editRecipient(null));
  $("#imp-form").addEventListener("submit", importFile);
  $("#d-close").addEventListener("click", () => $("#detail").close());
  $("#detail").addEventListener("click", (e) => { if (e.target === $("#detail")) $("#detail").close(); });
  renderPlan();
  editRecipient(null);
  showTab(location.hash.slice(1));
  search();
  if (STATE.fetch.running) pollFetch();
});
