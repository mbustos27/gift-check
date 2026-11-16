// Unit tests for the pure helpers of capture/pandora.js. Run: node --test tests/js/
'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const gc = require('../../capture/pandora.js');

const ORIGIN = 'https://us.pandora.net';

test('tileRecord: a "$812 VALUE" label before a multi-space name is not the price', () => {
  const t = {
    id: 'B802000',
    name: '  Holiday   Gift\n Set ',
    href: '/en/gift-sets/B802000.html',
    text: 'NEW $812 VALUE Holiday Gift Set $399.00',
    labels: ['NEW', '$812 VALUE'],
  };
  const r = gc.tileRecord(t, 'gift-sets', 'run-1');
  assert.equal(r.name, 'Holiday Gift Set');
  assert.equal(r.price, 399);
  assert.equal(r.listing, 'gift-sets');
  assert.equal(r.run, 'run-1');
});

test('tileRecord: an amount followed by VALUE after the name is skipped too', () => {
  const t = { id: 'B802001', name: 'Charm Gift Set', text: 'Charm Gift Set $812 VALUE $399.00', labels: [] };
  assert.equal(gc.tileRecord(t, 'gift-sets', 'r').price, 399);
  // A VALUE label is removed from the text even when it isn't followed by the word in the text.
  const u = { id: 'B802002', name: 'Charm Gift Set', text: 'Charm Gift Set worth $812 value-pack $99.00', labels: ['worth $812 value-pack'] };
  assert.equal(gc.tileRecord(u, 'gift-sets', 'r').price, 99);
});

test('tileRecord prefers the tile price elements and reads sale + regular price', () => {
  const t = {
    id: '790001', name: 'Lucky Cat Charm',
    text: 'Lucky Cat Charm free engraving worth $20.00 Save 30% $68.00 $98.00',
    prices: ['$68.00', '$98.00', '$812 VALUE'],
    labels: [],
  };
  const r = gc.tileRecord(t, 'charms', 'r');
  assert.equal(r.price, 68);
  assert.equal(r.shown_list_price, 98);
  assert.equal(r.on_sale, true);
  // Price elements without an amount fall back to the text after the name.
  const u = { ...t, prices: [''], text: 'Lucky Cat Charm $50.00' };
  assert.equal(gc.tileRecord(u, 'charms', 'r').price, 50);
});

test('robots: a leading BOM does not hide the first user-agent line', () => {
  const r = gc.parseRobots('﻿User-agent: *\nDisallow: /\n');
  assert.equal(r.matched, '*');
  assert.equal(gc.robotsAllowed(r, '/en/charms/?page=1'), false);
});

test('robots: CR-only and CRLF line endings', () => {
  const cr = gc.parseRobots('User-agent: *\rDisallow: /en/charms/\rAllow: /en/charms/x\r');
  assert.equal(gc.robotsAllowed(cr, '/en/charms/?page=1'), false);
  assert.equal(gc.robotsAllowed(cr, '/en/charms/x'), true);
  assert.equal(gc.robotsAllowed(cr, '/en/rings/'), true);
  const crlf = gc.parseRobots('User-agent: *\r\nDisallow: /en/rings/\r\n');
  assert.equal(gc.robotsAllowed(crlf, '/en/rings/?page=2'), false);
});

test('robots: consecutive user-agent lines form one group; star groups combine', () => {
  const txt = 'User-agent: googlebot\n# note\n\nUser-agent: *\nDisallow: /private\n\nUser-agent: bingbot\nDisallow: /\n\nUser-agent: *\nDisallow: /cart\n';
  const r = gc.parseRobots(txt);
  assert.equal(r.matched, '*');
  assert.equal(gc.robotsAllowed(r, '/private/x'), false, 'from the group shared with googlebot');
  assert.equal(gc.robotsAllowed(r, '/cart'), false, 'from the second * group');
  assert.equal(gc.robotsAllowed(r, '/en/charms/'), true, "bingbot's Disallow: / is not ours");
  // No group for * at all: everything allowed. Rules before any user-agent belong to nobody.
  assert.equal(gc.robotsAllowed(gc.parseRobots('User-agent: googlebot\nDisallow: /\n'), '/x'), true);
  assert.equal(gc.robotsAllowed(gc.parseRobots('Disallow: /\n'), '/x'), true);
});

