// The bridge's message filter (crates/ui/web/bridge.js), exercised with fake windows and events.
"use strict";
const test = require("node:test");
const assert = require("node:assert/strict");
const { classify, nonFinite } = require("../../crates/ui/web/bridge.js");

const storeTab = { name: "the store tab we opened" };
const otherTab = { name: "a tab we did not open" };
const opened = [{ win: storeTab, origin: "https://us.pandora.net", store: "pandora-us" }];
const capture = (extra = {}) => ({ type: "gift-check-capture", schema: 1, store: "pandora-us", capture_id: "pandora-abc12345", chunk: 1, chunks: 1, products: [{ id: "1", price: 10 }], ...extra });
const ev = (source, origin, data) => ({ source, origin, data });

test("a capture from the store tab we opened, on the store's origin, is accepted", () => {
  const v = classify(opened, ev(storeTab, "https://us.pandora.net", capture()));
  assert.equal(v.accept, true);
  assert.equal(v.entry.store, "pandora-us");
});

test("a window we did not open is ignored, even on the store's origin", () => {
  assert.ok(classify(opened, ev(otherTab, "https://us.pandora.net", capture())).ignore);
  assert.ok(classify(opened, ev(null, "https://us.pandora.net", capture())).ignore);
});

test("our tab after it navigated to another origin is ignored", () => {
  assert.ok(classify(opened, ev(storeTab, "https://evil.example", capture())).ignore);
  assert.ok(classify(opened, ev(storeTab, "http://us.pandora.net", capture())).ignore, "plain http is another origin");
  assert.ok(classify(opened, ev(storeTab, "null", capture())).ignore);
});

test("other message types from our tab are skipped silently", () => {
  assert.deepEqual(classify(opened, ev(storeTab, "https://us.pandora.net", { type: "something-else" })), { skip: true });
  assert.deepEqual(classify(opened, ev(storeTab, "https://us.pandora.net", "text")), { skip: true });
  assert.deepEqual(classify(opened, ev(storeTab, "https://us.pandora.net", null)), { skip: true });
});

test("a capture naming another store is refused with an answer", () => {
  const v = classify(opened, ev(storeTab, "https://us.pandora.net", capture({ store: "hardjewelry" })));
  assert.match(v.refuse, /opened for pandora-us/);
  assert.equal(v.accept, undefined);
});

test("NaN and Infinity are refused before anything is posted (JSON would turn them into null)", () => {
  for (const bad of [NaN, Infinity, -Infinity]) {
    const v = classify(opened, ev(storeTab, "https://us.pandora.net", capture({ products: [{ id: "1", price: 10 }, { id: "2", variants: [{ id: "v", list_price: bad }] }] })));
    assert.match(v.refuse, /message\.products\[1\]\.variants\[0\]\.list_price is NaN or Infinity/, String(bad));
  }
  assert.equal(nonFinite({ a: [1, 2.5, { b: 0 }], c: "NaN" }), null, "finite numbers and strings are fine");
  assert.equal(nonFinite(capture({ chunk: NaN })), "message.chunk");
});
