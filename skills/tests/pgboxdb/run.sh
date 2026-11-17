#!/usr/bin/env bash
# Tests for the pgboxdb Skill as a product: the Agent Skills validator, the
# frontmatter contract, size limits, and the project-detection script against
# fixture projects. No PGBoxDB runtime needed. Usage: skills/tests/pgboxdb/run.sh
# (Tests live outside skills/pgboxdb so an installed skill never carries them.)
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
skill=$(cd "$here/../../pgboxdb" && pwd)
fail=0
ok() { printf 'ok   %s\n' "$1"; }
bad() { printf 'FAIL %s\n' "$1"; fail=1; }

# 1. The official validator (skills-ref), via pipx or uvx when available.
if command -v pipx >/dev/null 2>&1; then
  if pipx run skills-ref validate "$skill" >/tmp/skills-ref.out 2>&1; then ok "skills-ref validate"; else bad "skills-ref validate: $(cat /tmp/skills-ref.out)"; fi
elif command -v uvx >/dev/null 2>&1; then
  if uvx skills-ref validate "$skill" >/tmp/skills-ref.out 2>&1; then ok "skills-ref validate"; else bad "skills-ref validate: $(cat /tmp/skills-ref.out)"; fi
else
  echo "skip skills-ref validate (no pipx/uvx)"
fi

# 2. Frontmatter: only the six spec fields, name matches the directory, sizes.
fm=$(awk 'NR==1 && $0!="---"{exit 1} NR>1 && $0=="---"{exit} NR>1{print}' "$skill/SKILL.md") || { bad "SKILL.md must start with --- frontmatter"; fm=""; }
keys=$(printf "%s\n" "$fm" | grep -E "^[a-z-]+:" | cut -d: -f1 | sort | tr "\n" " " || true)
for k in $keys; do
  case "$k" in name|description|license|compatibility|metadata|allowed-tools) ;; *) bad "unexpected frontmatter key: $k" ;; esac
done
grep -qE '^name: pgboxdb$' "$skill/SKILL.md" && ok "name: pgboxdb" || bad "name must be pgboxdb"
desc_len=$(python3 -I - "$skill/SKILL.md" <<'PY2'
import sys
lines=open(sys.argv[1]).read().split('\n')
assert lines[0]=='---'
fm=[]
for l in lines[1:]:
    if l=='---': break
    fm.append(l)
desc=[]; on=False
for l in fm:
    if l.startswith('description:'):
        on=True; rest=l.split(':',1)[1].strip()
        if rest and rest not in ('>','>-','|','|-'): desc.append(rest)
        continue
    if on:
        if l.startswith((' ','\t')): desc.append(l.strip())
        else: break
print(len(' '.join(desc)))
PY2
)
[ "$desc_len" -gt 100 ] && [ "$desc_len" -le 1024 ] && ok "description length $desc_len" || bad "description length $desc_len (want 100..1024)"
lines=$(wc -l < "$skill/SKILL.md"); [ "$lines" -lt 500 ] && ok "SKILL.md $lines lines (< 500)" || bad "SKILL.md too long: $lines"
words=$(wc -w < "$skill/SKILL.md"); [ "$words" -lt 3500 ] && ok "SKILL.md $words words (~< 5000 tokens)" || bad "SKILL.md too wordy: $words"
for ref in getting-started installation database-discovery postgres-import database-branching agent-lifecycle claude-code cursor codex troubleshooting security; do
  [ -f "$skill/references/$ref.md" ] && ok "references/$ref.md" || bad "missing references/$ref.md"
  grep -q "references/$ref.md" "$skill/SKILL.md" && : || bad "SKILL.md does not point at references/$ref.md"
done
# No secrets or absolute machine paths in the skill.
grep -rnE 'postgres://[^*$"<` ]+:[^*$"<` ]+@' "$skill/SKILL.md" "$skill/references" | grep -v '\.\.\.' | grep -v '…' | grep -v 'postgres://postgres:\*\*\*' >/dev/null && bad "a credentialed URL appears in the skill text" || ok "no credentialed URLs"
grep -rn '/Users/\|/home/alex' "$skill/SKILL.md" "$skill/references" "$skill/scripts" >/dev/null && bad "machine-specific path in the skill" || ok "no machine-specific paths"

