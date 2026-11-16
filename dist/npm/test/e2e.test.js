'use strict';
// End-to-end tests for the pgboxdb wrapper against a LOCAL http server serving a
// fake PGBoxDB tarball (no network, no real pgboxdb). The fake `pgboxdb` is a shell script
// that echoes its arguments, so these tests prove: download → checksum verify →
// extract → run, the cache hit on the second run, argument pass-through, exit
// code propagation, and refusal of a tampered or missing download.
const test = require('node:test');
const assert = require('node:assert/strict');
const crypto = require('node:crypto');
const fs = require('node:fs');
const http = require('node:http');
const os = require('node:os');
const path = require('node:path');
const { spawn, spawnSync } = require('node:child_process');

const BIN = path.join(__dirname, '..', 'bin', 'pgboxdb.js');
const VERSION = '9.9.9-test';
const platform = require('../lib/pgboxdb.js');
const TARGET = platform.platformTarget(process.platform, process.arch);
const NAME = `pgboxdb-${VERSION}-${TARGET}`;

function makeTarball(root, version, target) {
  const dist = path.join(root, `pgboxdb-${version}-${target}`);
  fs.mkdirSync(path.join(dist, 'bin'), { recursive: true });
  fs.mkdirSync(path.join(dist, 'share', 'pgboxdb'), { recursive: true });
  fs.writeFileSync(
    path.join(dist, 'bin', 'pgboxdb'),
    '#!/bin/sh\necho "fake-pgboxdb args: $*"\nif [ "$1" = exit-code ]; then exit "$2"; fi\nexit 0\n',
    { mode: 0o755 }
  );
  fs.writeFileSync(path.join(dist, 'share', 'pgboxdb', 'dist.json'), JSON.stringify({ pgboxdb_version: version, os: 'x', arch: 'y' }));
  const tgz = path.join(root, `pgboxdb-${version}-${target}.tar.gz`);
  const r = spawnSync('tar', ['-czf', tgz, '-C', root, `pgboxdb-${version}-${target}`]);
  assert.equal(r.status, 0, r.stderr.toString());
  const sum = crypto.createHash('sha256').update(fs.readFileSync(tgz)).digest('hex');
  return { tgz, sum };
}

/** Serve files from `dir` under /v<version>/; count requests per path. */
function startServer(dir, { connectionClose = false } = {}) {
  const hits = {};
  const overrides = {};
  const server = http.createServer((req, res) => {
    hits[req.url] = (hits[req.url] || 0) + 1;
    if (connectionClose) res.setHeader('connection', 'close');
    if (overrides[req.url] !== undefined) {
      res.writeHead(200);
      res.end(overrides[req.url]);
      return;
    }
    const file = path.join(dir, req.url);
    if (!fs.existsSync(file)) {
      res.writeHead(404);
      res.end('not found');
      return;
    }
    res.writeHead(200, { 'content-length': fs.statSync(file).size });
    fs.createReadStream(file).pipe(res);
  });
  return new Promise((resolve) => {
    server.listen(0, '127.0.0.1', () => {
      resolve({ server, hits, overrides, url: `http://127.0.0.1:${server.address().port}` });
    });
  });
}

/** A server that answers every request with a redirect to `location(req)`. */
function startRedirector(location) {
  const server = http.createServer((req, res) => {
    res.writeHead(302, { location: location(req) });
    res.end();
  });
  return new Promise((resolve) => {
    server.listen(0, '127.0.0.1', () => resolve({ server, url: `http://127.0.0.1:${server.address().port}` }));
  });
}

/** Run the wrapper asynchronously: the test's own HTTP server must stay responsive. */
function runPgboxdb(args, env) {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [BIN, ...args], { env: { PATH: process.env.PATH, ...env } });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', (d) => (stdout += d));
    child.stderr.on('data', (d) => (stderr += d));
    child.on('error', reject);
    child.on('close', (status) => resolve({ status, stdout, stderr }));
  });
}

