# Resource usage

How much memory and CPU Citadel uses in a real setup: the background daemon syncing a real docs site, an agent querying through `citadel mcp`, and the Web UI.

> **Measured on:** Apple M1 Pro (10 cores, 16 GB), macOS 27.2.1, `citadel 0.1.0` (commit `a327178`) installed with `cargo install`, daemon running under launchd (`citadel daemon install`). Date: 2026-11-22. One machine, one network; treat the numbers as a guide, not a guarantee.

## Summary

| Process | Situation | Memory (footprint) | CPU |
|---|---|---|---|
| `citadel daemon` | Idle, empty vault | **4.6 MB** | 0.1 % average |
| `citadel daemon` | Syncing 500 pages of dlthub docs (cold) | **44 MB** median, **114 MB** peak | 6.7 CPU-s over 64 s (10 % of one core on average, about one core at peak) |
| `citadel daemon` | Re-syncing the same Source (warm, 136 new pages) | 59 MB median, 119 MB peak | 5.7 CPU-s over 58 s |
| `citadel daemon` | Two Sources syncing at once | 59 MB median, 108 MB peak | 7.4 CPU-s over 62 s |
| `citadel daemon` | Idle after Syncs, 854 pages in the vault | **28–36 MB** (RSS fell to 25 MB) | 0.1 % average |
| `citadel mcp` | 1,000 FTS5 queries from an agent | 3.7 MB at start, **7.6 MB** peak | 2.7 ms CPU per query; median search 6.7 ms |
| `citadel ui` | 300 page reads + 48 searches | 4.1 MB at start, **9.2 MB** peak | 0.3 CPU-s for the whole burst |

In short: the daemon uses almost no CPU between Syncs. A Sync of a 500-page site takes about a minute, uses about 6 seconds of CPU in total, and briefly uses 100–120 MB. Afterwards the daemon settles at a 28–36 MB footprint, not back at its starting 4.6 MB.

**Against the SPEC budget** (idle daemon RSS under ~25 MB on a 1,000-page vault): with 854 pages, RSS reached 25 MB only about 10 minutes after the last Sync. For the first minutes it was 80–90 MB, and the footprint stayed above 25 MB throughout.

## What the columns mean

