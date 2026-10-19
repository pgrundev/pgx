# pgx.dev homepage — deployment plan (NOT yet deployed)

This directory holds the pgx.dev landing page (`index.html`), a self-contained
static file: no build step, no external JS, fonts from Google Fonts, everything
else inline. It is **prepared for review only**. Deploying it, and any DNS
change, require explicit approval.

## What must be preserved

- The existing **`pgx.dev/install` Worker** (`../install-worker.js`, Worker name
  `pgx-install`, route `pgx.dev/install`, `INSTALL_REF=v0.1.0-beta.1`). None of
  the options below touch that route or that Worker.
- **DNS is not changed** by any option here. `pgx.dev` already resolves through
  Cloudflare; we only add/modify how the **root path** is served.

## Current state (verified 2026-10-19)

- `GET https://pgx.dev/` → `200 text/html` (a placeholder/parked page).
- `GET https://pgx.dev/install` → `200 text/x-shellscript` (the installer Worker).
- `GET https://pgx.dev/anything-else` → `404` from origin (not our Worker).

So deploying this homepage **replaces what the root path serves today**. That is
the one outward-facing change and is what needs approval.

## Recommended option — Cloudflare Pages (static hosting)

Best fit: it's a static file, Pages serves it from Cloudflare's edge with no
Worker code to maintain, and it coexists with the installer Worker (Worker
routes take precedence over Pages for their exact path, so `/install` keeps
working).

```sh
cd dist/pgx.dev/site
# one-time: create the Pages project (no custom domain yet — no DNS change)
npx wrangler@4.151.1 pages project create pgx-dev-site --production-branch main

# deploy the static directory (gives a *.pages.dev preview URL to review first)
npx wrangler@4.151.1 pages deploy . --project-name pgx-dev-site

# ONLY after approval: attach the apex domain so pgx.dev root serves this.
# This is the step that changes what pgx.dev/ shows; confirm the installer
# route still answers afterwards.
#   (done in the Cloudflare dashboard: Pages → pgx-dev-site → Custom domains →
#    add pgx.dev; Cloudflare manages the record within the existing zone.)
curl -fsSL https://pgx.dev/install | head -1   # must still be the installer
curl -fsSI https://pgx.dev/ | head -1          # must now be the homepage
```

The first `pages deploy` returns a `https://<hash>.pgx-dev-site.pages.dev`
preview URL — that is a safe, shareable preview that touches neither pgx.dev nor
DNS. Review there before attaching the apex domain.

## Alternative — a root Worker

If you'd rather keep everything as Workers: add a second Worker on route
`pgx.dev/` (exact path) that returns `index.html` inline, leaving the
`pgx.dev/install` Worker untouched. More code to maintain than Pages and only
covers `/` unless you add `pgx.dev/*` (which would then need to exclude
`/install`). Pages is simpler; this is noted only as a fallback.

## Guardrails

- Do **not** add a `pgx.dev/*` catch-all route on the installer Worker — it
  would shadow `/install`.
- Do **not** change nameservers or existing DNS records.
- Verify both `/` and `/install` after any deploy.
- Roll back: Pages keeps every deployment; promote a previous one, or remove the
  custom domain to restore the prior root.