test('pgboxdb end to end', async (t) => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'pgboxdb-e2e-'));
  const srvRoot = path.join(tmp, 'srv', `v${VERSION}`);
  const stage = path.join(tmp, 'stage');
  const { tgz, sum } = makeTarball(stage, VERSION, TARGET);
  fs.mkdirSync(srvRoot, { recursive: true });
  fs.copyFileSync(tgz, path.join(srvRoot, `${NAME}.tar.gz`));
  fs.writeFileSync(path.join(srvRoot, `${NAME}.tar.gz.sha256`), `${sum}  ${NAME}.tar.gz\n`);
  const srv = await startServer(path.join(tmp, 'srv'));
  const home = path.join(tmp, 'home');
  const env = { HOME: home, PGBOXDB_VERSION: VERSION, PGBOXDB_BASE_URL: srv.url };
  const installDir = path.join(home, '.pgboxdb', 'dist', VERSION);
  const tgzPath = `/v${VERSION}/${NAME}.tar.gz`;
  t.after(() => {
    srv.server.close();
    fs.rmSync(tmp, { recursive: true, force: true });
  });

  await t.test('first run downloads, verifies, installs and runs `pgboxdb run` by default', async () => {
    const r = await runPgboxdb([], env);
    assert.equal(r.status, 0, r.stderr);
    assert.match(r.stdout, /^fake-pgboxdb args: run\n$/);
    assert.match(r.stderr, /downloading PGBoxDB 9\.9\.9-test/);
    assert.match(r.stderr, /installed PGBoxDB 9\.9\.9-test to /);
    assert.equal(srv.hits[tgzPath], 1);
    assert.equal(srv.hits[`${tgzPath}.sha256`], 1);
    assert.ok(fs.existsSync(path.join(installDir, 'bin', 'pgboxdb')), 'bin/pgboxdb installed in ~/.pgboxdb/dist/<version>');
    assert.deepEqual(
      fs.readdirSync(path.join(home, '.pgboxdb', 'dist')).filter((n) => n.startsWith('.')),
      [],
      'no temp dirs left behind'
    );
  });

  await t.test('second run uses the cache and passes arguments through', async () => {
    const r = await runPgboxdb(['ls', '--json'], env);
    assert.equal(r.status, 0, r.stderr);
    assert.equal(r.stdout, 'fake-pgboxdb args: ls --json\n');
    assert.equal(r.stderr, '', 'nothing downloaded, nothing logged');
    assert.equal(srv.hits[tgzPath], 1, 'tarball not fetched again');
  });

  await t.test('a bare --json still runs `run`', async () => {
    const r = await runPgboxdb(['--json'], env);
    assert.equal(r.stdout, 'fake-pgboxdb args: --json run\n');
  });

  await t.test('the pgboxdb exit code is propagated', async () => {
    const r = await runPgboxdb(['exit-code', '7'], env);
    assert.equal(r.status, 7);
  });

  await t.test('PGBOXDB_INSTALL_DIR overrides the cache location', async () => {
    const dir = path.join(tmp, 'custom');
    const r = await runPgboxdb(['version'], { ...env, PGBOXDB_INSTALL_DIR: dir });
    assert.equal(r.status, 0, r.stderr);
    assert.ok(fs.existsSync(path.join(dir, 'bin', 'pgboxdb')));
    assert.equal(srv.hits[tgzPath], 2);
  });

  await t.test('a cached install with the wrong version is replaced', async () => {
    fs.writeFileSync(path.join(installDir, 'share', 'pgboxdb', 'dist.json'), JSON.stringify({ pgboxdb_version: '0.0.1' }));
    // rename() onto a non-empty dir fails on POSIX, so pgboxdb must move the
    // stale dir aside itself and leave no `.old` copy behind.
    const r = await runPgboxdb(['version'], env);
    assert.equal(r.status, 0, r.stderr);
    assert.equal(srv.hits[tgzPath], 3, 're-downloaded');
    assert.equal(JSON.parse(fs.readFileSync(path.join(installDir, 'share', 'pgboxdb', 'dist.json'), 'utf8')).pgboxdb_version, VERSION);
    assert.deepEqual(fs.readdirSync(path.join(home, '.pgboxdb', 'dist')), [VERSION], 'no .old or temp dirs left');
  });

  await t.test('a tampered download is refused and nothing is installed', async () => {
    srv.overrides[`${tgzPath}.sha256`] = `${'0'.repeat(64)}  ${NAME}.tar.gz\n`;
    const dir = path.join(tmp, 'bad');
    const r = await runPgboxdb(['version'], { ...env, PGBOXDB_INSTALL_DIR: dir });
    delete srv.overrides[`${tgzPath}.sha256`];
    assert.equal(r.status, 1);
    assert.match(r.stderr, /checksum mismatch/);
    assert.equal(r.stdout, '');
    assert.ok(!fs.existsSync(dir), 'no partial install');
  });

  await t.test('a missing release asset is reported with the URL and status', async () => {
    const r = await runPgboxdb(['version'], { ...env, PGBOXDB_VERSION: '0.0.0-nope' });
    assert.equal(r.status, 1);
    assert.match(r.stderr, /download failed: .*pgboxdb-0\.0\.0-nope-.*\.tar\.gz: HTTP 404/);
  });

  await t.test('an unreachable server is reported, not thrown as a stack trace', async () => {
    const r = await runPgboxdb(['version'], { ...env, PGBOXDB_BASE_URL: 'http://127.0.0.1:1', PGBOXDB_VERSION: '0.0.0-down' });
    assert.equal(r.status, 1);
    assert.match(r.stderr, /^pgboxdb: download failed: http:\/\/127\.0\.0\.1:1\//m);
    assert.doesNotMatch(r.stderr, /at .*\.js:\d+/);
  });
  await t.test('redirects are followed to a server that closes every connection (GitHub release assets)', async () => {
    // github.com answers release downloads with a 302 to a CDN host; the CDN
    // (and python -m http.server in dist/test/npm_test.sh) closes the
    // connection after each response. Both must work for the tarball and the
    // checksum.
    const cdn = await startServer(path.join(tmp, 'srv'), { connectionClose: true });
    const front = await startRedirector((req) => `${cdn.url}${req.url}`);
    try {
      const home2 = path.join(tmp, 'home-redirect');
      const r = await runPgboxdb(['version'], { ...env, HOME: home2, PGBOXDB_BASE_URL: front.url });
      assert.equal(r.status, 0, r.stderr);
      assert.match(r.stdout, /^fake-pgboxdb args: version\n$/);
      assert.equal(cdn.hits[tgzPath], 1, 'tarball fetched from the redirect target');
      assert.equal(cdn.hits[`${tgzPath}.sha256`], 1, 'checksum fetched from the redirect target');
      assert.ok(platform.isValidInstall(path.join(home2, '.pgboxdb', 'dist', VERSION), VERSION));
    } finally {
      cdn.server.close();
      front.server.close();
    }
  });

  await t.test('a redirect loop is reported as too many redirects', async () => {
    const loop = await startRedirector((req) => req.url);
    try {
      const r = await runPgboxdb(['version'], { ...env, PGBOXDB_BASE_URL: loop.url, PGBOXDB_VERSION: '0.0.0-loop' });
      assert.equal(r.status, 1);
      assert.match(r.stderr, /^pgboxdb: download failed: .*pgboxdb-0\.0\.0-loop-.*\.tar\.gz: too many redirects$/m);
      assert.doesNotMatch(r.stderr, /at .*\.js:\d+/);
    } finally {
      loop.server.close();
    }
  });
});
