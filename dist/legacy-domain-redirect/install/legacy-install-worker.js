// Cloudflare Worker `pgx-install` for https://pgx.dev/install (the previous
// domain's installer endpoint, kept working for existing instructions).
//
// It serves the previous product's installer exactly as tagged (INSTALL_REF,
// e.g. v0.1.3), with one line added after the shebang: a default for its
// version variable (LEGACY_VERSION). Without it the script would install "the
// newest release", which is a PGBoxDB release from v0.2.0 on, whose assets have
// different names, so `curl … | sh` would fail. An explicit version set by the
// user still wins. The installer itself is not changed in any other way.
//
// It serves only during the authorised transition: from REDIRECT_UNTIL on (or
// if that is missing or invalid) every request gets 410 Gone, so the previous
// product's installer stops with the rest of pgx.dev.
const REPO = "pgrundev/pgboxdb";
const REF_RE = /^v\d+\.\d+\.\d+[0-9A-Za-z.-]*$/;
const VERSION_RE = /^\d+\.\d+\.\d+[0-9A-Za-z.-]*$/;
const VERSION_VAR = "PGX_VERSION"; // the variable the tagged installer reads

export function pin(script, version) {
  const nl = script.indexOf("\n");
  if (!script.startsWith("#!") || nl < 0) return null;
  const line = `${VERSION_VAR}="\${${VERSION_VAR}:-${version}}"  # pinned by the pgx.dev /install Worker\n`;
  return script.slice(0, nl + 1) + line + script.slice(nl + 1);
}

const text = (status, msg) =>
  new Response(msg + "\n", { status, headers: { "content-type": "text/plain; charset=utf-8" } });

export default {
  async fetch(request, env, _ctx, now = Date.now()) {
    const end = Date.parse(env && env.REDIRECT_UNTIL);
    if (!Number.isFinite(end) || now >= end) return text(410, "410 Gone");
    const url = new URL(request.url);
    if (url.pathname !== "/install") return text(404, "not found");
    if (request.method !== "GET" && request.method !== "HEAD") return text(405, "method not allowed");
    const ref = env && env.INSTALL_REF;
    const version = env && env.LEGACY_VERSION;
    if (!ref || !REF_RE.test(ref) || !version || !VERSION_RE.test(version)) {
      return text(500, "installer misconfigured: INSTALL_REF and LEGACY_VERSION must be pinned");
    }
    const upstream = await fetch(`https://raw.githubusercontent.com/${REPO}/${ref}/dist/install.sh`, {
      cf: { cacheTtl: 300, cacheEverything: true },
    });
    if (!upstream.ok) return text(502, `installer unavailable (upstream ${upstream.status} for ref ${ref})`);
    const body = pin(await upstream.text(), version);
    if (body === null) return text(502, `installer unavailable (unexpected script at ${ref})`);
    return new Response(body, {
      status: 200,
      headers: {
        "content-type": "text/x-shellscript; charset=utf-8",
        "cache-control": "public, max-age=300",
        "x-pgx-install-ref": ref,
        "x-pgx-install-version": version,
      },
    });
  },
};
