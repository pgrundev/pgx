// node --test dist/legacy-domain-redirect/install/legacy-install-worker.test.js
import { test } from "node:test";
import assert from "node:assert/strict";
import worker, { pin } from "./legacy-install-worker.js";

const SCRIPT = '#!/bin/sh\nset -eu\nversion="${PGX_VERSION:-}"\n';
const END = "2027-02-17T19:58:29Z";
const env = { INSTALL_REF: "v0.1.3", LEGACY_VERSION: "0.1.3", REDIRECT_UNTIL: END };
function stub(status = 200, body = SCRIPT) {
  const calls = [];
  globalThis.fetch = async (u) => { calls.push(String(u)); return new Response(body, { status }); };
  return calls;
}

test("serves the tagged script with the version default after the shebang", async () => {
  const calls = stub();
  const r = await worker.fetch(new Request("https://pgx.dev/install"), env);
  assert.equal(r.status, 200);
  assert.match(r.headers.get("content-type"), /text\/x-shellscript/);
  assert.equal(r.headers.get("x-pgx-install-version"), "0.1.3");
  const lines = (await r.text()).split("\n");
  assert.equal(lines[0], "#!/bin/sh");
  assert.match(lines[1], /^PGX_VERSION="\$\{PGX_VERSION:-0\.1\.3\}"/);
  assert.equal(lines[2], "set -eu");
  assert.deepEqual(calls, ["https://raw.githubusercontent.com/pgrundev/pgboxdb/v0.1.3/dist/install.sh"]);
});

test("an explicit version from the user still wins (shell semantics)", () => {
  assert.ok(pin(SCRIPT, "0.1.3").includes('PGX_VERSION="${PGX_VERSION:-0.1.3}"'));
});

test("refuses anything that is not a shell script", () => {
  assert.equal(pin("<html>", "0.1.3"), null);
});

test("misconfiguration fails loudly", async () => {
  stub();
  for (const e of [{ REDIRECT_UNTIL: END }, { ...env, INSTALL_REF: "main" }, { ...env, LEGACY_VERSION: "latest" }]) {
    assert.equal((await worker.fetch(new Request("https://pgx.dev/install"), e)).status, 500);
  }
});

test("upstream failure or non-script body is a 502, never an empty 200", async () => {
  stub(404, "");
  assert.equal((await worker.fetch(new Request("https://pgx.dev/install"), env)).status, 502);
  stub(200, "<html>oops</html>");
  assert.equal((await worker.fetch(new Request("https://pgx.dev/install"), env)).status, 502);
});

test("only GET/HEAD on /install", async () => {
  stub();
  assert.equal((await worker.fetch(new Request("https://pgx.dev/other"), env)).status, 404);
  assert.equal((await worker.fetch(new Request("https://pgx.dev/install", { method: "POST" }), env)).status, 405);
});

test("from the deadline on, or without one: 410 Gone, upstream never fetched", async () => {
  const calls = stub();
  const at = Date.parse(END);
  for (const [e, now] of [[env, at], [env, at + 86400000], [{ ...env, REDIRECT_UNTIL: undefined }, at - 1000]]) {
    const r = await worker.fetch(new Request("https://pgx.dev/install"), e, {}, now);
    assert.equal(r.status, 410);
    assert.equal(await r.text(), "410 Gone\n");
  }
  assert.equal(calls.length, 0);
});
