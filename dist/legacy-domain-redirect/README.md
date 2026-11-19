# The previous domain (pgx.dev) during and after the transition

Two Cloudflare Workers answer every request to `pgx.dev`. The domain itself
is never transferred, released or deleted.

| Worker | Route | Source |
|---|---|---|
| `pgx-dev-redirect` | `pgx.dev/*` (every path) | `redirect-worker.js`, `wrangler.toml` |
| `pgx-install` | `pgx.dev/install` | `install/` |

## Timeline

| | UTC |
|---|---|
| Transition activated (first 301) | **2026-11-19 19:58:36** |
| Authorised until (activation + 90 days) | 2027-02-17 19:58:36 |
| `REDIRECT_UNTIL`, enforced in code by both Workers | **2027-02-17 19:58:29** |

## Until `REDIRECT_UNTIL`

* `https://pgx.dev/*` and `http://pgx.dev/*` answer 301 to `https://pgboxdb.com`.
  * `/`, `/index.html`, `/llms.txt` and `/robots.txt` keep their path; these
    are the pages that exist on both sites. Every other path goes to the home
    page. The query string is always kept.
  * `Cache-Control: public, max-age=3600`, so browsers re-check hourly and do
    not keep the 301 after the transition.
* `https://pgx.dev/install` serves the previous product's installer script
  (tag `v0.1.3`). It is unchanged except for one line after the shebang that
  defaults its version to `0.1.3`. Without that line, the script would pick
  "the newest release", which from v0.2.0 on is a PGBoxDB release with other
  asset names, and `curl -fsSL https://pgx.dev/install | sh` would fail. A
  version the user sets still wins.
* There is no `www.pgx.dev` record, so there is nothing to redirect there.

## From `REDIRECT_UNTIL` on

Both Workers answer **`410 Gone`** (plain text, no branding) on every path,
`/install` included. They do this without any redeploy, and also if the
deadline variable is missing or invalid. The redirect Worker never forwards a
request to the origin, so the previous website can't be served again.
`curl -fsSL https://pgx.dev/install | sh` then fails (`curl -f` stops on the
410, and `sh` gets an empty script).

**Keep both Workers deployed after the deadline.** The previous site is still
deployed as the Worker `pgx`. Only the `pgx.dev/*` route keeps it unreachable,
so deleting `pgx-dev-redirect` would bring it back. To retire the old site for
good, remove the `pgx` Worker's domain or route as a separate step that needs
approval. That is not a domain transfer or deletion.

## Why the two routes can't conflict

* Cloudflare uses the most specific matching route, so `pgx.dev/install` (with
  any query string) reaches `pgx-install`. Verified live: 200,
  `text/x-shellscript`, `x-pgx-install-version: 0.1.3`.
* If the redirect Worker ever receives `/install` anyway, it hands the
  request to `pgx-install` through the `INSTALLER` service binding, never to
  the origin or to `pgboxdb.com`.
* `/install/` and every other path go to the redirect Worker: 301 until the
  deadline, then 410. No path reaches the old site. Verified live: `/` answers
  301, not the old page.
* Both Workers read the same `REDIRECT_UNTIL`, so they stop together.
* The tests (`redirect-worker.test.js`, `install/legacy-install-worker.test.js`,
  run in CI) fail if the redirect Worker forwards to the origin, if `/install`
  is redirected, or if anything other than 410 is served after the deadline.

## Deployments

| Worker | Version | Deployed (UTC) | Note |
|---|---|---|---|
| `pgx-dev-redirect` | `5c43142a-18c3-4385-942a-7b98cf8632c7` | 2026-11-19 19:58:36 | first version (activation) |
| `pgx-install` | `d1cc0c44-9283-4fd3-9fce-07c9ef118396` | 2026-11-19 20:04:41 | v0.1.3 script, version pinned |
| `pgx-install` | `f7c5157b-6261-4250-9501-a59715f43c0e` | 2026-11-19 20:23 | + 410 after the deadline |
| `pgx-dev-redirect` | `5d1fab37-4514-412c-98e7-0daa5c826709` | 2026-11-19 20:23:25 | + 410 after the deadline, `INSTALLER` binding |

Before the transition, `pgx-install` was `44d28964-e04c-4899-927a-105f9e6202ed`
(tag v0.1.0's script, unpinned).

## Change

```sh
node --test redirect-worker.test.js install/legacy-install-worker.test.js
(cd install && npx wrangler deploy)    # the binding target first
npx wrangler deploy
```

Never set `REDIRECT_UNTIL` later than `2027-02-17T19:58:36Z` in either file.