- **Footprint** is the memory macOS charges to the process: the "Memory" column in Activity Monitor, `MEM` in `top`. This is the number to compare with other apps.
- **RSS** (resident set size, from `ps`) is also reported below. It counts what is in physical RAM right now, including shared code (about 13 MB of the binary's `__TEXT` here) and memory the allocator has freed but macOS has not reclaimed yet. It leaves out memory macOS has compressed, so it can be higher or lower than the footprint.
- **CPU-s** is the CPU time the process used (from `ps`'s `TIME` column), measured from start to end of the window. "10 % average" means 10 % of one core over the window. The `ps %CPU` column was not used, because it is a decaying average that blurs short bursts.
- **Peak CPU** is the highest CPU use over any span of three sample gaps (about 5 s); 100 % means one core was busy.

## Scenarios and results

Each sample takes about 1.5 s. The raw CSVs were summarized per window. "Peak" is the highest sample, so a shorter spike between samples would be missed. "Settled" means the last minutes of a window with no Sync running.

### 1. Idle daemon, empty vault

The daemon was restarted (`launchctl kickstart -k`) and sampled for 8 minutes with no Sources.

| Footprint | RSS | CPU | Threads |
|---|---|---|---|
| 4.6 MB (flat) | 9.1 MB median | 0.48 CPU-s in 480 s (0.10 %) | 12 |

### 2. Cold Sync of a real docs site

`citadel add https://dlthub.com/docs --name dlthub --no-sync` with the defaults (`max_pages` 500, `max_depth` 5, 24 h interval). `--no-sync` leaves the first Sync to the daemon, which picked it up on its next scheduler tick.

| Measure | Value |
|---|---|
| Pages added | 500 (the `max_pages` limit), 802,286 words, 0 errors |
| Wall time | 64 s |
| CPU | 6.65 CPU-s (about 10 % average, peak 91 %) |
| Footprint | 5.5 MB → median 44 MB → peak 114 MB |
| RSS | peak 162 MB |
| Threads | up to 28 |
| After 5 min | footprint 27 MB, RSS 82.5 MB, 24 threads, 0.14 % CPU |
| Vault growth | about 20 MB of live database pages for 500 pages (7.3 MB of Markdown, 6,567 sections, plus FTS5 indexes) |

### 3. Warm re-Sync (same Source)

The Source's interval was set to 60 s so the daemon would run it again, then set back to 24 h.

| Measure | Value |
|---|---|
| Result | 364 pages unchanged, 136 added (see the note on `max_pages` below) |
| Wall time | 58 s |
| CPU | 5.65 CPU-s (9.4 % average, peak 75 %) |
| Footprint | 27 MB → median 59 MB during the Sync → peak 119 MB in the minute after it finished → back to 27 MB about two minutes after the end |
| RSS | peak 154 MB |

This run was not a pure no-change pass: 136 of its 500 pages were new. It cost about 85 % of the cold Sync's CPU. That fits the code: Citadel stores each page's `ETag` and `Last-Modified` but does not send them back on the next Sync, so every page is downloaded and converted again before its hash is compared.

### 4. Two Sources syncing at once

dlthub (warm) and the uv docs (`https://docs.astral.sh/uv/`, cold, 139 pages) were made due on the same tick. The daemon runs at most two Syncs at a time; the random start delay (0–30 s) put the second start 18 s after the first, and they overlapped for about 27 s.

| Measure | Value |
|---|---|
| CPU | 7.44 CPU-s over 62 s (12 % average, peak 72 %) |
| Footprint | median 59 MB, peak 108 MB, 28 MB one minute later |
| RSS | peak 185 MB |

The peak did not add up across Sources; it stayed near the single-Sync peak.

### 5. Settled idle with data

10 minutes of sampling after scenario 4, with 854 pages (1.49 million words) in a 40.5 MB `vault.db`.

| Measure | Value |
|---|---|
| CPU | 0.67 CPU-s in 610 s (0.11 %), peak 1 % |
| Footprint | 28 MB for the first 5 minutes, then 36 MB until the end |
| RSS | 173 MB → 88 MB → 46 MB → 25 MB |
| Threads | 24–25 |

The footprint rose by 8 MB while RSS went down, with no Sync running and almost no CPU. The cause of the rise was not established. At the end, `footprint -p <pid>` showed 31 MB of heap (`Malloc Small`) still held, and `top` reported 30 MB of the process's memory as compressed by macOS (`CMPRS`). Compressed memory counts toward the footprint but not toward RSS, which explains why RSS fell while the footprint did not.

### 6. Agent queries through `citadel mcp` (stdio)

A script started `citadel mcp`, sent `initialize`, then ran the agent skill's two-step recipe 500 times: an FTS5 section search (`LIMIT 15`) followed by fetching the bodies of the top three sections. 25 search terms × 20 rounds, 1,000 `query` calls in total, against the 854-page vault. It ran while the daemon sat idle in scenario 5; they are separate processes.

| Measure | Value |
|---|---|
| Footprint | 3.7 MB at start, 7.6 MB peak |
| RSS | 15.5 MB peak |
| CPU | 2.73 CPU-s for 1,000 calls (about 2.7 ms per call) |
| Latency, search step | median 6.7 ms, p95 21.8 ms |
| Latency, fetch step | median 3.0 ms, p95 13.8 ms |
| Errors | 0 |

Two shorter runs (100 calls each) gave the same footprint (7.6–7.7 MB) and median latency (6.1–6.4 ms).

### 7. Web UI (`citadel ui`)

`citadel ui --no-open` loaded the index page, the Sources list, 300 page detail requests and 48 searches through `/api/query`.

| Measure | Value |
|---|---|
| Footprint | 4.1 MB at start, 9.2 MB peak |
| RSS | 17.8 MB peak |
| CPU | 0.33 CPU-s for the 5-second burst |

`citadel serve` (daemon, UI and MCP endpoint in one process) was not measured.

## Notes and limits

- **Sync time depends on the network.** Citadel spends most of a Sync waiting on HTTP, so a 64 s Sync used only about 6.7 s of CPU. A slower site or network takes longer but does not use much more CPU.
- **Memory does not return to the starting level after the first Sync.** The daemon went from 4.6 MB to about 27 MB and stayed there, and its thread count stayed at 24 instead of 12. After each of the two later Syncs it came back to the same 27–28 MB, so in these runs it did not grow with each Sync. About 31 MB of heap stayed allocated while idle, and macOS compressed most of it. A multi-day run would show whether it stays flat over many Syncs.
- **`max_pages` limits each Sync, not the Source.** When a crawl stops at the limit, pages it did not visit are kept (they are not marked stale without `--prune`). Each warm Sync visited a slightly different set of URLs, so dlthub grew from 500 to 636 to 715 pages over three Syncs. Memory and CPU per Sync stayed about the same, but the vault and its FTS5 indexes keep growing.
- **Not measured:** `citadel serve`, `--render-js` (headless Chromium), local Markdown folder watching, Linux, and the Apple `container` image. The idle baseline was sampled once, not repeated.

## Reproduce

The sampler records RSS, `ps %CPU`, CPU time, thread count and `top` footprint about once per second:

```zsh
#!/bin/zsh
# usage: sample.sh PID OUTFILE SAMPLES
pid=$1; out=$2; n=$3
echo "epoch,rss_kb,cpu_pct,cputime,threads,footprint" > $out
for i in $(seq 1 $n); do
  line=$(ps -o rss=,%cpu=,time= -p $pid) || break
  th=$(ps -M -p $pid | tail -n +2 | wc -l | tr -d ' ')
  fp=$(top -l 1 -pid $pid -stats mem | tail -1 | tr -d ' ')
  echo "$(date +%s),$(echo $line | awk '{print $1","$2","$3}'),$th,$fp" >> $out
  sleep 1
done
```

```zsh
PID=$(pgrep -f 'citadel daemon start')
./sample.sh $PID cold-sync.csv 600 &
citadel add https://dlthub.com/docs --name dlthub --no-sync   # the daemon syncs it on its next tick
citadel show dlthub                                            # wait for Status: ok
```

CPU-s for a window is the last CPU time minus the first. Peak CPU is the CPU-time change over three sample gaps divided by the elapsed seconds. The footprint values come straight from `top`'s `MEM` column.
