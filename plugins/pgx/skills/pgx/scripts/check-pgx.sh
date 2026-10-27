#!/usr/bin/env bash
# check-pgx.sh: is a compatible PGX on PATH? Prints one JSON object and exits
#   0  compatible (version >= MIN)        {"ok":true,"version":"0.1.2","path":"…"}
#   3  pgx not found                      {"ok":false,"reason":"missing",…}
#   2  too old                            {"ok":false,"reason":"too_old","version":"0.1.1",…}
#   4  unreadable version output          {"ok":false,"reason":"malformed",…}
# `pgx agent --help` exits 0 on every PGX version (global help), so only the
# structured `pgx --json version` output counts.
set -u
MIN=${PGX_MIN_VERSION:-0.1.2}
esc() { printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g'; }
path=$(command -v pgx 2>/dev/null || true)
if [ -z "$path" ]; then
  printf '{"ok":false,"reason":"missing","min":"%s","hint":"install PGX: npm i -g pgxdb@%s (Node 18+), or run the installer from https://pgx.dev/install in your own shell"}\n' "$MIN" "$MIN"; exit 3
fi
out=$(pgx --json version 2>/dev/null || true)
ver=$(printf '%s' "$out" | sed -nE 's/.*"pgx_version":"([0-9]+\.[0-9]+\.[0-9]+)[^"]*".*/\1/p' | head -1)
if [ -z "$ver" ]; then
  printf '{"ok":false,"reason":"malformed","path":"%s","min":"%s","hint":"pgx --json version did not report pgx_version; reinstall PGX"}\n' "$(esc "$path")" "$MIN"; exit 4
fi
# semver compare on major.minor.patch
vcmp() { # returns 0 if $1 >= $2
  local a b; IFS=. read -r a1 a2 a3 <<<"$1"; IFS=. read -r b1 b2 b3 <<<"$2"
  [ "$a1" -gt "$b1" ] && return 0; [ "$a1" -lt "$b1" ] && return 1
  [ "$a2" -gt "$b2" ] && return 0; [ "$a2" -lt "$b2" ] && return 1
  [ "$a3" -ge "$b3" ]
}
if vcmp "$ver" "$MIN"; then
  printf '{"ok":true,"version":"%s","path":"%s","min":"%s"}\n' "$ver" "$(esc "$path")" "$MIN"; exit 0
fi
# Too old. Say what the installed version lacks: 0.1.1 isolated sessions but
# let Claude Code subagents share the parent's branch; anything older has no
# `pgx agent` at all. Either way: upgrade, do not run database work.
if vcmp "$ver" "0.1.1"; then
  why="PGX $ver isolates sessions only: Claude Code subagents would share the parent's branch"
else
  why="PGX $ver has no pgx agent"
fi
printf '{"ok":false,"reason":"too_old","version":"%s","path":"%s","min":"%s","hint":"%s; upgrade to %s or newer with the method it was installed with (npm i -g pgxdb@%s, or the installer again)"}\n' "$ver" "$(esc "$path")" "$MIN" "$why" "$MIN" "$MIN"; exit 2
