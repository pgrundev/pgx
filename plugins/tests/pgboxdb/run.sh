#!/usr/bin/env bash
# Tests for the PGBoxDB Claude Code plugin package: manifest and marketplace
# validation, no drift from the canonical skill, packaging hygiene, and a real
# install/enable/disable/update/uninstall cycle from a local marketplace in an
# isolated Claude configuration directory, and the same for the standalone
# repository tree that plugins/sync.sh --standalone assembles. Needs the `claude` CLI (2.1.3xx);
# no login, no network, no paid calls. Usage: plugins/tests/pgboxdb/run.sh
set -uo pipefail
repo=$(cd "$(dirname "$0")/../../.." && pwd)
plugin="$repo/plugins/pgboxdb"
fail=0
ok() { printf 'ok   %s\n' "$1"; }
bad() { printf 'FAIL %s\n' "$1"; fail=1; }
command -v claude >/dev/null 2>&1 || { echo "claude CLI not found (npm i -g @anthropic-ai/claude-code)"; exit 2; }
echo "claude $(claude --version 2>/dev/null | head -1)"
export DISABLE_TELEMETRY=1 DISABLE_AUTOUPDATER=1 CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1

# 1. The plugin copy of the skill is exactly the canonical skill.
"$repo/plugins/sync.sh" --check && ok "plugins/pgboxdb/skills/pgboxdb == skills/pgboxdb" || bad "plugin skill copy drifted from skills/pgboxdb (run plugins/sync.sh)"

# 2. Versions agree: plugin.json, the skill's metadata.version, and the entry carries none.
pv=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["version"])' "$plugin/.claude-plugin/plugin.json")
sv=$(grep -E '^  version: "' "$repo/skills/pgboxdb/SKILL.md" | head -1 | sed -E 's/.*"([^"]+)".*/\1/')
[ -n "$pv" ] && [ "$pv" = "$sv" ] && ok "plugin version $pv == skill metadata.version" || bad "plugin.json version '$pv' != SKILL.md metadata.version '$sv'"
python3 -I - "$repo/.claude-plugin/marketplace.json" <<'PY' && ok "marketplace entry: name pgboxdb, source ./plugins/pgboxdb, no version pin" || bad "marketplace entry shape"
import json,sys
m=json.load(open(sys.argv[1]))
assert m["name"]=="pgrundev" and m["owner"]["name"]
e=[p for p in m["plugins"] if p["name"]=="pgboxdb"]
assert len(e)==1 and e[0]["source"]=="./plugins/pgboxdb" and "version" not in e[0], e
PY
mn=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["name"])' "$plugin/.claude-plugin/plugin.json")
[ "$mn" = pgboxdb ] && ok "manifest name == entry name (pgboxdb)" || bad "manifest name '$mn' must equal the marketplace entry name"

# 3. Packaging hygiene: skill only, nothing executable at session start, nothing that escapes.
for d in hooks bin commands agents .mcp.json .lsp.json settings.json monitors workflows package.json; do
  [ -e "$plugin/$d" ] && bad "plugin must not ship $d (hooks come from pgboxdb agent setup)" || true
done
ok "no hooks/, bin/, MCP, LSP, settings or package.json in the plugin"
[ -f "$plugin/skills/pgboxdb/SKILL.md" ] && ok "skills/pgboxdb/SKILL.md present" || bad "skills/pgboxdb/SKILL.md missing"
[ -d "$plugin/skills/pgboxdb/.claude-plugin" ] && bad "the skill copy must not contain .claude-plugin" || ok "skill copy is a plain skill"
find "$plugin" "$repo/.claude-plugin" -type l | grep -q . && bad "symlinks in the plugin package" || ok "no symlinks"
grep -rn '/Users/\|/home/[a-z]' "$plugin" "$repo/.claude-plugin" >/dev/null && bad "absolute machine paths in the plugin package" || ok "no absolute machine paths"
grep -rn '\.\./' "$plugin/.claude-plugin" "$repo/.claude-plugin" >/dev/null && bad "parent-directory references in a manifest" || ok "no ../ in manifests"
grep -rnE 'postgres(ql)?://[^*$"<` ]+:[^*$"<` ]+@' "$plugin" | grep -v '\.\.\.' | grep -v '…' | grep -v ':\*\*\*@' >/dev/null && bad "credentialed URL in the plugin" || ok "no credentialed URLs"
grep -rnE '(sk-ant-|ghp_[A-Za-z0-9]{20}|AKIA[0-9A-Z]{16}|npm_[A-Za-z0-9]{30}|-----BEGIN [A-Z ]*PRIVATE KEY)' "$plugin" "$repo/.claude-plugin" >/dev/null && bad "secret-like token in the plugin" || ok "no secret-like tokens"
[ -x "$plugin/skills/pgboxdb/scripts/check-pgboxdb.sh" ] && [ -x "$plugin/skills/pgboxdb/scripts/detect-project.sh" ] && ok "helper scripts executable" || bad "helper scripts not executable"
words=$(wc -w < "$plugin/README.md"); [ "$words" -ge 40 ] && ok "plugin README ($words words)" || bad "plugin README too short for the directory ($words words)"
size=$(du -sk "$plugin" | cut -f1); [ "$size" -lt 1024 ] && ok "plugin package ${size} KB (< 1 MB)" || bad "plugin package too large: ${size} KB"
grep -q 'CLAUDE_PLUGIN_ROOT}/skills/pgboxdb/scripts/' "$plugin/skills/pgboxdb/SKILL.md" && ok "SKILL.md names the plugin script path (plugin root + skills/pgboxdb/scripts)" || bad "SKILL.md must name \${CLAUDE_PLUGIN_ROOT}/skills/pgboxdb/scripts/… for plugin installs"

