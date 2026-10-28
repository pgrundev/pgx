'use strict';
// pgxdb: a thin npm wrapper around the PGX CLI. It downloads the PGX release
// tarball for this OS/CPU once (SHA-256 verified), caches it under
// ~/.pgx/dist/<version> — the same place `curl -fsSL https://pgx.dev/install | sh`
// installs to — and execs `pgx` with the given arguments (`run` by default).
//
// Environment:
//   PGX_VERSION       PGX release to install (default: this package's version)
//   PGX_INSTALL_DIR   where the distribution tree goes (default: ~/.pgx/dist/<version>)
//   PGX_BASE_URL      release download base (default: the GitHub releases URL)
// Everything else (PGX_HOME, PGX_PORT, …) is passed through to pgx.

const crypto = require('node:crypto');
const fs = require('node:fs');
const http = require('node:http');
const https = require('node:https');
const os = require('node:os');
const path = require('node:path');
const { spawn, spawnSync } = require('node:child_process');
const { pipeline } = require('node:stream/promises');

const REPO = 'pgrundev/pgx';
const DEFAULT_BASE_URL = `https://github.com/${REPO}/releases/download`;
const PKG = require('../package.json');

class PgxdbError extends Error {}

function log(msg) {
  process.stderr.write(`pgxdb: ${msg}\n`);
}

/** Map Node's platform/arch to the PGX release target (`<os>-<arch>`). */
function platformTarget(platform, arch) {
  const oses = { darwin: 'darwin', linux: 'linux' };
  const arches = { arm64: 'arm64', x64: 'amd64' };
  if (!oses[platform] || !arches[arch]) {
    throw new PgxdbError(
      `${platform}/${arch} is not supported. PGX runs on macOS (arm64, x64) and Linux (arm64, x64).`
    );
  }
  return `${oses[platform]}-${arches[arch]}`;
}

function assetName(version, target) {
  return `pgx-${version}-${target}.tar.gz`;
}

function assetUrl(baseUrl, version, target) {
  return `${baseUrl.replace(/\/+$/, '')}/v${version}/${assetName(version, target)}`;
}

function resolveVersion(env, packageVersion) {
  const v = (env.PGX_VERSION || '').trim();
  return (v || packageVersion).replace(/^v/, '');
}

function installDir(env, version, home) {
  return env.PGX_INSTALL_DIR || path.join(home, '.pgx', 'dist', version);
}

/** The first 64-hex-digit token of a `.sha256` file (sha256sum or shasum format). */
function parseSha256(text) {
  const m = /^\s*([0-9a-fA-F]{64})(?:\s|$)/.exec(text || '');
  if (!m) throw new PgxdbError('checksum file is malformed (expected a 64-hex-digit SHA-256)');
  return m[1].toLowerCase();
}

function isValidInstall(dir, version) {
  try {
    const bin = path.join(dir, 'bin', 'pgx');
    fs.accessSync(bin, fs.constants.X_OK);
    if (!fs.statSync(bin).isFile()) return false;
    const info = JSON.parse(fs.readFileSync(path.join(dir, 'share', 'pgx', 'dist.json'), 'utf8'));
    return info.pgx_version === version;
  } catch {
    return false;
  }
}

/** Arguments to pass to pgx: `run` when none are given (a bare `--json` still means run). */
function pgxArgs(argv) {
  const positional = argv.filter((a) => !a.startsWith('--'));
  return positional.length === 0 ? [...argv, 'run'] : argv;
}

/** The error for a release asset that could not be fetched (HTTP status != 2xx). */
function missingAssetMessage(target, version, url, status) {
  let msg = `download failed: ${url}: HTTP ${status}`;
  if (target === 'darwin-amd64' && status === 404) {
    msg +=
      `\n  PGX ${version} predates macOS on Intel: no binary is published for darwin-amd64.` +
      '\n  Intel Macs are supported from the first release after 0.1.2; install `pgxdb@latest`.';
  }
  return msg;
}

/**
 * GET `url` with Node's core http/https client, following up to `maxRedirects`
 * redirects (github.com answers release downloads with a 302 to a CDN host).
 * Resolves with the final response stream; the caller checks the status.
 *
 * Not `fetch`: Node's bundled undici can crash the process
 * (`assert(!this.paused)`) when a server closes the connection while the
 * body stream is under backpressure, which CDNs and simple HTTP/1.0 servers
 * do after every response. The core client has no such path.
 */
function httpGet(url, maxRedirects = 10) {
  return new Promise((resolve, reject) => {
    let u;
    try {
      u = new URL(url);
    } catch (e) {
      reject(new Error(`invalid URL: ${e.message}`));
      return;
    }
    const client = u.protocol === 'https:' ? https : u.protocol === 'http:' ? http : null;
    if (!client) {
      reject(new Error(`unsupported URL scheme ${u.protocol}`));
      return;
    }
    const headers = { 'user-agent': `pgxdb/${PKG.version} node/${process.version}`, accept: '*/*' };
    const req = client.get(u, { headers }, (res) => {
      const { statusCode, headers: h } = res;
      if ([301, 302, 303, 307, 308].includes(statusCode) && h.location) {
        res.resume(); // discard the redirect body
        if (maxRedirects === 0) {
          reject(new Error('too many redirects'));
          return;
        }
        resolve(httpGet(new URL(h.location, u).href, maxRedirects - 1));
        return;
      }
      resolve(res);
    });
    req.on('error', reject);
  });
}

/** GET `url`; rejects with a PgxdbError on a network error or a non-2xx status. */
async function getOk(url, onHttpError) {
  let res;
  try {
    res = await httpGet(url);
  } catch (e) {
    throw new PgxdbError(`download failed: ${url}: ${e.message}`);
  }
  if (res.statusCode < 200 || res.statusCode >= 300) {
    res.resume();
    throw new PgxdbError(onHttpError ? onHttpError(res.statusCode) : `download failed: ${url}: HTTP ${res.statusCode}`);
  }
  return res;
}

