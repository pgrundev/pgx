# Transition redirect for the previous domain

A Cloudflare Worker that answers `https://pgx.dev/*` and `http://pgx.dev/*`
with an HTTP 301 to `https://pgboxdb.com`. It runs only for the transition
period after the rename.

| | |
|---|---|
| Worker | `pgx-dev-redirect` (account `af075c4f…`), route `pgx.dev/*` |
| Activated | **2026-11-19 19:58:36 UTC** (version `5c43142a-18c3-4385-942a-7b98cf8632c7`) |
| Authorised until | 90 days after activation, 2027-02-17 19:58:36 UTC |
| Stops redirecting at | **2027-02-17 19:58:29 UTC** (`REDIRECT_UNTIL`, enforced in code) |

Behaviour:

* `/`, `/index.html`, `/llms.txt` and `/robots.txt` keep their path; these
  are the pages that exist on both sites. Every other path goes to
  `https://pgboxdb.com/`. The query string is always kept.
* `/install` is **not** redirected. It is the PGX installer shell script,
  served by its own more specific route (Worker `pgx-install`), so
  `curl -fsSL https://pgx.dev/install | sh` keeps working for existing users.
* `Cache-Control: public, max-age=3600`, so browsers re-check hourly and
  do not keep the 301 after the transition ends.
* There is no `www.pgx.dev` record, so there is nothing to redirect there.
* From `REDIRECT_UNTIL` on, or if the variable is missing or invalid, the
  Worker passes requests through, and pgx.dev serves what it served before.
  The domain itself is never moved, released or deleted.

## End of the transition (no later than 2027-02-17 19:58 UTC)

The redirect ends by itself. Then remove the Worker and its route:

```sh
npx wrangler delete pgx-dev-redirect
curl -sSI https://pgx.dev/ | head -1          # no longer a 301
```

## Change or roll back

```sh
node --test redirect-worker.test.js
npx wrangler deploy                            # from this directory
npx wrangler delete pgx-dev-redirect           # rollback: pgx.dev is as before
```

Never set `REDIRECT_UNTIL` later than 2027-02-17T19:58:36Z.