test('robots: longest match wins, Allow wins a tie, wildcards, end anchor and queries', () => {
  const r = gc.parseRobots('User-agent: *\nAllow: /p\nDisallow: /\n');
  assert.equal(gc.robotsAllowed(r, '/page'), true);
  assert.equal(gc.robotsAllowed(r, '/x'), false);
  const tie = gc.parseRobots('User-agent: *\nDisallow: /folder\nAllow: /folder\n');
  assert.equal(gc.robotsAllowed(tie, '/folder/page'), true);
  const tie2 = gc.parseRobots('User-agent: *\nAllow: /folder\nDisallow: /folder\n');
  assert.equal(gc.robotsAllowed(tie2, '/folder/page'), true, 'order does not matter');
  const w = gc.parseRobots('User-agent: *\nDisallow: /*?*page=\nAllow: /en/charms/$\nDisallow: /*.json$\n');
  assert.equal(gc.robotsAllowed(w, '/en/charms/?search_engine=bloomreach&site=us&page=2'), false);
  assert.equal(gc.robotsAllowed(w, '/en/charms/'), true);
  assert.equal(gc.robotsAllowed(w, '/data.json'), false);
  assert.equal(gc.robotsAllowed(w, '/data.json?x=1'), true, 'with a query the path no longer ends in .json');
  assert.equal(gc.robotsAllowed(w, '/robots.txt'), true);
  assert.ok(gc.robotsMatch('/a*b*c$', '/a-x-b-y-c'));
  assert.ok(!gc.robotsMatch('/a*b*c$', '/a-x-b-y-c-d'));
  const pct = gc.parseRobots('User-agent: *\nDisallow: /en/*%2b*\n');
  assert.equal(gc.robotsAllowed(pct, '/en/a%2Bb'), false, 'percent escapes compare case-insensitively');
});

test('robots answers: what stops the run', () => {
  assert.equal(gc.robotsDecision(200, 'text/plain', 'User-agent: *\nDisallow:\n').ok, true);
  assert.equal(gc.robotsDecision(200, 'text/html', '<html>Just a moment</html>').ok, false);
  assert.equal(gc.robotsDecision(200, '', '<!DOCTYPE html><html>').ok, false);
  for (const s of [401, 403, 429, 500, 503, 0]) assert.equal(gc.robotsDecision(s, '', '').ok, false, String(s));
  const none = gc.robotsDecision(404, 'text/html', '');
  assert.equal(none.ok, true);
  assert.equal(gc.robotsAllowed(none.robots, '/anything'), true, 'no robots.txt: no rules');
});

test('pageVerdict stops on the first page that is not a normal listing', () => {
  const ok = { readable: true, title: 'Charms | Pandora US', hasData: true, tiles: 28, status: 200 };
  assert.deepEqual(gc.pageVerdict(ok), { ok: true });
  assert.equal(gc.pageVerdict({ ...ok, status: undefined }).ok, true, 'unknown status is not used');
  assert.equal(gc.pageVerdict({ ...ok, status: 0 }).ok, true);
  const no = (x) => gc.pageVerdict({ ...ok, ...x });
  assert.match(no({ status: 403 }).why, /403/);
  assert.match(no({ status: 429 }).why, /429/);
  assert.equal(no({ status: 500 }).ok, false);
  assert.equal(no({ status: 301 }).ok, false);
  assert.match(no({ hasData: false }).why, /no listing data/);
  assert.match(no({ tiles: 0 }).why, /no product tiles/);
  assert.match(no({ title: 'Just a moment...' }).why, /browser check/);
  assert.equal(no({ readable: false }).ok, false);
  assert.equal(gc.pageVerdict({ readable: true, title: 'x', hasData: true }).ok, true, 'tiles not counted yet');
});

test('bridgeOrigin accepts only http://127.0.0.1:<port>', () => {
  assert.equal(gc.bridgeOrigin('http://127.0.0.1:8781'), 'http://127.0.0.1:8781');
  assert.equal(gc.bridgeOrigin('http://127.0.0.1:1'), 'http://127.0.0.1:1');
  for (const bad of ['*', 'http://localhost:8781', 'https://127.0.0.1:8781', 'http://127.0.0.1:8781/',
    'http://127.0.0.1:8781/bridge', 'http://127.0.0.1', 'http://127.0.0.1:0', 'http://127.0.0.1:65536',
    'http://127.0.0.1:08781', 'http://127.0.0.1.evil.example:8781', 'http://evil.example', '', null, undefined]) {
    assert.throws(() => gc.bridgeOrigin(bad), /127\.0\.0\.1/, String(bad));
  }
});

test('clampProduct keeps every field inside the server limits', () => {
  const p = gc.clampProduct({
    id: '1', name: 'x'.repeat(310), url: ORIGIN + '/en/1.html',
    category: 'c'.repeat(65), listing: 'charms', collection: 'k'.repeat(121), metal: 'Sterling silver',
    metal_group: 'g'.repeat(65),
    badges: ['NEW', 'b'.repeat(41), ...Array.from({ length: 20 }, (_, i) => 'B' + i), 'new'],
    themes: ['t'.repeat(61), ...Array.from({ length: 40 }, (_, i) => 'theme ' + i)],
    price: NaN, list_price: 98,
  });
  assert.equal([...p.name].length, 300);
  assert.equal(p.category, null);
  assert.equal(p.listing, 'charms');
  assert.equal(p.collection, null);
  assert.equal(p.metal, 'Sterling silver');
  assert.equal(p.metal_group, null);
  assert.equal(p.badges.length, 12);
  assert.ok(p.badges.every((b) => b.length <= 40));
  assert.equal(p.badges[0], 'NEW');
  assert.equal(p.themes.length, 30);
  assert.ok(p.themes.every((t) => t.length <= 60));
  assert.equal(p.price, null, 'NaN is never sent');
  assert.equal(p.list_price, 98);
});

