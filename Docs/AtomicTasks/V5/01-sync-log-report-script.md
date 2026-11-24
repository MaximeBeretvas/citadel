# Task V5-01: `sync_log_report.py` splits a daemon log per Sync

- **Phase:** V5
- **Dependencies:** none (fixtures use the line formats of tasks 02 and 04 as given in the design)
- **Suggested Commit:** `feat(skill): add sync_log_report.py to split daemon logs per Sync`

## Goal
Running one command on a daemon log prints one row per web Sync: its duration, logged requests, time before the first and after the last `fetch` line, median gap and requests per second. Done when the 6 `unittest` cases pass.

## Exact Files
- `.agents/skills/measuring-citadel-footprint/scripts/sync_log_report.py` (new, standard library only):
  - **Arguments:** a log path, `--origin name=url` (repeatable, for old logs) and `--single` (the whole file is one Sync, as in the stderr of `citadel sync`).
  - **Reading:** streams the file line by line and reads it once.
  - **Opening and closing a Sync:** a Sync opens on `starting scheduled web sync source=<name>` (it reads `origin=<url>` when present) and closes on `scheduled web sync complete` or `scheduled web sync failed`.
  - **Attributing `fetch` lines:** a `INFO spider::utils: fetch <url>` line goes to the only open Sync. When two Syncs are open, it goes to the one whose Origin has the same host. When both share a host, both rows are marked "not separable". A line that fits no open Sync is counted as unattributed.
  - **Summary lines:** reads `web_sync_summary` lines (quoted text values, `-` for a step not reached) and prints their numbers next to the row.
  - **Change of pace (old logs only):** for a Sync without a summary line, reports a second part when, from some point on, the median gap over the rest of the Sync is more than twice the median gap before it.
  - **No requests:** prints "no fetch lines found" when there are none.
  - Line formats: design §Technical Plan item 1 (lines 155-173) and item 2 (lines 202-212).
- `.agents/skills/measuring-citadel-footprint/scripts/test_sync_log_report.py` (new): hand-written logs, one test each (red-first list below).
- `.agents/skills/measuring-citadel-footprint/SKILL.md`: a section on the script. It covers when to use it, how to capture stderr on a scratch vault, and the requests that leave no `fetch` line (discovery, spider's sitemap pass, the re-fetch after a 404 sitemap probe, redirect hops, `robots.txt`).

## Red-First Test Cases
1. Two overlapping Syncs on different hosts with `origin=` on their start lines are split correctly. One of them is closed by its failed line.
2. Two overlapping Syncs on one host are both marked "not separable".
3. An old-format log (no `origin=`, no summary line) run with `--origin` gets its rows, and a slow last part is reported.
4. `--single` on a log with no daemon lines gives one row.
5. A `fetch` line outside any open Sync is counted as unattributed.
6. A log with no `fetch` lines prints "no fetch lines found".

## Acceptance Criteria
- Run on the real `daemon.log` (lines 1-3439) with `--origin` for the five Sources, the script reproduces the request counts 139, 500, 500, 139, 500, 59, 1,003, 19 and 540.

## Checks Gate
```bash
python3 -m unittest discover -s .agents/skills/measuring-citadel-footprint/scripts -p 'test_*.py'
```
