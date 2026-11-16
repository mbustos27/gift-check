// gift-check capture script for us.pandora.net (capture schema 1).
//
// Run it in a us.pandora.net tab (Claude's browser pane runs it with javascript_tool; a person can
// paste it into DevTools). Pandora answers plain HTTP clients with a Cloudflare check, so the catalog
// is read the way a shopper sees it: each listing page opens in a same-origin frame, the store's own
// scripts load the current (sale) prices, and the script reads
//   - the tiles the page actually draws (id, name, link, shown prices, labels), and
//   - the page's structured product data (metal, category, collection, themes, regular price, stock).
// The two lists differ for the same page number (the page re-sorts after loading), so both are kept and
// merged by product id.
//
// Manners:
//   - /robots.txt is read once per run and obeyed for user-agent `*` (RFC 9309); a listing page it
//     disallows is skipped, and that listing never counts as complete. If robots.txt can't be read
//     (5xx, network error, a 401/403/429 or a web page instead of a robots file) nothing is read.
//   - One page at a time, a pause between pages, in a frame you can see: a panel in the bottom-right
//     corner shows the page being read ("gift-check is reading Pandora listings — page N").
//   - The whole run stops at the FIRST page that isn't a normal listing: an HTTP status other than
//     200 (403, 429, ...), a browser check, a page without the listing data or without product tiles,
//     or a frame that can't be read. It never tries to get past a check, and never retries a block.
//
// Results stay in this origin's IndexedDB ("gift-check-pandora") until they are sent. Every record
// carries the id of the run that read it; capture()/send()/download() use only the latest run's
// records (or the run named with {run: "<id>"}). Records from before run ids existed belong to the run
// "legacy-2026-11-15".
//   gcPandora.both()               the usual way: start(), then recheck()
//   gcPandora.start()              resume the latest run (if it started under 6 hours ago and wasn't
//                                  sent yet), else start a new one; start({fresh: true}) always starts anew
//   gcPandora.recheck()            re-read the run's first pages and pages with a low sale share,
//                                  waiting longer for sale prices (run after start() has finished)
//   gcPandora.stop()               stop after the current page
//   gcPandora.state                progress
//   await gcPandora.stats()        coverage counts of the latest run
//   await gcPandora.capture()      the schema-1 capture object of the latest run
//   await gcPandora.send()         post it to the bridge in acknowledged chunks (this tab must have been
//                                  opened by the bridge, so window.opener is the bridge); a run is sent
//                                  once, send({again: true}) sends it again
//   await gcPandora.download()     save it as gift-check-pandora-<date>.json, for `gift-check import`
//                                  or the window's Captures tab (use this where window.opener is
//                                  missing, e.g. Claude's built-in browser opens the store in the same tab)
//
// The IndexedDB data is per site, so a run started in any us.pandora.net tab can be sent from the
// tab the bridge opens: load this script there and call send().
(() => {
  'use strict';
  const CATS = [
    ['charms', '/en/charms/'],
    ['bracelets', '/en/bracelets/'],
    ['necklaces', '/en/necklaces/'],
    ['rings', '/en/rings/'],
    ['earrings', '/en/earrings/'],
    ['gift-sets', '/en/gifts/featured-gifting/gift-sets/'],
  ];
  const PER_PAGE = 28;
  const PAUSE_MS = 2500;
  const FRESH_MS = 6 * 3600 * 1000;
  const DB_NAME = 'gift-check-pandora';
  const LEGACY_RUN = 'legacy-2026-11-15';
  const DEFAULT_BRIDGE = 'http://127.0.0.1:8781';
  const OPENER_MISSING = "window.opener is missing: this tab wasn't opened by the gift-check bridge, or the " +
    "browser opened the store in the same tab (Claude's built-in browser does that). Use " +
    'gcPandora.download() and import the file in the gift-check window (Captures tab, Import a capture ' +
    'file) or with `gift-check import <file>`.';
  // What the gift-check server accepts (docs/CAPTURE.md); longer values are dropped before sending.
  const LIMITS = { name: 300, category: 64, listing: 64, collection: 120, metal: 100, metal_group: 64,
    badges: [12, 40], themes: [30, 60] };

  // ------------------------------------------------------------------ pure helpers (tested in node)

  const collapse = (s) => String(s == null ? '' : s).replace(/\s+/g, ' ').trim();
  const codePoints = (s) => [...s];

  /** Dollar amounts in a text, in order. An amount followed by "VALUE" ("$812 VALUE") is not a price. */
  function amounts(s) {
    const text = String(s || '');
    const out = [];
    const re = /\$([\d,]+(?:\.\d\d)?)/g;
    let m;
    while ((m = re.exec(text))) {
      if (/^\s*value\b/i.test(text.slice(m.index + m[0].length))) continue;
      const n = Number(m[1].replace(/,/g, ''));
      if (Number.isFinite(n)) out.push(n);
    }
    return out;
  }

  /** An absolute URL for a path (or URL) on `origin`. */
  const absUrl = (path, origin) => new URL(path, origin).href;

  /** The first non-empty string among the values (arrays: their first non-empty string). */
  function firstText(...vals) {
    for (const v of vals) {
      const s = Array.isArray(v) ? v.find((x) => typeof x === 'string' && x.trim()) : v;
      if (typeof s === 'string' && s.trim()) return s.trim();
    }
    return null;
  }

  /**
   * One drawn tile as stored. Prices come from the tile's price elements when the page marks them
   * (`t.prices`), else from the text after the name. Labels before the name can hold amounts such as
   * "$812 VALUE", and no amount from a label containing VALUE is ever taken.
   */
  function tileRecord(t, listing, run) {
    const name = collapse(t.name) || null;
    const text = collapse(t.text);
    const labels = (t.labels || []).map(collapse).filter(Boolean);
    let amts = amounts((t.prices || []).map(collapse).filter((p) => !/value/i.test(p)).join(' '));
    if (!amts.length) {
      const i = name ? text.indexOf(name) : -1;
      let region = i >= 0 ? text.slice(i + name.length) : text;
      for (const l of labels) if (/value/i.test(l)) region = region.split(l).join(' ');
      amts = amounts(region);
    }
    const onSale = /Save \d+%/i.test(text);
    return {
      id: t.id,
      name,
      path: t.href || null,
      listing,
      run,
      labels,
      price: amts.length ? amts[0] : null,
      shown_list_price: onSale && amts.length > 1 ? amts[1] : null,
      on_sale: onSale,
      seen_at: new Date().toISOString(),
    };
  }

  /** One hit of the page's structured product data as stored. */
  function ssrRecord(hit, listing, run) {
    const x = hit.c_extend || {};
    const c = x.custom || {};
    return {
      id: hit.productId,
      name: hit.productName,
      path: x.productUrl ? String(x.productUrl).split('?')[0] : null,
      category: c.primaryCategory || null,
      collection: firstText(c.collection, c.collectionName, x.collection),
      listing,
      run,
      metal: (c.metalName || [])[0] || null,
      metal_group: c.metalGroupId || null,
      themes: c.themes || [],
      badge: c.badge || '',
      engravable: !!x.isEngravable,
      in_stock: typeof x.productInStock === 'boolean' ? x.productInStock : null,
      currency: hit.currency || 'USD',
      list_price: typeof hit.price === 'number' ? hit.price : null,
      image: (hit.primaryImage && hit.primaryImage.disBaseLink) || (hit.image && hit.image.disBaseLink) || null,
    };
  }

  /**
   * Is this a normal listing page? `tiles` is left out for the first look (before the tiles had time
   * to appear). `status` is the frame's navigation responseStatus: a number other than 200 stops the
   * run; undefined or 0 means the browser didn't say.
   */
  function pageVerdict({ readable, title, hasData, tiles, status }) {
    if (!readable) return { ok: false, why: 'the page could not be read (the frame is not readable)' };
    if (typeof status === 'number' && status !== 0 && status !== 200) {
      const what = status === 403 ? ' (blocked)' : status === 429 ? ' (too many requests)' : '';
      return { ok: false, why: 'the page answered HTTP ' + status + what };
    }
    if (/just a moment|attention required|verify you are human|access denied|are you a robot/i.test(title || '')) {
      return { ok: false, why: 'Pandora showed a browser check' };
    }
    if (!hasData) return { ok: false, why: 'the page has no listing data (not a normal listing page)' };
    if (tiles !== undefined && !(tiles > 0)) return { ok: false, why: 'the page shows no product tiles' };
    return { ok: true };
  }

  // robots.txt, RFC 9309, for user-agent `*` (a browser has no product token of its own).

  /** Upper-case percent escapes; non-ASCII becomes percent-encoded UTF-8. */
  function robotsNormalize(s) {
    let out = '';
    const cps = codePoints(String(s));
    for (let i = 0; i < cps.length; i++) {
      const ch = cps[i];
      if (ch === '%' && /^[0-9a-f]$/i.test(cps[i + 1] || '') && /^[0-9a-f]$/i.test(cps[i + 2] || '')) {
        out += '%' + cps[i + 1].toUpperCase() + cps[i + 2].toUpperCase();
        i += 2;
      } else if (ch.codePointAt(0) < 0x80) {
        out += ch;
      } else {
        try { out += encodeURIComponent(ch); } catch { /* a lone surrogate */ }
      }
    }
    return out;
  }

  /** `*` matches any run of characters; a final `$` anchors the end; otherwise a prefix match. */
  function robotsMatch(pattern, path) {
    let pat = pattern;
    let anchored = false;
    if (pat.endsWith('$')) { pat = pat.slice(0, -1); anchored = true; }
    const pieces = pat.split('*');
    let pos = 0;
    for (let i = 0; i < pieces.length; i++) {
      const piece = pieces[i];
      if (i === 0) {
        if (!path.startsWith(piece)) return false;
        pos = piece.length;
        continue;
      }
      if (i === pieces.length - 1 && anchored) return path.length >= pos + piece.length && path.endsWith(piece);
      const at = path.indexOf(piece, pos);
      if (at < 0) return false;
      pos = at + piece.length;
    }
    return !anchored || pos === path.length;
  }

  /**
   * The rules for user-agent `*`: consecutive user-agent lines (blank lines and comments between them
   * don't break the run) form one group; every group naming `*` is combined. A leading BOM is ignored
   * and lines end in \r\n, \r or \n. Only the first 500 KiB are read.
   */
  function parseRobots(text) {
    let s = String(text || '');
    if (s.charCodeAt(0) === 0xfeff) s = s.slice(1);
    s = s.slice(0, 500 * 1024);
    const groups = [];
    let cur = null;
    let collecting = false;
    for (const raw of s.split(/\r\n|\r|\n/)) {
      const line = raw.split('#')[0].trim();
      const colon = line.indexOf(':');
      if (colon < 0) continue;
      const key = line.slice(0, colon).trim().toLowerCase();
      const value = line.slice(colon + 1).trim();
      if (key === 'user-agent' || key === 'useragent' || key === 'user agent') {
        if (!collecting) {
          if (cur) groups.push(cur);
          cur = { agents: [], rules: [] };
          collecting = true;
        }
        cur.agents.push(value.toLowerCase());
      } else if (key === 'allow' || key === 'disallow') {
        collecting = false;
        // Rules before any user-agent line belong to no group; an empty value disallows nothing.
        if (cur && value) cur.rules.push({ allow: key === 'allow', pattern: robotsNormalize(value) });
      } else if (key === 'crawl-delay') {
        collecting = false;
      }
    }
    if (cur) groups.push(cur);
    const star = groups.filter((g) => g.agents.includes('*'));
    if (!star.length) return { matched: 'none', rules: [] };
    return { matched: '*', rules: star.flatMap((g) => g.rules) };
  }

  /** May this path (with its query string) be loaded? Longest match wins; Allow wins a tie. */
  function robotsAllowed(robots, pathAndQuery) {
    const path = robotsNormalize(pathAndQuery);
    if (path === '/robots.txt') return true;
    let best = null;
    for (const r of robots.rules) {
      if (!robotsMatch(r.pattern, path)) continue;
      const len = r.pattern.length;
      if (!best || len > best.len || (len === best.len && r.allow)) best = { len, allow: r.allow };
    }
    return !best || best.allow;
  }

  /** What a robots.txt answer means for this run: { ok, robots } or { ok: false, why }. */
  function robotsDecision(status, contentType, body) {
    if (status >= 200 && status < 300) {
      if (/text\/html/i.test(contentType || '') || /^\s*<(!doctype|html|head|body)\b/i.test(body || '')) {
        return { ok: false, why: 'robots.txt came back as a web page, not a robots file (a browser check?)' };
      }
      return { ok: true, robots: parseRobots(body) };
    }
    if (status === 401 || status === 403 || status === 429) {
      return { ok: false, why: 'robots.txt answered HTTP ' + status + ' (blocked or rate-limited)' };
    }
    if (status >= 400 && status < 500) return { ok: true, robots: { matched: 'none', rules: [] } };
    return { ok: false, why: 'robots.txt could not be read (HTTP ' + status + '); without it nothing may be read (RFC 9309)' };
  }

  /** The bridge's exact origin, `http://127.0.0.1:<port>`; anything else (including "*") is refused. */
  function bridgeOrigin(s) {
    const m = typeof s === 'string' ? /^http:\/\/127\.0\.0\.1:([1-9]\d{0,4})$/.exec(s) : null;
    if (!m || Number(m[1]) > 65535) {
      throw new Error('the bridge must be exactly http://127.0.0.1:<port> (like ' + DEFAULT_BRIDGE + '), not ' + JSON.stringify(s));
    }
    return s;
  }

  /** A product cut to what the server accepts: over-long optional text is dropped, lists are limited. */
  function clampProduct(p) {
    const out = { ...p };
    out.name = codePoints(collapse(p.name)).slice(0, LIMITS.name).join('');
    for (const k of ['category', 'listing', 'collection', 'metal', 'metal_group']) {
      if (out[k] == null) continue;
      const v = collapse(out[k]);
      out[k] = v && codePoints(v).length <= LIMITS[k] ? v : null;
    }
    for (const k of ['badges', 'themes']) {
      if (!Array.isArray(out[k])) continue;
      const [maxItems, maxLen] = LIMITS[k];
      const seen = new Set();
      out[k] = out[k].map(collapse).filter((v) => {
        const key = v.toLowerCase();
        if (!v || codePoints(v).length > maxLen || seen.has(key)) return false;
        seen.add(key);
        return true;
      }).slice(0, maxItems);
    }
    for (const k of ['price', 'list_price']) {
      if (out[k] != null && !(Number.isFinite(out[k]) && out[k] >= 0 && out[k] <= 1e6)) out[k] = null;
    }
    return out;
  }

  /** The run a record belongs to; records from before run ids existed belong to the legacy run. */
  const runOf = (r) => (r && r.run) || LEGACY_RUN;

  /** Category and page number of a page record (new: fields; legacy: "page:<cat>:<n>"). */
  function pageInfo(m) {
    if (m.cat) return { cat: m.cat, page: m.page };
    const [, cat, page] = m.k.split(':');
    return { cat, page: Number(page) };
  }

  /** The latest run: the newest run record, else the legacy run when legacy records exist. */
  function pickRun(runMetas, hasLegacy) {
    const sorted = runMetas.slice().sort((a, b) => String(b.started_at || '').localeCompare(String(a.started_at || '')));
    if (sorted.length) return sorted[0].id;
    return hasLegacy ? LEGACY_RUN : null;
  }

  /**
   * Listings read completely in a run: the distinct ids read from the listing reach its reported total,
   * and none of its pages was skipped (robots.txt) or failed (`bad`).
   */
  function completeListings(cats, pages, records, bad) {
    const out = [];
    for (const cat of cats) {
      if ((bad || []).includes(cat)) continue;
      const mine = pages.filter((m) => pageInfo(m).cat === cat);
      if (mine.some((m) => m.skipped)) continue;
      const total = Math.max(0, ...mine.map((m) => Number(m.total) || 0));
      if (!total) continue;
      const ids = new Set();
      for (const m of mine) for (const id of m.ids || []) ids.add(id);
      for (const r of records) if (r.listing === cat && r.id) ids.add(r.id);
      if (ids.size >= total) out.push(cat);
    }
    return out;
  }

  /**
   * The schema-1 capture of one run's records. `pages` are the run's page records; `bad` the listings
   * with a skipped or failed page. Products whose link isn't on `origin` are left out.
   */
  function buildCapture({ ssr, tiles, pages, origin, bad = [], cats = CATS.map(([c]) => c) }) {
    const byId = new Map();
    for (const s of ssr) byId.set(s.id, { s });
    for (const t of tiles) byId.set(t.id, { ...(byId.get(t.id) || {}), t });
    const products = [];
    let dropped = 0;
    for (const [id, { s, t }] of byId) {
      const path = (t && t.path) || (s && s.path);
      const name = (t && t.name) || (s && s.name);
      if (!path || !name) { dropped++; continue; }
      let url;
      try { url = absUrl(path, origin); } catch { dropped++; continue; }
      if (new URL(url).origin !== origin) { dropped++; continue; }
      const list = (s && s.list_price != null) ? s.list_price : (t && (t.shown_list_price ?? t.price));
      const badges = s && s.badge ? s.badge.split('|').map((x) => x.trim()).filter(Boolean) : (t ? t.labels || [] : []);
      const digit = (String(id).match(/^\d(\d)/) || [])[1];
      products.push(clampProduct({
        id,
        name,
        url,
        listing: (s && s.listing) || (t && t.listing) || null,
        category: (s && s.category) || (t && t.listing) || null,
        collection: (s && s.collection) || null,
        metal: (s && s.metal) || null,
        metal_group: (s && s.metal_group) || METAL_BY_DIGIT[digit] || null,
        themes: (s && s.themes) || [],
        badges,
        engravable: s ? s.engravable : badges.some((b) => /engrav/i.test(b)),
        in_stock: s ? s.in_stock : null,
        currency: (s && s.currency) || 'USD',
        list_price: list ?? null,
        price: t ? t.price : null,
        image: (s && s.image) || null,
      }));
    }
    const read = pages.filter((m) => !m.skipped && m.at);
    const captured = read.map((m) => m.at).sort()[0] || null;
    const complete_listings = completeListings(cats, pages, [...ssr, ...tiles], bad);
    return {
      schema: 1,
      store: 'pandora-us',
      origin,
      captured_at: captured,
      complete: cats.every((c) => complete_listings.includes(c)),
      complete_listings,
      sources: [...new Set(read.map((m) => m.url).filter(Boolean))].slice(0, 1000),
      products,
      dropped,
    };
  }

  // Metal from the style number when the structured data is missing: second digit 9 silver,
  // 6 gold-plated, 8 rose gold-plated (Pandora's numbering; see data/rules.toml in gift-check).
  const METAL_BY_DIGIT = { 9: 'Silver', 6: 'Gold', 8: 'Rose Gold' };

  const helpers = { CATS, LEGACY_RUN, amounts, absUrl, firstText, tileRecord, ssrRecord, pageVerdict,
    robotsNormalize, robotsMatch, parseRobots, robotsAllowed, robotsDecision, bridgeOrigin, clampProduct,
    runOf, pageInfo, pickRun, completeListings, buildCapture };
  if (typeof module !== 'undefined' && typeof window === 'undefined') {
    module.exports = helpers;
    return;
  }

  // ------------------------------------------------------------------ in the store tab

  if (window.gcPandora) return 'gcPandora already loaded';
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const state = { running: false, stop: false, run: null, pagesDone: 0, pagesSkipped: 0, status: 'idle',
    errors: [], skipped: [], notes: [], started: null, finished: null };
  let robotsCache = null;
  let panel = null;

  const openDb = () => new Promise((res, rej) => {
    const o = indexedDB.open(DB_NAME, 1);
    o.onupgradeneeded = () => {
      for (const [name, key] of [['tiles', 'id'], ['ssr', 'id'], ['meta', 'k']]) {
        o.result.createObjectStore(name, { keyPath: key });
      }
    };
    o.onsuccess = () => res(o.result);
    o.onerror = () => rej(o.error);
  });
  const all = (db, store) => new Promise((res, rej) => {
    const r = db.transaction(store, 'readonly').objectStore(store).getAll();
    r.onsuccess = () => res(r.result);
    r.onerror = () => rej(r.error);
  });
  const putAll = (db, store, rows) => new Promise((res, rej) => {
    const t = db.transaction(store, 'readwrite');
    for (const row of rows) t.objectStore(store).put(row);
    t.oncomplete = res;
    t.onerror = () => rej(t.error);
  });

  const isPage = (m) => m.k.startsWith('page:');
  const isRun = (m) => m.k.startsWith('run:');

  /** Everything stored, and which run is the latest (or the one asked for). */
  async function load(db, wanted) {
    const [tiles, ssr, meta] = [await all(db, 'tiles'), await all(db, 'ssr'), await all(db, 'meta')];
    const runs = meta.filter(isRun);
    const hasLegacy = [...tiles, ...ssr, ...meta.filter(isPage)].some((r) => runOf(r) === LEGACY_RUN);
    const run = wanted || pickRun(runs, hasLegacy);
    return { tiles, ssr, meta, runs, run, runMeta: runs.find((r) => r.id === run) || null };
  }

  /** Resume the latest run (under 6 hours old, not sent) or start a new one. */
  async function beginRun(db, fresh) {
    const { runs, run, runMeta } = await load(db);
    if (!fresh && runMeta && !runMeta.sent_at && Date.now() - Date.parse(runMeta.started_at) < FRESH_MS) {
      return runMeta.id;
    }
    if (run === LEGACY_RUN && !runs.some((r) => r.id === LEGACY_RUN && r.sent_at)) {
      state.notes.push('The earlier run ' + LEGACY_RUN + " wasn't sent; it can still be sent with send({run: '" +
        LEGACY_RUN + "'}) or download({run: '" + LEGACY_RUN + "'}).");
    }
    const now = new Date();
    const id = 'run-' + now.toISOString().slice(0, 10).replace(/-/g, '') + '-' + now.getTime().toString(36) +
      Math.random().toString(36).slice(2, 6);
    await putAll(db, 'meta', [{ k: 'run:' + id, id, started_at: now.toISOString(), sent_at: null, bad: [] }]);
    return id;
  }

  async function markRun(db, id, fields) {
    const meta = await all(db, 'meta');
    const old = meta.find((m) => m.k === 'run:' + id) || { k: 'run:' + id, id, started_at: null, bad: [] };
    await putAll(db, 'meta', [{ ...old, ...fields }]);
  }

  async function readRobots() {
    if (robotsCache) return robotsCache;
    let r;
    try {
      r = await fetch(absUrl('/robots.txt', location.origin), { credentials: 'same-origin', cache: 'no-store' });
    } catch (e) {
      return (robotsCache = { ok: false, why: 'robots.txt could not be read (' + e.message + ')' });
    }
    if (r.redirected && new URL(r.url).origin !== location.origin) {
      return (robotsCache = { ok: false, why: 'robots.txt redirected to another site' });
    }
    const body = r.ok ? await r.text() : '';
    return (robotsCache = robotsDecision(r.status, r.headers.get('content-type') || '', body));
  }

  function showPanel() {
    if (panel) return panel;
    const box = document.createElement('div');
    box.setAttribute('role', 'status');
    box.style.cssText = 'position:fixed;right:12px;bottom:12px;z-index:2147483647;width:480px;' +
      'max-height:calc(100vh - 24px);overflow:auto;background:#fff;color:#111;border:2px solid #444;' +
      'border-radius:6px;box-shadow:0 4px 16px rgba(0,0,0,.35);font:13px/1.4 system-ui,sans-serif;' +
      'opacity:1;pointer-events:auto';
    const head = document.createElement('div');
    head.style.cssText = 'padding:6px 10px;background:#f3f3f3;border-bottom:1px solid #ccc';
    const body = document.createElement('div');
    body.style.cssText = 'width:480px;height:640px';
    box.append(head, body);
    document.body.appendChild(box);
    panel = { box, head, body };
    setPanel('');
    return panel;
  }
  function setPanel(detail) {
    if (!panel) return;
    panel.head.textContent = 'gift-check is reading Pandora listings — page ' + (state.pagesDone + 1) +
      (detail ? ' (' + detail + ')' : '');
  }
  function hidePanel() {
    if (panel) panel.box.remove();
    panel = null;
  }

  class PageStop extends Error {}

  const PRICE_SEL = '[itemprop="price"], [data-testid*="price" i], [class*="price" i]';

  async function renderPage(path, minSettleMs = 5000) {
    const { body } = showPanel();
    const f = document.createElement('iframe');
    f.title = 'gift-check: Pandora listing';
    f.style.cssText = 'display:block;width:480px;height:640px;border:0;background:#fff';
    body.appendChild(f);
    try {
      await new Promise((r) => { f.onload = r; f.src = absUrl(path, location.origin); setTimeout(r, 30000); });
      const loadedAt = Date.now();
      await sleep(1500);
      let w = null;
      let d = null;
      try { w = f.contentWindow; d = f.contentDocument; } catch { /* not readable */ }
      const readable = !!(d && d.body);
      let status;
      try { status = (w.performance.getEntriesByType('navigation')[0] || {}).responseStatus; } catch { /* unknown */ }
      let pg = null;
      if (readable) {
        try {
          const md = d.getElementById('mobify-data');
          const qs = JSON.parse(md.textContent).__PRELOADED_STATE__.__reactQuery.queries;
          pg = qs.find((x) => JSON.stringify(x.queryKey).includes('product-search')).state.data.pages[0];
          if (!pg || !Array.isArray(pg.hits) || !(Number(pg.total) >= 0)) pg = null;
        } catch { pg = null; }
      }
      const look = { readable, title: readable ? d.title : '', hasData: !!pg, status };
      let v = pageVerdict(look);
      if (!v.ok) throw new PageStop(v.why);

      // The page's own tiles: the first PER_PAGE distinct ones (later ones belong to the next page).
      const pageTiles = () => {
        const seen = new Set();
        const out = [];
        for (const t of d.querySelectorAll('a[data-testid="product-tile"]')) {
          if (!t.dataset.pid || seen.has(t.dataset.pid)) continue;
          seen.add(t.dataset.pid);
          out.push(t);
          if (out.length === PER_PAGE) break;
        }
        return out;
      };
      for (let i = 0; i < 10 && !pageTiles().length; i++) await sleep(500);
      v = pageVerdict({ ...look, tiles: pageTiles().length });
      if (!v.ok) throw new PageStop(v.why);
      // Tiles draw their price when scrolled into view.
      for (let i = 0; i < 40; i++) {
        const tiles = pageTiles();
        const missing = tiles.filter((t) => !/\$\d/.test(t.textContent));
        if (tiles.length && !missing.length) break;
        const target = missing[0] || tiles[tiles.length - 1];
        if (target) target.scrollIntoView({ block: 'center' }); else w.scrollBy(0, 600);
        await sleep(400);
      }
      // Sale prices replace regular prices a few seconds after load (sometimes later): wait until
      // the tiles stop changing, at least minSettleMs after load and at most 10 s more.
      let prev = '';
      let stable = 0;
      while (Date.now() - loadedAt < minSettleMs + 10000) {
        const cur = pageTiles().map((t) => t.textContent).join('|');
        stable = cur === prev ? stable + 1 : 0;
        prev = cur;
        if (stable >= 2 && Date.now() - loadedAt >= minSettleMs) break;
        await sleep(700);
      }
      const tiles = pageTiles().map((t) => {
        const nameEl = t.querySelector('[itemprop="name"]');
        const els = [...t.querySelectorAll(PRICE_SEL)];
        const leaves = els.filter((e) => !els.some((o) => o !== e && e.contains(o)));
        return {
          id: t.dataset.pid,
          name: nameEl ? collapse(nameEl.textContent) : null,
          href: (t.getAttribute('href') || '').split(/[?#]/)[0],
          text: collapse(t.textContent).slice(0, 400),
          prices: leaves.map((e) => collapse(e.textContent)).filter((s) => /\$\d/.test(s) && !/value/i.test(s)),
          labels: [...t.querySelectorAll('[itemprop="label"]')].map((e) => collapse(e.textContent)).filter(Boolean),
        };
      });
      return { pg, tiles };
    } finally {
      // Unload the store's page before removing the frame: ~120 full page loads in one tab
      // crashed the renderer once (memory).
      f.src = 'about:blank';
      f.remove();
    }
  }

  function stopRun(why) {
    state.stop = true;
    state.status = 'stopped: ' + why + '. Nothing more is read in this run; open the site yourself, ' +
      'check it, and start again later.';
  }

  // A tile read before the store's sale price arrived shows the regular price. A sale seen in the
  // last 6 hours therefore wins over a later reading without one (and joins the current run);
  // otherwise the newest reading wins.
  async function storeTiles(db, shown, run) {
    const old = new Map((await all(db, 'tiles')).map((t) => [t.id, t]));
    let upgraded = 0;
    const rows = shown.map((r) => {
      const o = old.get(r.id);
      if (o && o.on_sale && !r.on_sale && Date.now() - Date.parse(o.seen_at) < FRESH_MS) return { ...o, run };
      if (o && !o.on_sale && r.on_sale) upgraded++;
      return r;
    });
    await putAll(db, 'tiles', rows);
    return upgraded;
  }

  async function run(opts = {}) {
    const db = await openDb();
    const runId = await beginRun(db, !!opts.fresh);
    state.run = runId;
    const robots = await readRobots();
    if (!robots.ok) { stopRun(robots.why); return; }
    showPanel();
    const { meta } = await load(db, runId);
    const mine = meta.filter((m) => isPage(m) && runOf(m) === runId);
    const bad = new Set((meta.find((m) => m.k === 'run:' + runId) || {}).bad || []);
    try {
      for (const [cat, path] of CATS) {
        let page = 1;
        let pages = 1;
        while (page <= pages && !state.stop) {
          const key = 'page:' + runId + ':' + cat + ':' + page;
          const known = mine.find((m) => m.k === key);
          if (known && known.total) pages = Math.ceil(known.total / PER_PAGE);
          if (known) { state.pagesSkipped++; page++; continue; }
          const url = path + '?search_engine=bloomreach&site=us&page=' + page;
          if (!robotsAllowed(robots.robots, url)) {
            state.skipped.push(url);
            bad.add(cat);
            await putAll(db, 'meta', [{ k: key, run: runId, cat, page, skipped: true, url: absUrl(url, location.origin) }]);
            page++;
            continue;
          }
          state.status = cat + ' page ' + page + '/' + pages;
          setPanel(cat + ' ' + page + '/' + pages);
          let got;
          try {
            got = await renderPage(url);
          } catch (e) {
            bad.add(cat);
            state.errors.push(cat + ' p' + page + ': ' + e.message);
            stopRun(e instanceof PageStop ? cat + ' page ' + page + ': ' + e.message : e.message);
            return;
          }
          const { pg, tiles } = got;
          pages = Math.ceil(pg.total / PER_PAGE);
          const ssr = pg.hits.filter((h) => h.productId).map((h) => ssrRecord(h, cat, runId));
          const shown = tiles.map((t) => tileRecord(t, cat, runId));
          await putAll(db, 'ssr', ssr);
          await storeTiles(db, shown, runId);
          await putAll(db, 'meta', [{ k: key, run: runId, cat, page, at: new Date().toISOString(),
            tiles: shown.length, priced: shown.filter((r) => r.price != null).length,
            sale: shown.filter((r) => r.on_sale).length, ssr: ssr.length, total: pg.total,
            ids: [...new Set([...ssr.map((r) => r.id), ...shown.map((r) => r.id)])],
            url: absUrl(url, location.origin) }]);
          state.pagesDone++;
          page++;
          await sleep(PAUSE_MS);
        }
      }
    } finally {
      await markRun(db, runId, { bad: [...bad] });
    }
  }

  // Re-read the run's first pages and pages whose sale share looks low, with a longer wait.
  async function recheck({ minSettleMs = 9000, maxSaleShare = 0.85, run: wanted } = {}) {
    const db = await openDb();
    const { meta, run: runId } = await load(db, wanted || state.run);
    if (!runId) { state.status = 'recheck: nothing read yet'; return; }
    state.run = runId;
    const robots = await readRobots();
    if (!robots.ok) { stopRun(robots.why); return; }
    const pages = meta.filter((m) => isPage(m) && runOf(m) === runId && !m.skipped && !m.rechecked_at &&
      (pageInfo(m).page === 1 || m.sale / Math.max(1, m.tiles) < maxSaleShare));
    state.recheck = { planned: pages.length, done: 0, upgraded: 0 };
    for (const m of pages) {
      if (state.stop) break;
      const { cat, page } = pageInfo(m);
      const u = new URL(m.url, location.origin);
      const path = u.pathname + u.search;
      if (!robotsAllowed(robots.robots, path)) { state.skipped.push(path); state.recheck.done++; continue; }
      state.status = 'recheck ' + cat + ' ' + page + ' (' + (state.recheck.done + 1) + '/' + pages.length + ')';
      setPanel('recheck ' + cat + ' ' + page);
      try {
        const { tiles } = await renderPage(path, minSettleMs);
        const shown = tiles.map((t) => tileRecord(t, cat, runId));
        state.recheck.upgraded += await storeTiles(db, shown, runId);
        await putAll(db, 'meta', [{ ...m, run: runId, rechecked_at: new Date().toISOString(),
          recheck_sale: shown.filter((r) => r.on_sale).length }]);
        state.pagesDone++;
      } catch (e) {
        state.errors.push('recheck ' + cat + ' p' + page + ': ' + e.message);
        stopRun(e instanceof PageStop ? 'recheck of ' + cat + ' page ' + page + ': ' + e.message : e.message);
        return;
      }
      state.recheck.done++;
      await sleep(PAUSE_MS);
    }
  }

  /** The capture of the latest run (or `opts.run`), with what it was built from. */
  async function merged(opts = {}) {
    const db = await openDb();
    const { tiles, ssr, meta, run: runId, runMeta } = await load(db, opts.run);
    const pick = (r) => runOf(r) === runId;
    const pages = meta.filter((m) => isPage(m) && pick(m));
    const cap = buildCapture({ ssr: ssr.filter(pick), tiles: tiles.filter(pick), pages,
      origin: location.origin, bad: (runMeta && runMeta.bad) || [] });
    if (!cap.captured_at) cap.captured_at = new Date().toISOString();
    return { db, run: runId, runMeta, pages, cap };
  }

  async function stats(opts) {
    const { run: runId, runMeta, pages, cap } = await merged(opts);
    const totals = {};
    for (const m of pages) if (m.total) totals[pageInfo(m).cat] = m.total;
    return {
      run: runId,
      sent_at: runMeta ? runMeta.sent_at || null : null,
      captured_at: cap.captured_at,
      products: cap.products.length,
      withShownPrice: cap.products.filter((p) => p.price != null).length,
      onSale: cap.products.filter((p) => p.price != null && p.list_price != null && p.price < p.list_price).length,
      complete: cap.complete,
      complete_listings: cap.complete_listings,
      totals,
      pagesDone: pages.filter((m) => !m.skipped).length,
      pagesSkippedByRobots: pages.filter((m) => m.skipped).length,
      dropped: cap.dropped,
    };
  }

  async function capture(opts) {
    const { cap } = await merged(opts);
    const { dropped, ...doc } = cap;
    return doc;
  }

  // Bridge protocol: capture/README.md. Chunks are numbered 1..chunks; each waits for its ack.
  function waitAck(bridge, id, chunk) {
    return new Promise((resolve, reject) => {
      const t = setTimeout(() => { removeEventListener('message', on); reject(new Error('no answer from the bridge')); }, 30000);
      function on(e) {
        if (e.origin !== bridge || e.source !== window.opener) return;
        const d = e.data;
        if (!d || d.type !== 'gift-check-ack' || d.capture_id !== id || d.chunk !== chunk) return;
        clearTimeout(t);
        removeEventListener('message', on);
        resolve(d);
      }
      addEventListener('message', on);
    });
  }

  async function send(opts = {}, chunkSizeArg) {
    if (typeof opts === 'string') opts = { bridge: opts, chunkSize: chunkSizeArg };
    const bridge = bridgeOrigin(opts.bridge || DEFAULT_BRIDGE);
    if (!window.opener) throw new Error(OPENER_MISSING);
    const { db, run: runId, runMeta, cap } = await merged(opts);
    if (!runId) throw new Error('nothing to send: no run has been read in this browser');
    if (runMeta && runMeta.sent_at && !opts.again) {
      throw new Error('run ' + runId + ' was already sent (' + runMeta.sent_at + '); send({again: true}) sends it again');
    }
    const { dropped, products, ...header } = cap;
    const chunkSize = Math.min(5000, Math.max(1, Number(opts.chunkSize) || 1000));
    const id = 'pandora-' + Date.now().toString(36) + '-' + Math.random().toString(36).slice(2, 8);
    const chunks = Math.max(1, Math.ceil(products.length / chunkSize));
    let last = null;
    for (let i = 1; i <= chunks; i++) {
      const msg = { type: 'gift-check-capture', capture_id: id, chunk: i, chunks, ...header,
        products: products.slice((i - 1) * chunkSize, i * chunkSize) };
      last = null;
      for (let attempt = 1; attempt <= 3 && !last; attempt++) {
        if (!window.opener) throw new Error(OPENER_MISSING);
        const wait = waitAck(bridge, id, i);
        window.opener.postMessage(msg, bridge);
        try { last = await wait; } catch (e) { if (attempt === 3) throw e; }
      }
      if (!last.ok) throw new Error('chunk ' + i + ' refused: ' + last.error);
    }
    await markRun(db, runId, { sent_at: new Date().toISOString(),
      started_at: (runMeta && runMeta.started_at) || header.captured_at });
    return { run: runId, capture_id: id, chunks, products: products.length, complete: header.complete,
      complete_listings: header.complete_listings, dropped, last };
  }

  /** Saves the run's capture as a file for `gift-check import` or the window's Captures tab. */
  async function download(opts = {}) {
    const { db, run: runId, runMeta, cap } = await merged(opts);
    if (!runId) throw new Error('nothing to save: no run has been read in this browser');
    const { dropped, ...doc } = cap;
    const text = JSON.stringify(doc);
    const blob = new Blob([text], { type: 'application/json' });
    const file = 'gift-check-pandora-' + doc.captured_at.slice(0, 10) + '.json';
    const a = document.createElement('a');
    a.href = URL.createObjectURL(blob);
    a.download = file;
    document.body.appendChild(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(a.href), 60000);
    if (!runMeta || !runMeta.sent_at) {
      await markRun(db, runId, { sent_at: new Date().toISOString(),
        started_at: (runMeta && runMeta.started_at) || doc.captured_at });
    }
    return { run: runId, products: doc.products.length, bytes: blob.size, file, dropped };
  }

  function launch(fn, label) {
    if (state.running) return 'already running';
    robotsCache = null;
    Object.assign(state, { running: true, stop: false, errors: [], skipped: [], notes: [], pagesDone: 0,
      pagesSkipped: 0, status: label + ' running', started: new Date().toISOString(), finished: null });
    fn().catch((e) => {
      state.errors.push(label + ': ' + e.message);
      stopRun(e.message);
    }).finally(() => {
      hidePanel();
      state.running = false;
      state.finished = new Date().toISOString();
      if (!state.status.startsWith('stopped')) state.status = state.stop ? label + ' stopped' : label + ' done';
    });
    return label + ' started';
  }

  window.gcPandora = {
    state,
    start(opts) { return launch(() => run(opts), 'run'); },
    recheck(opts) { return launch(() => recheck(opts), 'recheck'); },
    // The usual way: read every page, then re-read the doubtful ones.
    both(opts) { return launch(async () => { await run(opts); if (!state.stop) await recheck({ ...opts, run: state.run }); }, 'run+recheck'); },
    stop() { state.stop = true; return 'stopping after the current page'; },
    stats,
    capture,
    send,
    download,
  };
  return 'gcPandora loaded';
})();
