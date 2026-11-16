// Tests for the pgboxdb.com /install Worker: pinned-ref requirement, response
// shape, upstream-failure handling, and route/method isolation. Run with:
//   node --test dist/pgboxdb.com/
import { test } from "node:test";
import assert from "node:assert/strict";
import worker from "./install-worker.js";

const SCRIPT = "#!/bin/sh\necho pgboxdb installer\n";

// Install a fake global fetch; returns a recorder of the URLs it was asked for.
function stubFetch(impl) {
  const calls = [];
  globalThis.fetch = async (u, opts) => {
    calls.push(String(u));
    return impl(String(u), opts);
  };
  return calls;
}
const okResp = () => new Response(SCRIPT, { status: 200 });
const notFound = () => new Response("", { status: 404 });

test("valid pinned ref serves the script with the right type and ref header", async () => {
  const calls = stubFetch(okResp);
  const res = await worker.fetch(new Request("https://pgboxdb.com/install"), { INSTALL_REF: "v0.1.0-beta.1" });
  assert.equal(res.status, 200);
  assert.match(res.headers.get("content-type"), /text\/x-shellscript/);
  assert.equal(res.headers.get("x-pgboxdb-install-ref"), "v0.1.0-beta.1");
  assert.equal(await res.text(), SCRIPT);
  assert.equal(calls.length, 1);
  assert.ok(calls[0].includes("/pgrundev/pgboxdb/v0.1.0-beta.1/dist/install.sh"), calls[0]);
});

test("upstream failure returns 502, not an empty 200", async () => {
  stubFetch(notFound);
  const res = await worker.fetch(new Request("https://pgboxdb.com/install"), { INSTALL_REF: "v0.1.0-beta.1" });
  assert.equal(res.status, 502);
  assert.notEqual(await res.text(), "");
});

test("missing INSTALL_REF fails loudly without fetching", async () => {
  const calls = stubFetch(okResp);
  const res = await worker.fetch(new Request("https://pgboxdb.com/install"), {});
  assert.equal(res.status, 500);
  assert.equal(calls.length, 0, "must not fetch when misconfigured");
});

test("mutable branch ref (main) is rejected", async () => {
  const calls = stubFetch(okResp);
  const res = await worker.fetch(new Request("https://pgboxdb.com/install"), { INSTALL_REF: "main" });
  assert.equal(res.status, 500);
  assert.equal(calls.length, 0);
});

test("accepts a commit sha ref", async () => {
  const calls = stubFetch(okResp);
  const res = await worker.fetch(new Request("https://pgboxdb.com/install"), { INSTALL_REF: "4175767612" });
  assert.equal(res.status, 200);
  assert.ok(calls[0].includes("/4175767612/dist/install.sh"));
});

test("route isolation: only /install is served", async () => {
  const calls = stubFetch(okResp);
  const res = await worker.fetch(new Request("https://pgboxdb.com"), { INSTALL_REF: "v0.1.0-beta.1" });
  assert.equal(res.status, 404);
  assert.equal(calls.length, 0);
});

test("non-GET methods are rejected", async () => {
  stubFetch(okResp);
  const res = await worker.fetch(new Request("https://pgboxdb.com/install", { method: "POST" }), { INSTALL_REF: "v0.1.0-beta.1" });
  assert.equal(res.status, 405);
});