# 4. The authoritative validators.
claude plugin validate --strict "$plugin" >/tmp/pv.out 2>&1 && ok "claude plugin validate --strict plugins/pgboxdb" || bad "plugin validate: $(tail -5 /tmp/pv.out)"
claude plugin validate --strict "$repo" >/tmp/mv.out 2>&1 && ok "claude plugin validate --strict . (marketplace)" || bad "marketplace validate: $(tail -5 /tmp/mv.out)"
claude plugin validate --profile directory "$plugin" >/tmp/dv.out 2>&1 && ok "directory submission profile: no refusals" || bad "directory profile: $(grep -E '✘|refuses' /tmp/dv.out | head -3)"

# 5. Install cycle from the repository as a local marketplace, in an isolated config dir.
tmp=$(mktemp -d "${TMPDIR:-/tmp}/pgbplug.XXXX")
export CLAUDE_CONFIG_DIR="$tmp/config"; mkdir -p "$CLAUDE_CONFIG_DIR" "$tmp/proj"
( cd "$tmp/proj" && git init -q . )
j() { python3 -I -c 'import json,sys; d=json.loads(sys.stdin.read().strip().splitlines()[-1]); print(eval(sys.argv[1],{"d":d}))' "$1"; }
out=$(claude plugin marketplace add "$repo" --json 2>&1); [ "$(printf '%s' "$out" | j 'd["outcome"]')" = ok ] && ok "marketplace add (directory source) -> pgrundev" || bad "marketplace add: $out"
out=$(cd "$tmp/proj" && claude plugin install pgboxdb@pgrundev --json 2>&1); [ "$(printf '%s' "$out" | j 'd["outcome"]')" = ok ] && ok "install pgboxdb@pgrundev (user scope)" || bad "install: $out"
lst=$(claude plugin list --json 2>/dev/null)
python3 -I - "$pv" <<PY && ok "list --json: pgboxdb@pgrundev enabled, version $pv" || bad "list --json: $lst"
import json,sys
l=json.loads('''$lst''')
e=[p for p in l if p["id"]=="pgboxdb@pgrundev"]
assert len(e)==1 and e[0]["enabled"] is True and e[0]["version"]==sys.argv[1], e
PY
det=$(claude plugin details pgboxdb 2>&1)
printf '%s' "$det" | grep -qE 'Skills \(1\) +pgboxdb' && printf '%s' "$det" | grep -q 'Hooks (0)' && printf '%s' "$det" | grep -q 'MCP servers (0)' && ok "details: Skills (1) pgboxdb, Hooks (0), MCP servers (0)" || bad "details: $det"
printf '%s' "$det" | grep -qE 'Always-on: +~[0-9]+ tok' && tok=$(printf '%s' "$det" | grep -oE 'Always-on: +~[0-9]+' | grep -oE '[0-9]+$') && [ "$tok" -lt 400 ] && ok "always-on context cost ~${tok} tokens (< 400)" || bad "always-on cost missing or too high: $(printf '%s' "$det" | grep Always-on)"
out=$(claude plugin disable pgboxdb@pgrundev --json 2>&1); [ "$(printf '%s' "$out" | j 'd["outcome"]')" = ok ] && ok "disable" || bad "disable: $out"
claude plugin list --json 2>/dev/null | python3 -I -c 'import json,sys; l=json.load(sys.stdin); assert [p for p in l if p["id"]=="pgboxdb@pgrundev"][0]["enabled"] is False' && ok "disabled state recorded" || bad "disable not recorded"
out=$(claude plugin enable pgboxdb@pgrundev --json 2>&1); [ "$(printf '%s' "$out" | j 'd["outcome"]')" = ok ] && ok "enable" || bad "enable: $out"
out=$(claude plugin update pgboxdb@pgrundev --json 2>&1); printf '%s' "$out" | grep -qE 'ok|latest' && ok "update (already latest)" || bad "update: $out"
# install at project and local scope too (the three scopes the dialog offers)
out=$(cd "$tmp/proj" && claude plugin install pgboxdb@pgrundev --scope project --json 2>&1); [ "$(printf '%s' "$out" | j 'd["outcome"]')" = ok ] && ok "install --scope project" || bad "project scope: $out"
( cd "$tmp/proj" && grep -q '"pgboxdb@pgrundev": true' .claude/settings.json ) && ok "project scope writes .claude/settings.json" || bad "project settings missing"
out=$(cd "$tmp/proj" && claude plugin install pgboxdb@pgrundev --scope local --json 2>&1); [ "$(printf '%s' "$out" | j 'd["outcome"]')" = ok ] && ok "install --scope local" || bad "local scope: $out"
( cd "$tmp/proj" && grep -q '"pgboxdb@pgrundev": true' .claude/settings.local.json ) && ok "local scope writes .claude/settings.local.json" || bad "local settings missing"
# a project that already has hooks (as pgboxdb agent setup writes) is not touched by install/uninstall
mkdir -p "$tmp/proj2/.claude"; ( cd "$tmp/proj2" && git init -q . )
cat > "$tmp/proj2/.claude/settings.json" <<'JSON'
{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"/bin/sh -c 'command -v pgboxdb >/dev/null 2>&1 && exec pgboxdb agent hook; exit 0'"}]}]}}
JSON
before=$(shasum "$tmp/proj2/.claude/settings.json")
( cd "$tmp/proj2" && claude plugin install pgboxdb@pgrundev --scope local --json >/dev/null 2>&1 && claude plugin uninstall pgboxdb@pgrundev --scope local --json >/dev/null 2>&1 )
[ "$before" = "$(shasum "$tmp/proj2/.claude/settings.json")" ] && ok "existing project hooks untouched by plugin install/uninstall" || bad "plugin install changed .claude/settings.json"
# coexistence with a Skills-CLI copy in the same project: a plain skill, not a skills-dir plugin
mkdir -p "$tmp/proj/.claude/skills"; cp -R "$repo/skills/pgboxdb" "$tmp/proj/.claude/skills/pgboxdb"
[ ! -e "$tmp/proj/.claude/skills/pgboxdb/.claude-plugin" ] && ok "Skills-CLI copy has no plugin manifest (stays /pgboxdb, no skills-dir plugin)" || bad "skills copy would load as a skills-dir plugin"
( cd "$tmp/proj" && claude plugin list 2>&1 | grep -q 'pgboxdb@pgrundev' ) && ok "plugin still listed with the standalone skill present" || bad "plugin list broke with standalone skill"
# cache contents: exactly the plugin package, under 1 MB
cache=$(claude plugin list --json 2>/dev/null | python3 -I -c 'import json,sys; print([p for p in json.load(sys.stdin) if p["id"]=="pgboxdb@pgrundev"][0]["installPath"])')
[ -f "$cache/skills/pgboxdb/SKILL.md" ] && ok "cache holds the skill ($cache)" || bad "cache missing skill at $cache"
find "$cache" -name '*.rs' -o -name Cargo.toml | grep -q . && bad "Rust sources in the plugin cache" || ok "no Rust sources in the installed plugin"
out=$(claude plugin uninstall pgboxdb@pgrundev --json 2>&1); [ "$(printf '%s' "$out" | j 'd["outcome"]')" = ok ] && ok "uninstall (user scope)" || bad "uninstall: $out"
out=$(cd "$tmp/proj" && claude plugin install pgboxdb@pgrundev --json 2>&1); [ "$(printf '%s' "$out" | j 'd["outcome"]')" = ok ] && ok "reinstall" || bad "reinstall: $out"
out=$(claude plugin marketplace remove pgrundev --json 2>&1); [ "$(printf '%s' "$out" | j 'd["outcome"]')" = ok ] && ok "marketplace remove" || bad "marketplace remove: $out"

