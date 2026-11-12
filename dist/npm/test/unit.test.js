'use strict';
// Unit tests for the pure parts of the pgxdb wrapper (no network, no pgx).
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const pgxdb = require('../lib/pgxdb.js');

test('platformTarget maps node platform/arch to PGX release target names', () => {
  assert.equal(pgxdb.platformTarget('darwin', 'arm64'), 'darwin-arm64');
  assert.equal(pgxdb.platformTarget('darwin', 'x64'), 'darwin-amd64');
  assert.equal(pgxdb.platformTarget('linux', 'arm64'), 'linux-arm64');
  assert.equal(pgxdb.platformTarget('linux', 'x64'), 'linux-amd64');
});

test('platformTarget rejects unsupported platforms with a readable message', () => {
  assert.throws(() => pgxdb.platformTarget('win32', 'x64'), /win32.*not supported.*macOS.*Linux/s);
  assert.throws(() => pgxdb.platformTarget('linux', 'ia32'), /ia32.*not supported/s);
});

test('assetName and assetUrl follow the GitHub release layout', () => {
  assert.equal(pgxdb.assetName('0.1.0', 'linux-amd64'), 'pgx-0.1.0-linux-amd64.tar.gz');
  assert.equal(
    pgxdb.assetUrl('https://github.com/pgrundev/pgx/releases/download', '0.1.0', 'darwin-arm64'),
    'https://github.com/pgrundev/pgx/releases/download/v0.1.0/pgx-0.1.0-darwin-arm64.tar.gz'
  );
  // A trailing slash on the base must not produce a double slash.
  assert.equal(
    pgxdb.assetUrl('http://127.0.0.1:1/', '0.1.0-beta.1', 'linux-arm64'),
    'http://127.0.0.1:1/v0.1.0-beta.1/pgx-0.1.0-beta.1-linux-arm64.tar.gz'
  );
});

test('resolveVersion strips a leading v and prefers PGX_VERSION over the package version', () => {
  assert.equal(pgxdb.resolveVersion({ PGX_VERSION: 'v0.2.0' }, '0.1.0'), '0.2.0');
  assert.equal(pgxdb.resolveVersion({ PGX_VERSION: '' }, '0.1.0'), '0.1.0');
  assert.equal(pgxdb.resolveVersion({}, '0.1.0'), '0.1.0');
});

test('parseSha256 accepts sha256sum and shasum formats and rejects garbage', () => {
  const hex = 'a'.repeat(64);
  assert.equal(pgxdb.parseSha256(`${hex}  pgx-0.1.0-linux-amd64.tar.gz\n`), hex);
  assert.equal(pgxdb.parseSha256(`${hex} *pgx.tar.gz`), hex);
  assert.equal(pgxdb.parseSha256(`${hex.toUpperCase()}\n`), hex);
  assert.throws(() => pgxdb.parseSha256(''), /checksum/);
  assert.throws(() => pgxdb.parseSha256('<html>not found</html>'), /checksum/);
});

test('installDir defaults to ~/.pgx/dist/<version> and honours PGX_INSTALL_DIR', () => {
  assert.equal(pgxdb.installDir({}, '0.1.0', '/home/u'), path.join('/home/u', '.pgx', 'dist', '0.1.0'));
  assert.equal(pgxdb.installDir({ PGX_INSTALL_DIR: '/opt/pgx' }, '0.1.0', '/home/u'), '/opt/pgx');
});

test('isValidInstall requires an executable bin/pgx and a dist.json for that version', () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'pgxdb-unit-'));
  try {
    assert.equal(pgxdb.isValidInstall(dir, '0.1.0'), false, 'empty dir');
    fs.mkdirSync(path.join(dir, 'bin'));
    fs.mkdirSync(path.join(dir, 'share', 'pgx'), { recursive: true });
    fs.writeFileSync(path.join(dir, 'bin', 'pgx'), '#!/bin/sh\n', { mode: 0o755 });
    assert.equal(pgxdb.isValidInstall(dir, '0.1.0'), false, 'no dist.json');
    fs.writeFileSync(path.join(dir, 'share', 'pgx', 'dist.json'), JSON.stringify({ pgx_version: '0.0.9' }));
    assert.equal(pgxdb.isValidInstall(dir, '0.1.0'), false, 'wrong version');
    fs.writeFileSync(path.join(dir, 'share', 'pgx', 'dist.json'), JSON.stringify({ pgx_version: '0.1.0' }));
    assert.equal(pgxdb.isValidInstall(dir, '0.1.0'), true);
    fs.chmodSync(path.join(dir, 'bin', 'pgx'), 0o644);
    assert.equal(pgxdb.isValidInstall(dir, '0.1.0'), false, 'not executable');
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test('pgxArgs defaults to `run` when no arguments are given', () => {
  assert.deepEqual(pgxdb.pgxArgs([]), ['run']);
  assert.deepEqual(pgxdb.pgxArgs(['ls', '--json']), ['ls', '--json']);
  assert.deepEqual(pgxdb.pgxArgs(['--json']), ['--json', 'run'], 'bare --json still means run');
});

test('missingAssetMessage explains a release that predates macOS Intel', () => {
  const url = 'https://example/v0.1.0/pgx-0.1.0-darwin-amd64.tar.gz';
  const m = pgxdb.missingAssetMessage('darwin-amd64', '0.1.0', url, 404);
  assert.match(m, /download failed: .*darwin-amd64\.tar\.gz: HTTP 404/);
  assert.match(m, /0\.1\.0 predates macOS on Intel/);
  assert.match(m, /pgxdb@latest/);
  // Other targets get the plain message only.
  const other = pgxdb.missingAssetMessage('linux-amd64', '0.1.0', url, 404);
  assert.equal(other, `download failed: ${url}: HTTP 404`);
  // A non-404 (e.g. 500) is a plain failure even on Intel macs.
  assert.equal(pgxdb.missingAssetMessage('darwin-amd64', '0.1.0', url, 503), `download failed: ${url}: HTTP 503`);
});
