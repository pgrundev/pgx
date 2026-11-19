// node --test dist/legacy-domain-redirect/redirect-worker.test.js
import { test } from "node:test";
import assert from "node:assert/strict";
import worker, { target } from "./redirect-worker.js";

const END = "2027-02-17T19:58:29Z";
const before = Date.parse(END) - 1000;
const after = Date.parse(END);

function installer() {
  const calls = [];
  return {
    calls,
    fetch: async (req) => {
      calls.push(new URL(req.url).pathname);
      return new Response("#!/bin/sh\n", { status: 200, headers: { "content-type": "text/x-shellscript" } });
    },
  };
}
// Any fetch to the origin would expose the previous site: fail the test.
globalThis.fetch = async () => {
  throw new Error("must never forward to the origin");
};
const run = (u, env, now = before) => worker.fetch(new Request(u), env, {}, now);
const env = () => ({ REDIRECT_UNTIL: END, INSTALLER: installer() });

test("home page: 301 to pgboxdb.com, query kept, bounded cache", async () => {
  const r = await run("https://pgx.dev/?utm_source=x&a=1", env());
  assert.equal(r.status, 301);
  assert.equal(r.headers.get("location"), "https://pgboxdb.com/?utm_source=x&a=1");
  assert.equal(r.headers.get("cache-control"), "public, max-age=3600");
  assert.equal(r.headers.get("x-redirect-until"), "2027-02-17T19:58:29.000Z");
});

test("http is redirected to https on the new domain", async () => {
  const r = await run("http://pgx.dev/llms.txt", env());
  assert.equal(r.headers.get("location"), "https://pgboxdb.com/llms.txt");
});

test("paths that exist on both sites are kept, others go home", () => {
  for (const p of ["/", "/index.html", "/llms.txt", "/robots.txt"]) {
    assert.equal(target(new URL("https://pgx.dev" + p)), "https://pgboxdb.com" + p);
  }
  assert.equal(target(new URL("https://pgx.dev/docs/x?q=1")), "https://pgboxdb.com/?q=1");
});

test("/install goes to the installer Worker (service binding), never redirected", async () => {
  const e = env();
  const r = await run("https://pgx.dev/install", e);
  assert.equal(r.status, 200);
  assert.match(r.headers.get("content-type"), /shellscript/);
  assert.deepEqual(e.INSTALLER.calls, ["/install"]);
});

test("/install/ and other near misses are redirected, not served from the origin", async () => {
  const r = await run("https://pgx.dev/install/", env());
  assert.equal(r.status, 301);
});

test("at and after the deadline: 410 for every path, /install included", async () => {
  const e = env();
  for (const p of ["/", "/llms.txt", "/install", "/anything?x=1"]) {
    const r = await run("https://pgx.dev" + p, e, after);
    assert.equal(r.status, 410, p);
    assert.equal(await r.text(), "410 Gone\n");
  }
  assert.equal(e.INSTALLER.calls.length, 0);
});

test("missing or invalid deadline, or missing binding: 410, never the old site", async () => {
  assert.equal((await run("https://pgx.dev/", {})).status, 410);
  assert.equal((await run("https://pgx.dev/", { REDIRECT_UNTIL: "soon" })).status, 410);
  assert.equal((await run("https://pgx.dev/install", { REDIRECT_UNTIL: END })).status, 410);
});

test("redirect target never points back at pgx.dev (no loop)", () => {
  for (const p of ["/", "/a", "/install-x", "//pgx.dev/"]) {
    assert.equal(new URL(target(new URL("https://pgx.dev" + p))).host, "pgboxdb.com");
  }
});
