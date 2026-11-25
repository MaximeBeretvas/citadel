# `citadel doctor` — design

> Size: small project. Status: approved 2026-11-25.

## Intent

One command that answers "is Citadel running, and is it set up right?". Today that takes `citadel daemon status`, `citadel update --check`, and reading several agent config files by hand. `doctor` puts all of it in one read-only report.

## Goals / non-goals / anti-goals

- **Goals:** in one report, show
  1. **Version:** whether the CLI is the latest release, and whether the running daemon runs the same version as the CLI.
  2. **Running:** whether the native daemon is running, the OS service is installed and loaded, how many `citadel daemon`/`citadel serve` processes run, and whether `serve` answers on `127.0.0.1:4317`.
  3. **Setup:** where the Citadel skill and MCP entry are configured for each agent (Claude Code, Antigravity, Codex) and scope (local = current directory, global = home), and with which transport.
- **Non-goals:**
  - **History** (past Syncs, failed Syncs, scheduler ticks, `daemon.log` errors). That is debugging and out of scope. Doctor shows only the current status.
  - **Fixing anything.** Doctor never writes files, starts processes or changes config.
  - Making `citadel serve` take the `daemon.pid` lock (see *Known gap*). That's a follow-up, not part of this work.
  - Web UI or `/api/doctor` endpoint.
- **Anti-goals:**
  - A report that says "OK" while the daemon runs an older binary than the CLI. Version mismatch must be detected exactly, not guessed.
  - A slow or hanging command. Every network or process probe has a short timeout.
  - Doctor changing state, even incidentally: it must not create the data directory, `vault.db` or `update-check.json`.

## Approaches

| | Approach | Trade-off |
|---|---|---|
| **1 (picked)** | New read-only `doctor` module reusing existing helpers (`daemon_status`, update check, service paths, setup path table). The daemon writes its version to a new `daemon.json` next to `daemon.pid` so doctor can compare versions. | Small daemon change; `daemon.pid` itself stays as it is, so older binaries keep working. |
| 2 | Same module, no daemon change: compare binary mtime with the daemon process start time. | Only a guess; wrong after `touch` or copy, and can't name the daemon's version. |
| 3 | Option 1 plus `/api/doctor` and a Web UI panel. | Also covers the container case from the browser, but it's UI work nobody asked for. |

Picked: **1**. It reports the actual status without guessing, for a few lines of daemon change.

## Design

### Command

```text
citadel doctor [--json]
```

- Plain text by default; `--json` prints one JSON object with the same checks.
- **Exit code:** `0` when no check fails (warnings allowed), `1` when any check fails.
- Doctor is a read-only command: it never shows the automatic "Update available" notice (it reports versions itself) and never spawns the background update refresh.

### Report (text shape)

```text
citadel doctor
  Version
    ✓ citadel 0.3.0 (latest: 0.3.0)
    ✓ daemon runs 0.3.0
  Running
    ✓ daemon running (pid 37556)
    ✓ launchd service installed and loaded (~/Library/LaunchAgents/com.citadel.daemon.plist)
    ✓ 1 daemon process, 0 serve processes
    · serve on 127.0.0.1:4317: not answering
  Setup
    ✓ Claude Code  global  skill  ~/.claude/skills/citadel → ~/.agents/skills/citadel
    ✓ Claude Code  global  MCP    ~/.claude.json  (stdio: citadel mcp)
    · Codex        global  not configured
    ! Claude Code  local   MCP    ./.mcp.json  (http: http://127.0.0.1:4317/mcp — not answering)
      → start it with `make run`, or re-run `citadel setup --transport stdio`
  Data
    · data dir ~/Library/Application Support/com.citadel.citadel/
```

Markers: `✓` ok, `·` info (not a problem), `!` warning, `✗` fail. Every `!`/`✗` line carries a one-line hint naming the command that fixes it. Doctor prints the hint but never runs it.

### Checks

**Version**
- *CLI vs latest:* live GitHub Releases check with a short timeout (≈3 s), reusing the `src/update/` code. Offline or on error: fall back to `update-check.json` and say "(cached, checked <date>)". No cache either: `·` "latest version unknown (offline)". Newer release available: `!` with hint `citadel update`. `CITADEL_NO_UPDATE_CHECK=1`: skip the live check, use the cache only.
- *Daemon vs CLI:* read the version from `daemon.json`. Different version: `!` with a restart hint. Without the service: `citadel daemon stop && citadel daemon start`. With the service: `launchctl kickstart -k gui/<uid>/com.citadel.daemon` or `systemctl --user restart citadel.service`, because `KeepAlive` would relaunch the service's own binary on its own. No `daemon.json` (daemon started by an older citadel): `!` "daemon started by an older citadel; restart it".
- *Stale build at the same version (chosen: catch it):* versions match, but the file at `daemon.json`'s `exe` path now has a newer modification time than `exe_mtime` → `!` "binary replaced since the daemon started; restart it". This catches a `cargo install` at the same version number without a build script. If the file is missing, doctor warns `!` "daemon's binary no longer exists".
- *Serve vs CLI (my call):* when `serve` answers on `:4317`, doctor sends MCP `initialize` (not `ping`) and compares `serverInfo.version` with the CLI. This covers the container default, where there is no daemon.
- *Stdio command vs CLI (my call):* when an MCP entry's stdio command resolves on `PATH` to a different file than the running doctor binary, doctor runs `<that> --version` (short timeout) and warns `!` on a mismatch.

