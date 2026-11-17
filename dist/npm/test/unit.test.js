'use strict';
// Unit tests for the pure parts of the pgboxdb wrapper (no network, no pgboxdb).
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const pgboxdb = require('../lib/pgboxdb.js');

test('platformTarget maps node platform/arch to PGBoxDB release target names', () => {
  assert.equal(pgboxdb.platformTarget('darwin', 'arm64'), 'darwin-arm64');
  assert.equal(pgboxdb.platformTarget('darwin', 'x64'), 'darwin-amd64');
  assert.equal(pgboxdb.platformTarget('linux', 'arm64'), 'linux-arm64');
  assert.equal(pgboxdb.platformTarget('linux', 'x64'), 'linux-amd64');
});

test('platformTarget rejects unsupported platforms with a readable message', () => {
  assert.throws(() => pgboxdb.platformTarget('win32', 'x64'), /win32.*not supported.*macOS.*Linux/s);
  assert.throws(() => pgboxdb.platformTarget('linux', 'ia32'), /ia32.*not supported/s);
});

test('assetName and assetUrl follow the GitHub release layout', () => {
  assert.equal(pgboxdb.assetName('0.1.0', 'linux-amd64'), 'pgboxdb-0.1.0-linux-amd64.tar.gz');
  assert.equal(
    pgboxdb.assetUrl('https://github.com/pgrundev/pgboxdb/releases/download', '0.1.0', 'darwin-arm64'),
    'https://github.com/pgrundev/pgboxdb/releases/download/v0.1.0/pgboxdb-0.1.0-darwin-arm64.tar.gz'
  );
  // A trailing slash on the base must not produce a double slash.
  assert.equal(
    pgboxdb.assetUrl('http://127.0.0.1:1/', '0.1.0-beta.1', 'linux-arm64'),
    'http://127.0.0.1:1/v0.1.0-beta.1/pgboxdb-0.1.0-beta.1-linux-arm64.tar.gz'
  );
});

test('resolveVersion strips a leading v and prefers PGBOXDB_VERSION over the package version', () => {
  assert.equal(pgboxdb.resolveVersion({ PGBOXDB_VERSION: 'v0.2.0' }, '0.1.0'), '0.2.0');
  assert.equal(pgboxdb.resolveVersion({ PGBOXDB_VERSION: '' }, '0.1.0'), '0.1.0');
  assert.equal(pgboxdb.resolveVersion({}, '0.1.0'), '0.1.0');
});

test('parseSha256 accepts sha256sum and shasum formats and rejects garbage', () => {
  const hex = 'a'.repeat(64);
  assert.equal(pgboxdb.parseSha256(`${hex}  pgboxdb-0.1.0-linux-amd64.tar.gz\n`), hex);
  assert.equal(pgboxdb.parseSha256(`${hex} *pgboxdb.tar.gz`), hex);
  assert.equal(pgboxdb.parseSha256(`${hex.toUpperCase()}\n`), hex);
  assert.throws(() => pgboxdb.parseSha256(''), /checksum/);
  assert.throws(() => pgboxdb.parseSha256('<html>not found</html>'), /checksum/);
});

test('installDir defaults to ~/.pgboxdb/dist/<version> and honours PGBOXDB_INSTALL_DIR', () => {
  assert.equal(pgboxdb.installDir({}, '0.1.0', '/home/u'), path.join('/home/u', '.pgboxdb', 'dist', '0.1.0'));
  assert.equal(pgboxdb.installDir({ PGBOXDB_INSTALL_DIR: '/opt/pgboxdb' }, '0.1.0', '/home/u'), '/opt/pgboxdb');
});

test('isValidInstall requires an executable bin/pgboxdb and a dist.json for that version', () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'pgboxdb-unit-'));
  try {
    assert.equal(pgboxdb.isValidInstall(dir, '0.1.0'), false, 'empty dir');
    fs.mkdirSync(path.join(dir, 'bin'));
    fs.mkdirSync(path.join(dir, 'share', 'pgboxdb'), { recursive: true });
    fs.writeFileSync(path.join(dir, 'bin', 'pgboxdb'), '#!/bin/sh\n', { mode: 0o755 });
    assert.equal(pgboxdb.isValidInstall(dir, '0.1.0'), false, 'no dist.json');
    fs.writeFileSync(path.join(dir, 'share', 'pgboxdb', 'dist.json'), JSON.stringify({ pgboxdb_version: '0.0.9' }));
    assert.equal(pgboxdb.isValidInstall(dir, '0.1.0'), false, 'wrong version');
    fs.writeFileSync(path.join(dir, 'share', 'pgboxdb', 'dist.json'), JSON.stringify({ pgboxdb_version: '0.1.0' }));
    assert.equal(pgboxdb.isValidInstall(dir, '0.1.0'), true);
    fs.chmodSync(path.join(dir, 'bin', 'pgboxdb'), 0o644);
    assert.equal(pgboxdb.isValidInstall(dir, '0.1.0'), false, 'not executable');
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test('pgboxdbArgs defaults to `run` when no arguments are given', () => {
  assert.deepEqual(pgboxdb.pgboxdbArgs([]), ['run']);
  assert.deepEqual(pgboxdb.pgboxdbArgs(['ls', '--json']), ['ls', '--json']);
  assert.deepEqual(pgboxdb.pgboxdbArgs(['--json']), ['--json', 'run'], 'bare --json still means run');
});

test('missingAssetMessage names macOS Intel as unsupported when its asset is missing', () => {
  const url = 'https://example/v0.1.0/pgboxdb-0.1.0-darwin-amd64.tar.gz';
  const m = pgboxdb.missingAssetMessage('darwin-amd64', '0.1.0', url, 404);
  assert.match(m, /download failed: .*darwin-amd64\.tar\.gz: HTTP 404/);
  assert.match(m, /macOS on Intel .*not supported yet/);
  assert.match(m, /Apple silicon|Linux/);
  // Other targets get the plain message only.
  const other = pgboxdb.missingAssetMessage('linux-amd64', '0.1.0', url, 404);
  assert.equal(other, `download failed: ${url}: HTTP 404`);
  // A non-404 (e.g. 500) is a plain failure even on Intel macs.
  assert.equal(pgboxdb.missingAssetMessage('darwin-amd64', '0.1.0', url, 503), `download failed: ${url}: HTTP 503`);
});

test('ensureInstalled never replaces a distribution from before the rename (and downloads nothing)', async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'pgboxdb-unit-'));
  try {
    fs.mkdirSync(path.join(dir, 'share', 'pgx'), { recursive: true });
    fs.writeFileSync(path.join(dir, 'share', 'pgx', 'dist.json'), JSON.stringify({ pgx_version: '0.1.3' }));
    assert.equal(pgboxdb.isPreRenameDist(dir), true);
    await assert.rejects(
      // An unroutable base URL: the guard must fire before any download.
      pgboxdb.ensureInstalled({ version: '0.1.3', target: 'darwin-arm64', dir, baseUrl: 'http://127.0.0.1:9' }),
      /before the rename to PGBoxDB; refusing to replace it/
    );
    assert.ok(fs.existsSync(path.join(dir, 'share', 'pgx', 'dist.json')), 'the previous dist is untouched');
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});