# 6. The standalone plugin repository (pgrundev/pgboxdb-plugin) as plugins/sync.sh --standalone assembles it:
#    valid on its own, acceptable to the directory profile, a one-entry marketplace rooted at "./",
#    and installable from a clone of it (what `claude plugin marketplace add pgrundev/pgboxdb-plugin` does).
sa="$tmp/standalone"
"$repo/plugins/sync.sh" --standalone "$sa" >/dev/null 2>&1 && ok "sync.sh --standalone assembles" || bad "sync.sh --standalone failed"
"$repo/plugins/sync.sh" --check-standalone "$sa" >/dev/null 2>&1 && ok "sync.sh --check-standalone: fresh assembly has no drift" || bad "check-standalone reports drift on a fresh assembly"
echo "x" >> "$sa/README.md"; "$repo/plugins/sync.sh" --check-standalone "$sa" >/dev/null 2>&1 && bad "check-standalone missed a modified README" || ok "check-standalone detects drift"
"$repo/plugins/sync.sh" --standalone "$sa" >/dev/null 2>&1
for f in .claude-plugin/plugin.json .claude-plugin/marketplace.json skills/pgboxdb/SKILL.md README.md LICENSE .github/workflows/validate.yml; do [ -f "$sa/$f" ] || bad "standalone tree lacks $f"; done; ok "standalone tree: manifest, marketplace, skill, README, LICENSE, workflow"
diff -r "$plugin/skills/pgboxdb" "$sa/skills/pgboxdb" >/dev/null && cmp -s "$plugin/.claude-plugin/plugin.json" "$sa/.claude-plugin/plugin.json" && cmp -s "$repo/LICENSE" "$sa/LICENSE" && ok "standalone skill, manifest and LICENSE are byte-identical to the canonical ones" || bad "standalone tree differs from plugins/pgboxdb / LICENSE"
find "$sa" -type l | grep -q . && bad "symlinks in the standalone tree" || ok "standalone: no symlinks"
ssize=$(du -sk "$sa" | cut -f1); [ "$ssize" -lt 256 ] && ok "standalone tree ${ssize} KB (< 256 KB)" || bad "standalone tree too large: ${ssize} KB"
claude plugin validate --strict "$sa" >/tmp/sv.out 2>&1 && ok "claude plugin validate --strict (standalone root: plugin + marketplace)" || bad "standalone validate: $(tail -5 /tmp/sv.out)"
claude plugin validate --profile directory "$sa" >/tmp/sd.out 2>&1 && ok "standalone: directory profile, no refusals" || bad "standalone directory profile: $(grep -E '✘|refuses' /tmp/sd.out | head -3)"
grep -q 'treats such a folder as a plugin' /tmp/sd.out && ok "directory profile treats the standalone root as the plugin" || bad "directory profile did not treat the root as a plugin"
( cd "$sa" && git init -q . && git add -A && git -c user.name=t -c user.email=t@t commit -q -m init )
out=$(claude plugin marketplace add "$sa" --json 2>&1); [ "$(printf '%s' "$out" | j 'd["outcome"]')" = ok ] && ok "marketplace add (standalone clone) -> pgboxdb-plugin" || bad "standalone marketplace add: $out"
out=$(cd "$tmp/proj" && claude plugin install pgboxdb@pgboxdb-plugin --scope local --json 2>&1); [ "$(printf '%s' "$out" | j 'd["outcome"]')" = ok ] && ok "install pgboxdb@pgboxdb-plugin (local scope)" || bad "standalone install: $out"
det=$(cd "$tmp/proj" && claude plugin details pgboxdb 2>&1)
printf '%s' "$det" | grep -qE 'Skills \(1\) +pgboxdb' && printf '%s' "$det" | grep -q 'Hooks (0)' && printf '%s' "$det" | grep -q 'MCP servers (0)' && ok "standalone details: Skills (1) pgboxdb, Hooks (0), MCP servers (0)" || bad "standalone details: $det"
cache=$(cd "$tmp/proj" && claude plugin list --json 2>/dev/null | python3 -I -c 'import json,sys; print([p for p in json.load(sys.stdin) if p["id"]=="pgboxdb@pgboxdb-plugin"][0]["installPath"])')
[ -f "$cache/skills/pgboxdb/SKILL.md" ] && [ ! -e "$cache/.git" ] && ok "standalone cache holds the skill, no .git" || bad "standalone cache: $cache"
out=$(cd "$tmp/proj" && claude plugin uninstall pgboxdb@pgboxdb-plugin --scope local --json 2>&1); [ "$(printf '%s' "$out" | j 'd["outcome"]')" = ok ] && ok "uninstall pgboxdb@pgboxdb-plugin" || bad "standalone uninstall: $out"
out=$(claude plugin marketplace remove pgboxdb-plugin --json 2>&1); [ "$(printf '%s' "$out" | j 'd["outcome"]')" = ok ] && ok "marketplace remove pgboxdb-plugin" || bad "standalone marketplace remove: $out"
echo "scratch: $tmp"
[ "$fail" -eq 0 ] && echo "ALL PLUGIN TESTS PASSED" || { echo "PLUGIN TESTS FAILED"; exit 1; }