**Running**
- *Daemon:* `daemon_status()` → `Running(pid)` ✓, `Stopped` `·` with hint `citadel daemon start`, `Stale` (pid file left behind, no lock holder) `!` with hint `citadel daemon start` (it clears the stale file).
- *OS service:* macOS: plist exists and `launchctl` reports it loaded. Linux: unit file exists and `systemctl --user is-active`. Not installed: `·` (optional). Installed but not loaded: `!`.
- *Service vs CLI (my call):* doctor parses the plist or unit for its `CITADEL_HOME` and binary path. If the data dir differs from the CLI's, it warns `!` "service uses data dir X; this CLI uses Y", so "daemon stopped" isn't misread. If the binary path differs from the doctor binary, it warns with hint `citadel daemon install` (which rewrites the service).
- *Process count:* list running `citadel daemon start` and `citadel serve` processes. `ps` can't see a process's `CITADEL_HOME`, and on Linux it also lists container processes, so the warnings say "may" (my call): more than one daemon gives `!` "N daemons running (may be different data dirs)", and a daemon plus a `serve` gives `!` "daemon and serve both running; if they share a vault, both run scheduled Syncs" (see *Known gap*). There is no per-OS reading of process environments.
- *Serve endpoint:* JSON-RPC `ping` POST to `http://127.0.0.1:4317/mcp`, same probe `citadel setup` uses, short timeout. Answering: ✓. Not answering: `·`, except `!` when an HTTP MCP entry points at it (see Setup).
- *Container:* if doctor itself runs inside a container (existing detection in `src/update/`), skip daemon and service checks and say "running in a container: `serve` is the daemon".

**Setup**: for each agent × scope in the `src/setup/agents.rs` path table:
- *Skill:* not present → `·` not configured; present → ✓ with its path, plus the symlink target where it's a symlink; dangling symlink → `✗`; installed `SKILL.md` differs from the one embedded in this binary → `!` "skill is outdated" with hint `citadel setup --only skill --force` (or `citadel skill install --global --force`).
- *MCP entry:* read the config file (`mcpServers.citadel` in JSON, `[mcp_servers.citadel]` in TOML) and show the transport: `stdio: <command> <args>` or `http: <url>`. Unparseable config file → `✗` with the parse error. Stdio command that doesn't resolve on `PATH` → `✗`. HTTP URL that doesn't answer the ping → `!`.
- Local scope uses the current directory, so the report says which directory it checked.

### Daemon change *(my call, after the builder check)*

- `daemon.pid` stays unchanged. The existing readers (`src/daemon/mod.rs:116,121`) parse the whole file as one integer, so adding a line would break older binaries, for example an old CLI unable to stop a new daemon.
- Instead, while the daemon holds its lock it writes a sibling file, `daemon.json`: `{ "pid", "version", "exe", "exe_mtime", "started_at" }`, where `exe_mtime` is the modification time of its own binary at startup. The daemon removes it together with `daemon.pid`. Doctor trusts it only when its `pid` matches the lock holder's pid; otherwise it treats the file as left behind.
- `daemon_status()` stays as is. A new reader returns `daemon.json` for doctor.

### Known gap (reported, not fixed here)

`citadel serve` (`src/serve.rs`) runs its own scheduler and watcher but never takes the `daemon.pid` lock. So a native `serve` and a native `daemon` on the same vault both run scheduled Syncs. The Sync claim stops them from syncing the same Source at once, but not from both running. Doctor flags it. Making `serve` take the lock is a separate follow-up.

## Constraints

- AGENTS.md invariants: `Result` plus `thiserror` in the module, `anyhow` at the CLI, no `unwrap`/`expect` outside tests; `--no-default-features` (lite) must compile and pass.
- Must not open `vault.db` read-write or create the data dir. Doctor's checks don't need the database.
- macOS and Linux only for the service and process checks. On Windows, those checks print `·` "not supported".
- TDD: each check is a pure function over injected inputs (paths, home dir, process list, probe results) so tests don't need a real daemon, network or `launchctl`.
- Docs: add `doctor` to `Docs/SPEC.md` §6 and to the README command list; add a V8 atomic task under `Docs/AtomicTasks/`.

## Left to the builder

- How to list processes (shell out to `ps -axo pid,command` vs a crate; prefer no new dependency).
- Exact timeouts, colors and alignment of the text output (follow existing CLI output style; no colors when stdout isn't a TTY).
- JSON field names (one entry per check: `section`, `name`, `status`, `detail`, `hint`).
- Whether to reuse `setup::build_plan` for paths or factor out the path table it uses.
