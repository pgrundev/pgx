// The previous domain, pgx.dev: every path is answered here (route pgx.dev/*).
//
// Until REDIRECT_UNTIL (the authorised 90-day transition):
//   * 301 to https://pgboxdb.com, keeping the path where pgboxdb.com has the
//     same page (else the home page) and always the query string, with a
//     bounded Cache-Control so browsers re-check;
//   * /install goes to the installer Worker through the INSTALLER service
//     binding, so the shell script is never redirected to an HTML page, and no
//     route ordering can send it anywhere else.
// From REDIRECT_UNTIL on, or if the deadline is missing or invalid:
//   * 410 Gone for every path, /install included. The previous website is never
//     served again (the Worker never forwards to the origin).

const TARGET = "https://pgboxdb.com";
// Paths that exist on both sites (checked 2026-11-19).
const SAME_PATH = new Set(["/", "/index.html", "/llms.txt", "/robots.txt"]);
const CACHE_SECONDS = 3600;

export function redirectEnd(env) {
  const t = Date.parse(env && env.REDIRECT_UNTIL);
  return Number.isFinite(t) ? t : null;
}

export function target(url) {
  const path = SAME_PATH.has(url.pathname) ? url.pathname : "/";
  return TARGET + path + url.search;
}

export function gone() {
  return new Response("410 Gone\n", {
    status: 410,
    headers: {
      "content-type": "text/plain; charset=utf-8",
      "cache-control": `public, max-age=${CACHE_SECONDS}`,
    },
  });
}

export default {
  async fetch(request, env, _ctx, now = Date.now()) {
    const url = new URL(request.url);
    const end = redirectEnd(env);
    if (end === null || now >= end) return gone();
    if (url.pathname === "/install") {
      if (!env.INSTALLER) return gone();
      return env.INSTALLER.fetch(request);
    }
    return new Response(null, {
      status: 301,
      headers: {
        location: target(url),
        "cache-control": `public, max-age=${CACHE_SECONDS}`,
        "x-redirect-until": new Date(end).toISOString(),
      },
    });
  },
};