# 3. detect-project.sh against fixtures. (git keeps no empty directories; the
#    "empty" fixture has only a README, which the script does not look at.)
det="$skill/scripts/detect-project.sh"
mkdir -p "$here/fixtures/empty"
[ -x "$det" ] || bad "scripts/detect-project.sh is not executable"
j() { PATH=/usr/bin:/bin "$det" "$here/fixtures/$1"; }
check() { # name jq-like python expr
  local out; out=$(j "$1") || { bad "detect $1: script failed"; return; }
  if python3 -I -c "import json,sys; d=json.loads(sys.argv[1]); assert ($2), d" "$out" 2>/dev/null; then ok "detect $1: $2"; else bad "detect $1: $2 -- got $out"; fi
}
check rails "d['framework']=='rails' and d['migrations']=='rails'"
check rails "d['database_config']['classification']=='local' and d['database_config']['host']=='localhost'"
check rails "'config/database.yml' in d['database_config']['files']"
check django "d['framework']=='django' and d['migrations']=='django'"
check django "d['database_config']['classification']=='local' and d['database_config']['db']=='app_dev'"
check prisma "d['framework']=='prisma' and d['migrations']=='prisma'"
check prisma "d['database_config']['classification']=='sensitive'"
check node "d['framework']=='node-pg' and d['database_config']['classification']=='local'"
check go "d['framework']=='go' and d['database_config']['classification']=='local'"
check empty "d['framework']=='unknown' and d['database_config']['classification']=='none'"
check sensitive "d['database_config']['classification']=='sensitive'"
check empty "d['pgboxdb']['installed']==False"
check remote-ssl "d['database_config']['classification']=='sensitive'"
grep -q '0.2.0 or newer' "$skill/SKILL.md" && ! grep -qE '0\.1\.[0-9] or newer' "$skill/SKILL.md" && ! grep -qE 'pgboxdb agent --help. must succeed' "$skill/SKILL.md" && grep -q 'MIN=${PGBOXDB_MIN_VERSION:-0.2.0}' "$skill/scripts/check-pgboxdb.sh" && ok "SKILL.md and check-pgboxdb.sh gate on PGBoxDB 0.2.0, not on --help" || bad "SKILL.md and check-pgboxdb.sh must gate on 0.2.0, the first PGBoxDB release (pgboxdb agent --help exits 0 on old binaries)"
grep -q 'scripts/detect-project.sh' "$skill/SKILL.md" && grep -qE '\.claude/skills/pgboxdb/scripts/detect-project\.sh|\.agents/skills/pgboxdb/scripts/detect-project\.sh' "$skill/SKILL.md" && ok "SKILL.md names the installed script paths" || bad "SKILL.md must name the installed script paths"
# Secrets never leak into the output.
for f in django prisma node sensitive remote-ssl; do
  out=$(j $f)
  case "$out" in *localsecret*|*pw@*|*hunter2*|*topsecret*|*s3cret*) bad "detect $f leaks a secret: $out" ;; *) ok "detect $f: no secret in output" ;; esac
done
# The output is one JSON object per run, nothing else on stdout.
j rails | python3 -I -c 'import json,sys; json.load(sys.stdin)' && ok "detect output is a single JSON document" || bad "detect output is not clean JSON"

# 4. The version-check script.
[ -x "$skill/scripts/check-pgboxdb.sh" ] && ok "scripts/check-pgboxdb.sh executable" || bad "scripts/check-pgboxdb.sh missing or not executable"
"$here/check-pgboxdb.test.sh" >/tmp/check-pgboxdb.out 2>&1 && ok "check-pgboxdb.sh: missing/old/current/malformed" || bad "check-pgboxdb.sh tests: $(tail -3 /tmp/check-pgboxdb.out)"
grep -q 'check-pgboxdb.sh' "$skill/SKILL.md" && ok "SKILL.md points at check-pgboxdb.sh" || bad "SKILL.md must use check-pgboxdb.sh"

[ "$fail" -eq 0 ] && echo "ALL SKILL TESTS PASSED" || { echo "SKILL TESTS FAILED"; exit 1; }