async function download(url, dest, { onProgress, onHttpError } = {}) {
  const res = await getOk(url, onHttpError);
  const total = Number(res.headers['content-length']) || 0;
  let received = 0;
  if (onProgress) {
    res.on('data', (chunk) => {
      received += chunk.length;
      onProgress(received, total);
    });
  }
  try {
    await pipeline(res, fs.createWriteStream(dest));
  } catch (e) {
    throw new PgxdbError(`download failed: ${url}: ${e.message}`);
  }
}

async function fetchText(url) {
  const res = await getOk(url);
  const chunks = [];
  try {
    for await (const chunk of res) chunks.push(chunk);
  } catch (e) {
    throw new PgxdbError(`download failed: ${url}: ${e.message}`);
  }
  return Buffer.concat(chunks).toString('utf8');
}

function sha256File(file) {
  const h = crypto.createHash('sha256');
  h.update(fs.readFileSync(file));
  return h.digest('hex');
}

function progressReporter(name) {
  if (!process.stderr.isTTY) return undefined;
  let lastPct = -1;
  return (received, total) => {
    if (!total) return;
    const pct = Math.floor((received / total) * 100);
    if (pct === lastPct) return;
    lastPct = pct;
    process.stderr.write(`\rpgxdb: downloading ${name} … ${pct}%${pct === 100 ? '\n' : ''}`);
  };
}

/**
 * Ensure the PGX distribution for `version` is installed under `dir`;
 * download, verify and extract it if not. Returns the path of bin/pgx.
 */
async function ensureInstalled({ version, target, dir, baseUrl }) {
  if (isValidInstall(dir, version)) return path.join(dir, 'bin', 'pgx');

  const name = assetName(version, target);
  const url = assetUrl(baseUrl, version, target);
  const parent = path.dirname(dir);
  fs.mkdirSync(parent, { recursive: true });
  // Sibling of the final dir so the final rename is atomic (same filesystem).
  const tmp = fs.mkdtempSync(path.join(parent, `.pgxdb-${version}-`));
  try {
    const tarball = path.join(tmp, name);
    log(`downloading PGX ${version} (${target}) from ${url}`);
    await download(url, tarball, {
      onProgress: progressReporter(name),
      onHttpError: (status) => missingAssetMessage(target, version, url, status),
    });
    const expected = parseSha256(await fetchText(`${url}.sha256`));
    const actual = sha256File(tarball);
    if (expected !== actual) {
      throw new PgxdbError(
        `checksum mismatch for ${name} (expected ${expected}, got ${actual}); refusing to install`
      );
    }
    const tar = spawnSync('tar', ['-xzf', tarball, '-C', tmp], { stdio: ['ignore', 'ignore', 'pipe'] });
    if (tar.error) throw new PgxdbError(`cannot run tar: ${tar.error.message}`);
    if (tar.status !== 0) throw new PgxdbError(`extract failed: ${tar.stderr.toString().trim()}`);
    const extracted = path.join(tmp, name.replace(/\.tar\.gz$/, ''));
    if (!isValidInstall(extracted, version)) {
      throw new PgxdbError(`archive ${name} does not contain a valid PGX ${version} distribution`);
    }
    // Move a stale (invalid) install aside first so the rename into place is
    // atomic; a concurrent pgxdb may also have installed the same version first.
    const stale = `${dir}.old-${process.pid}`;
    if (fs.existsSync(dir) && !isValidInstall(dir, version)) {
      fs.renameSync(dir, stale);
    }
    try {
      fs.renameSync(extracted, dir);
    } catch (e) {
      if (!isValidInstall(dir, version)) throw e;
    }
    fs.rmSync(stale, { recursive: true, force: true });
    log(`installed PGX ${version} to ${dir}`);
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
  return path.join(dir, 'bin', 'pgx');
}

/** Run pgx with inherited stdio; resolves with the exit code, or re-raises the signal. */
function runPgx(bin, args) {
  return new Promise((resolve, reject) => {
    const child = spawn(bin, args, { stdio: 'inherit' });
    const forward = (sig) => () => {
      try {
        child.kill(sig);
      } catch {
        /* already gone */
      }
    };
    const handlers = ['SIGINT', 'SIGTERM', 'SIGHUP'].map((sig) => {
      const h = forward(sig);
      process.on(sig, h);
      return [sig, h];
    });
    child.on('error', reject);
    child.on('exit', (code, signal) => {
      for (const [sig, h] of handlers) process.off(sig, h);
      if (signal) {
        process.kill(process.pid, signal);
        return;
      }
      resolve(code ?? 1);
    });
  });
}

async function main(argv = process.argv.slice(2), env = process.env) {
  try {
    const target = platformTarget(process.platform, process.arch);
    const version = resolveVersion(env, PKG.version);
    const dir = installDir(env, version, os.homedir());
    const baseUrl = env.PGX_BASE_URL || DEFAULT_BASE_URL;
    const bin = await ensureInstalled({ version, target, dir, baseUrl });
    return await runPgx(bin, pgxArgs(argv));
  } catch (e) {
    if (e instanceof PgxdbError) {
      log(e.message);
      return 1;
    }
    throw e;
  }
}

module.exports = {
  DEFAULT_BASE_URL,
  PgxdbError,
  assetName,
  assetUrl,
  ensureInstalled,
  installDir,
  isValidInstall,
  main,
  missingAssetMessage,
  parseSha256,
  pgxArgs,
  platformTarget,
  resolveVersion,
  runPgx,
};
