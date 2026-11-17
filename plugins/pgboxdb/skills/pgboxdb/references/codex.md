# Codex (CLI, IDE extension)

Status: **wrapper verified by tests; native hooks implemented against
Codex's documented payloads, not yet validated on a logged-in Codex.**

## What Codex can and cannot do

Codex has `SessionStart`, `UserPromptSubmit`, `SessionEnd` hooks (0.124+,
`SessionEnd` since 0.145) keyed by `session_id`, but **no hook can set
environment variables** for the shell commands it runs. Shell commands do get
`CODEX_SESSION_ID` and `CODEX_THREAD_ID`, and Codex inherits the environment
it was started with.

## The two ways in

1. **Wrapper (automatic within the run):**
   ```sh
   pgboxdb agent exec -- codex                 # interactive
   pgboxdb agent exec -- codex exec "add a users table and run the tests"
   ```
   The branch is in Codex's environment, so in every command it runs. It is
   dropped when Codex exits (`--keep` retires it with the grace period).
2. **Inside a Codex session started any other way:** run database work as
   `pgboxdb agent exec -- <command>`. It attaches to the session
   (`CODEX_SESSION_ID`), creating the branch on first use and keeping it for
   the whole session. The SessionStart hook (installed by `pgboxdb agent setup`)
   creates the branch up front and tells the model this in its context.

Never rewrite commands or inject the URL through other hooks; that is not a
documented Codex contract.

## Sandbox

In `workspace-write` the Codex sandbox has **no network at all, loopback
included** (Seatbelt on macOS, a private network namespace plus seccomp on
Linux), so the TCP `DATABASE_URL` fails and commands see
`CODEX_SANDBOX_NETWORK_DISABLED=1`. The project needs
`sandbox_workspace_write.network_access = true` in `.codex/config.toml` (or
`-c sandbox_workspace_write.network_access=true`). Tell the user, show the
line, and let them decide. Never change sandbox settings yourself.
`codex exec` defaults to read-only; database work needs `--sandbox
workspace-write` or `--full-auto` as the user prefers.

## Hooks and trust

`.codex/hooks.json` is read for trusted projects and must be trust-pinned
once (`/hooks` in the Codex TUI, or `--dangerously-bypass-hook-trust` for a
single run). The hook answers `SessionStart` with
`hookSpecificOutput.additionalContext`.
