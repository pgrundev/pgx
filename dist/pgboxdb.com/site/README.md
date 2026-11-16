# pgboxdb.com site content (prepared, not deployed)

The live https://pgboxdb.com is a single static page served through Cloudflare;
its source is not in this repository (only the `/install` Worker next to this
directory is). These two files are the skill-first replacement, ready to
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
