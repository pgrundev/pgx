# pgboxdb.com site content (prepared, not deployed)

https://pgboxdb.com is not served yet: DNS and hosting are separate steps
(reports/PGBOXDB_RELEASE_CHECKLIST.md). The site that served the product
before the rename kept its source outside this repository; only the
`/install` Worker next to this directory lives here. These two files are the
pages for https://pgboxdb.com (documentation: https://pgbox.dev/db), ready to
upload wherever the site is hosted:

- `index.html`: the homepage. Keeps the existing look (dark, JetBrains Mono,
  Archivo Black, the interactive ASCII "PGBoxDB" hero, now in plain JavaScript
  without the design-component wrapper) and adds the headline *Every AI agent
  gets its own Postgres.*, the subtitle *Install one Skill. Your coding agent
  handles the database.*, both install commands, the six feature points, the
  host matrix and links.
- `skill.html`: install, set up and use the Skill with Claude Code, Cursor and
  Codex; safety rules; links.

Both are self-contained (Google Fonts only). The skills.sh link is present as
an HTML comment in each file; enable it once https://skills.sh/pgrundev/pgboxdb
lists the skill. Deploying is a manual step for the site owner; nothing here
claims the live site has changed.