test('runs: legacy records form their own run; the newest run is the latest', () => {
  assert.equal(gc.runOf({ id: '1' }), 'legacy-2026-11-15');
  assert.equal(gc.runOf({ id: '1', run: 'run-x' }), 'run-x');
  assert.equal(gc.pickRun([], true), 'legacy-2026-11-15');
  assert.equal(gc.pickRun([], false), null);
  const runs = [{ id: 'legacy-2026-11-15', started_at: '2026-11-15T10:00:00Z' }, { id: 'run-b', started_at: '2026-11-15T18:00:00Z' }];
  assert.equal(gc.pickRun(runs, true), 'run-b');
});

test('buildCapture: listings, complete_listings, captured_at and links on the store only', () => {
  const page = (cat, n, ids, total, at) => ({ k: `page:run-1:${cat}:${n}`, run: 'run-1', cat, page: n, ids, total, at, url: `${ORIGIN}/en/${cat}/?page=${n}` });
  const pages = [
    page('charms', 1, ['a', 'b'], 3, '2026-11-15T12:05:00Z'),
    page('charms', 2, ['c'], 3, '2026-11-15T12:00:00Z'),
    page('bracelets', 1, ['d'], 2, '2026-11-15T12:10:00Z'),
  ];
  const ssr = [
    { id: 'a', name: 'A Charm', path: '/en/a.html', listing: 'charms', category: 'charms', collection: 'Pandora Moments', list_price: 50, run: 'run-1' },
    { id: 'd', name: 'D Bracelet', path: '/en/d.html', listing: 'bracelets', list_price: 80, run: 'run-1' },
    { id: 'x', name: 'Elsewhere', path: 'https://evil.example/x.html', listing: 'charms', run: 'run-1' },
  ];
  const tiles = [
    { id: 'b', name: 'B Charm', path: '/en/b.html', listing: 'charms', price: 40, labels: [], run: 'run-1' },
    { id: 'c', name: 'C Charm', path: '/en/c.html', listing: 'charms', price: 30, labels: [], run: 'run-1' },
  ];
  const cap = gc.buildCapture({ ssr, tiles, pages, origin: ORIGIN, cats: ['charms', 'bracelets'] });
  assert.deepEqual(cap.complete_listings, ['charms'], 'bracelets: 1 of 2 read');
  assert.equal(cap.complete, false);
  assert.equal(cap.captured_at, '2026-11-15T12:00:00Z', 'the earliest page read');
  assert.equal(cap.dropped, 1);
  assert.ok(cap.products.every((p) => p.url.startsWith(ORIGIN + '/')));
  const a = cap.products.find((p) => p.id === 'a');
  assert.equal(a.listing, 'charms');
  assert.equal(a.collection, 'Pandora Moments');
  // A listing with a page skipped by robots.txt (or a failed one) is never complete.
  const skipped = [...pages, { k: 'page:run-1:charms:3', run: 'run-1', cat: 'charms', page: 3, skipped: true }];
  assert.deepEqual(gc.buildCapture({ ssr, tiles, pages: skipped, origin: ORIGIN, cats: ['charms'] }).complete_listings, []);
  assert.deepEqual(gc.buildCapture({ ssr, tiles, pages, origin: ORIGIN, cats: ['charms'], bad: ['charms'] }).complete_listings, []);
  // Legacy page records (no ids) count the run's records read from the listing.
  const legacy = [{ k: 'page:charms:1', total: 3, at: '2026-11-15T09:00:00Z' }];
  assert.deepEqual(gc.buildCapture({ ssr, tiles, pages: legacy, origin: ORIGIN, cats: ['charms'] }).complete_listings, ['charms']);
});

test('ssrRecord reads a collection from the structured data when there is one', () => {
  const hit = { productId: '790001', productName: 'Cat Charm', price: 50, c_extend: { productUrl: '/en/c.html?x=1', custom: { collectionName: ['', 'Pandora ME'] } } };
  const r = gc.ssrRecord(hit, 'charms', 'run-1');
  assert.equal(r.collection, 'Pandora ME');
  assert.equal(r.path, '/en/c.html');
  assert.equal(gc.ssrRecord({ productId: '1', c_extend: {} }, 'charms', 'r').collection, null);
});
