---
name: measuring-citadel-footprint
description: Use when asked how much memory, RAM, or CPU citadel uses — the daemon idle or during a Sync, `citadel mcp` under agent queries, or `citadel ui` — or when re-measuring, benchmarking, or updating Docs/resource-usage.md, the README "Resource Usage" table, or the SPEC resource budget.
---

# Measuring Citadel's footprint

## Core principle

Measure **the process that does the work in real use**, on **a vault you can throw away**, and report numbers that mean what the table says.

- In real use, the long-running **daemon** runs scheduled Syncs. `citadel sync` and `citadel add` (without `--no-sync`) crawl inside the short-lived CLI process instead. Their numbers say nothing about the daemon's idle or retained memory.
- The user's vault (`~/Library/Application Support/com.citadel.citadel`) already holds Sources. Origins are unique, so re-adding a URL fails. Removing a Source deletes the user's data. So **never add, sync, remove, or restart against the user's vault or launchd daemon**. Run a second daemon on a scratch `--data-dir`.
- Sampling the launchd daemon by pid (`ps`/`top`/`footprint` only) is read-only and allowed. Label it "installed daemon, idle, N pages".

## Run: one daemon, one sampler, windows by timestamp

```zsh
SK=/abs/path/to/.agents/skills/measuring-citadel-footprint/scripts   # absolute: .agents/ is not in every worktree
T=$(mktemp -d "$TMPDIR/citadel-bench.XXXX")
citadel list                      # no STATUS "syncing": a launchd Sync during the run adds noise

citadel daemon start --foreground --data-dir "$T" 2> "$T/stderr.log" &   # foreground logs to stderr, not daemon.log
DPID=$!; sleep 5
zsh $SK/sample.sh $DPID "$T/d.csv" 4800 &   # N counts samples (~1.5 s each): 4800 ≈ 2 h of headroom; killed below
SPID=$!; T0=$(date +%s)

sleep 300; T1=$(date +%s)                                   # idle, empty vault
citadel --data-dir "$T" add <URL> --name x --no-sync        # due on the daemon's next 60 s tick (+0–30 s jitter)
until citadel --data-dir "$T" show x | grep -Eq '^Status: +(ok|error)'; do sleep 10; done   # output is padded
sqlite3 "$T/vault.db" "SELECT id, status, pages_added, pages_updated, pages_unchanged, pages_failed FROM sync_runs"
read S E <<< "$(sqlite3 -separator ' ' "$T/vault.db" \
  "SELECT strftime('%s',started_at), strftime('%s',finished_at) FROM sync_runs ORDER BY id DESC LIMIT 1")"
sleep $(( E + 720 - $(date +%s) )); footprint -p $DPID | grep -E 'Footprint|Malloc Small'

kill $SPID $DPID
python3 -I $SK/summarize.py "$T/d.csv" $((T0+30)) $T1           # idle, empty (skip startup)
python3 -I $SK/summarize.py "$T/d.csv" $S $((E+60))              # Sync (footprint can peak after finished_at)
python3 -I $SK/summarize.py "$T/d.csv" $((E+120)) $((E+720))     # settled idle with data
citadel --data-dir "$T" show x                                   # real page count
# run the citadel mcp / ui scenarios below now, while $T still holds data
rm -rf "$T"
```

- `--no-sync` with the default (24 h) interval counts as due on the first tick; this has been verified. **`--interval 0` means manual only**: the daemon never syncs it.
- Match the limits of the user's real Source (`citadel show <name>`: `max_pages`, `max_depth`, `render_js`) when comparing with their vault. Defaults are `max_pages` 500 and `max_depth` 5.
- To get the whole-life peak between samples, start the daemon as `/usr/bin/time -l citadel daemon start ...`, use `DPID=$(pgrep -P $!)`, and read "peak memory footprint" from `$T/stderr.log` after `kill`.

## Other scenarios

| Scenario | How to trigger it | Window |
|---|---|---|
| Warm Sync | add it with `--interval 1m`; the daemon re-runs it every minute. Stop after run 2. | run 2's `sync_runs` row, + 60 s |
| Two Sources at once | two `--no-sync` adds back to back (the daemon runs at most 2) | first start → last finish + 60 s |
| `citadel mcp` | `CITADEL_HOME="$T" python3 -I $SK/mcp_load.py $SK/sample.sh "$T/mcp.csv" 20`. Report footprint peak, CPU-s ÷ calls (ms of CPU per call), and the search median / p95 latency it prints. | the whole CSV; use ≥ 20 rounds, because 100 calls finish inside one sample |
| One-shot CLI / `citadel ui` request burst | `/usr/bin/time -l citadel ...`, or sample the `citadel ui --no-open --port P` pid while you `curl` it | the command, or the burst |

## Where a web Sync spends its time: `sync_log_report.py`

Use it to see, per web Sync, how long the Sync took, how many requests spider handed out, and how the time splits around them. It reads a log; it never syncs.

