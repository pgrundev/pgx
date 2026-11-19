// node --test dist/legacy-domain-redirect/redirect-worker.test.js
import { test } from "node:test";
import assert from "node:assert/strict";
import worker, { target } from "./redirect-worker.js";

const END = "2027-02-17T20:00:00Z";
const env = { REDIRECT_UNTIL: END };
const before = Date.parse(END) - 1000;
const after = Date.parse(END);

function stubFetch() {
  const calls = [];
  globalThis.fetch = async (req) => {
    calls.push(req.url);
    return new Response("origin", { status: 200 });
  };
  return calls;
}
const get = (u, e = env, now = before) => worker.fetch(new Request(u), e, {}, now);

test("home page: 301 to pgboxdb.com, query kept, bounded cache", async () => {
  const r = await get("https://pgx.dev/?utm_source=x&a=1");
  assert.equal(r.status, 301);
  assert.equal(r.headers.get("location"), "https://pgboxdb.com/?utm_source=x&a=1");
  assert.equal(r.headers.get("cache-control"), "public, max-age=3600");
  assert.equal(r.headers.get("x-redirect-until"), "2027-02-17T20:00:00.000Z");
});

test("http is redirected to https on the new domain", async () => {
  const r = await get("http://pgx.dev/llms.txt");
  assert.equal(r.headers.get("location"), "https://pgboxdb.com/llms.txt");
});

test("paths that exist on both sites are kept", () => {
  for (const p of ["/", "/index.html", "/llms.txt", "/robots.txt"]) {
    assert.equal(target(new URL("https://pgx.dev" + p)), "https://pgboxdb.com" + p);
  }
});

test("other paths go to the home page", () => {
  assert.equal(target(new URL("https://pgx.dev/docs/x?q=1")), "https://pgboxdb.com/?q=1");
  assert.equal(target(new URL("https://pgx.dev/nope")), "https://pgboxdb.com/");
});

test("/install is never redirected", async () => {
  const calls = stubFetch();
  for (const p of ["/install", "/install/"]) {
    const r = await get("https://pgx.dev" + p);
    assert.equal(r.status, 200);
  }
  assert.equal(calls.length, 2);
});

test("at and after the deadline the Worker passes through", async () => {
  const calls = stubFetch();
  const r = await get("https://pgx.dev/", env, after);
  assert.equal(r.status, 200);
  assert.equal(calls.length, 1);
});

test("missing or invalid deadline: no redirect (fail closed)", async () => {
  const calls = stubFetch();
  assert.equal((await get("https://pgx.dev/", {})).status, 200);
  assert.equal((await get("https://pgx.dev/", { REDIRECT_UNTIL: "soon" })).status, 200);
  assert.equal(calls.length, 2);
});

test("redirect target never points back at pgx.dev (no loop)", () => {
  for (const p of ["/", "/a", "/install-x", "//pgx.dev/"]) {
    assert.ok(new URL(target(new URL("https://pgx.dev" + p))).host === "pgboxdb.com");
  }
});
