#!/usr/bin/env bash
# Tests for scripts/check-pgboxdb.sh: missing, too old, current, malformed.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd); chk="$here/../../pgboxdb/scripts/check-pgboxdb.sh"
fail=0; ok() { echo "ok   $1"; }; bad() { echo "FAIL $1"; fail=1; }
mk() { # name version-json-or-garbage
  d=$(mktemp -d); printf '#!/bin/sh\ncase "$*" in *version*) printf %%s %s ;; *agent*--help*) echo "pgboxdb help"; exit 0 ;; esac\n' "'$2'" > "$d/pgboxdb"; chmod +x "$d/pgboxdb"; echo "$d"
}
run() { PATH="$1:/usr/bin:/bin" bash "$chk"; }
set +e
out=$(PATH=/usr/bin:/bin bash "$chk"); rc=$?; [ $rc = 3 ] && [[ $out == *'"reason":"missing"'* ]] && ok "missing → 3" || bad "missing: rc=$rc $out"
d=$(mk old '{"pgboxdb_version":"0.1.0","os":"darwin"}'); out=$(run "$d"); rc=$?; [ $rc = 2 ] && [[ $out == *'"too_old"'* ]] && [[ $out == *"first PGBoxDB release"* ]] && ok "0.1.0 → 2 (too old) even though agent --help exits 0" || bad "old: rc=$rc $out"
d=$(mk dev '{"pgboxdb_version":"0.1.3","os":"darwin"}'); out=$(run "$d"); rc=$?; [ $rc = 2 ] && [[ $out == *'"too_old"'* ]] && [[ $out == *'"min":"0.2.0"'* ]] && ok "0.1.3 (a build from before the first release) → 2" || bad "0.1.3: rc=$rc $out"
d=$(mk cur '{"pgboxdb_version":"0.2.0","os":"darwin"}'); out=$(run "$d"); rc=$?; [ $rc = 0 ] && [[ $out == *'"ok":true'* ]] && ok "0.2.0 → 0" || bad "current: rc=$rc $out"
d=$(mk env '{"pgboxdb_version":"0.1.1","os":"darwin"}'); out=$(PGBOXDB_MIN_VERSION=0.1.1 PATH="$d:/usr/bin:/bin" bash "$chk"); rc=$?; [ $rc = 0 ] && ok "PGBOXDB_MIN_VERSION override still works" || bad "override: rc=$rc $out"
d=$(mk new '{"pgboxdb_version":"0.3.1","os":"linux"}'); out=$(run "$d"); rc=$?; [ $rc = 0 ] && ok "0.3.1 → 0" || bad "newer: rc=$rc $out"
d=$(mk ci '{"pgboxdb_version":"0.0.0-ci.abc","os":"linux"}'); out=$(run "$d"); rc=$?; [ $rc = 2 ] && ok "0.0.0-ci → 2" || bad "ci build: rc=$rc $out"
d=$(mk bad 'not json at all'); out=$(run "$d"); rc=$?; [ $rc = 4 ] && [[ $out == *'"malformed"'* ]] && ok "garbage → 4 (malformed)" || bad "malformed: rc=$rc $out"
d=$(mk empty ''); out=$(run "$d"); rc=$?; [ $rc = 4 ] && ok "empty → 4" || bad "empty: rc=$rc $out"
for o in "$(PATH=/usr/bin:/bin bash "$chk")" "$(run "$(mk x '{"pgboxdb_version":"0.1.0"}')")"; do python3 -I -c 'import json,sys; json.loads(sys.argv[1])' "$o" && ok "output is JSON" || bad "not JSON: $o"; done
set -e
[ $fail = 0 ] && echo "CHECK-PGBoxDB TESTS PASSED" || { echo "CHECK-PGBoxDB TESTS FAILED"; exit 1; }
