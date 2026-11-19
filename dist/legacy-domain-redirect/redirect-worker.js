// Transition redirect for the previous domain: https://pgx.dev/* → https://pgboxdb.com/*
//
// Authorised for at most 90 days. REDIRECT_UNTIL (wrangler.toml) is the hard
// end: from that instant the Worker stops redirecting and passes every request
// through to the zone's normal handling, as if it were not deployed. Remove the
// route after the deadline (README).
//
// * 301 (permanent), but with a bounded Cache-Control so browsers do not keep
//   the redirect forever after the transition ends.
// * The path is kept where pgboxdb.com has the same page. Any other path goes
//   to the home page. The query string is always kept.
// * /install is never redirected: it is a shell script for `curl … | sh`, and
//   it is served by its own, more specific route (pgx-install Worker). It is
//   passed through here too, in case this Worker ever sees it.

const TARGET = "https://pgboxdb.com";
// Paths that exist on both sites (checked 2026-11-19).
const SAME_PATH = new Set(["/", "/index.html", "/llms.txt", "/robots.txt"]);
const PASS_THROUGH = new Set(["/install", "/install/"]);
const CACHE_SECONDS = 3600;

export function redirectEnd(env) {
  const t = Date.parse(env && env.REDIRECT_UNTIL);
  return Number.isFinite(t) ? t : null;
}

export function target(url) {
  const path = SAME_PATH.has(url.pathname) ? url.pathname : "/";
  return TARGET + path + url.search;
}

export default {
  async fetch(request, env, _ctx, now = Date.now()) {
    const url = new URL(request.url);
    const end = redirectEnd(env);
    // Fail closed on a missing or invalid deadline: no redirect without an end.
    if (end === null || now >= end || PASS_THROUGH.has(url.pathname)) {
      return fetch(request);
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