```zsh
python3 -I $SK/sync_log_report.py "$HOME/Library/Application Support/com.citadel.citadel/daemon.log"   # read-only
python3 -I $SK/sync_log_report.py old.log --origin dlthub=https://dlthub.com/docs   # start lines without origin=
python3 -I $SK/sync_log_report.py "$T/stderr.log"                                   # foreground daemon on a scratch vault
citadel --data-dir "$T" sync x 2> "$T/sync.log"; python3 -I $SK/sync_log_report.py --single "$T/sync.log"
```

- **One row per Sync**, from the daemon's start line to its complete or failed line. Each `INFO spider::utils: fetch <url>` line goes to the only open Sync, or to the open Sync whose Origin has the same host. Two overlapping Syncs on one host are marked "not separable", and their rows share those lines. Lines outside any Sync are counted as unattributed.
- **`web_sync_summary` lines** (written at the end of every web Sync) are printed under their row. They give the exact step durations, pages received, results and drop counts. Prefer them over the timings derived from `fetch` lines.
- **Change of pace:** for a Sync without a summary line, the script fits two parts to the gaps between requests and prints the split when the last part's median gap is more than twice the first's (at least 10 gaps per part).
- **To capture a log on a scratch vault,** run the foreground daemon as in "Run" above (`2> "$T/stderr.log"`), add Sources with `--no-sync`, and read `$T/stderr.log` once their Syncs finish.
- **Requests that leave no `fetch` line,** so the count is a floor: Citadel's own discovery requests (`sitemap.xml`, `llms.txt`); spider's sitemap pass at the end of each crawler; spider's re-request of the start URL after a 404 on `<start URL>/sitemap.xml`; redirect hops; `robots.txt` fetches.
- If spider changes its log format, the script prints "no fetch lines found".

## What each number means

| Report | Source | Do not use |
|---|---|---|
| **Memory = footprint** (Activity Monitor "Memory"). Headline: Sync median + peak, and settled median. | `top -stats mem` | RSS as the headline |
| RSS (secondary) | `ps -o rss=`: resident only. It includes about 13 MB of shared `__TEXT` and excludes compressed pages, so it can be **above or below** the footprint. | treating RSS as a subset of the footprint |
| Compressed | `top -stats cmprs`. Idle retained heap gets compressed, so RSS falls while the footprint stays flat or rises. | explaining an idle footprint change without checking this |
| **CPU-s** | last − first `ps` TIME in the window | `ps %CPU` (a decaying average) |
| avg CPU % | CPU-s ÷ wall s (100 % = one core) | |
| peak CPU % | CPU-s change over **≥ 3 sample gaps** ÷ elapsed seconds (`summarize.py`) | 1-gap peaks: whole-second epochs over about 1.5 s gaps overstate CPU by up to 60 % |
| Retained heap | `footprint -p $DPID` (look at `Malloc Small`) | |

## Report

1. **Environment header:** chip, cores, RAM (`sysctl -n machdep.cpu.brand_string hw.ncpu hw.memsize`), `sw_vers -productVersion`, `citadel --version`, and the installed commit from `grep citadel ~/.cargo/.crates2.json`.
2. **One row per scenario:** the process measured, the Source limits, footprint median / peak / settled, CPU-s, avg and peak %, pages added / updated / unchanged / failed, and wall time.
3. **Label each run by what it actually was.** A "warm" run with `pages_added > 0` is a mixed run. A scratch vault with 500 pages is not the user's 715-page vault.
4. **Not measured:** list every scenario you skipped, and say "single run" when you did not repeat it.
5. **Before you call something a cause**, check that a measurement shows it. Otherwise write "cause not established".
6. If `Docs/resource-usage.md` exists on the branch, update it, the README "Resource Usage" table and the SPEC lines that cite it **together**, so they agree. If it doesn't exist, ask which branch to write to.

## Common mistakes

| Mistake | Fix |
|---|---|
| Sampling `citadel sync` / `citadel add` as "the daemon" | Use `--no-sync` and let the daemon's tick sync it. |
| `citadel add <URL already in vault>` | Origins are unique: use a scratch `--data-dir`. |
| `grep 'Status: ok'` never matches | The output is padded: use `grep -E '^Status: +(ok\|error)'`. |
| `tail -f daemon.log \| grep -m1 ...` waits forever | Foreground logs go to stderr, and the line reads `scheduled web sync complete source=x`. Poll `show` or `sync_runs` instead. |
| `--interval 0` Source never syncs | 0 means manual only; keep the default or use `1m`. |
| Summary window ends at `finished_at` | Extend it by ≥ 60 s. |
| Relative `$SK` in a worktree | `.agents/` may not exist there: use an absolute path. |
| Reporting `max_pages` as the vault size | Unvisited pages are kept, so a Source grows past `max_pages` across warm Syncs. Report `show` page counts. |
| Leaving the bench daemon or sampler running | `kill $SPID $DPID`, then `rm -rf "$T"`. |
