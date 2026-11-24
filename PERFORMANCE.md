# Performance findings — Citadel

Code review of `src/` at commit `2216364` (v0.2.0), 2026-11-23. Nothing was changed; this is a list of findings to pick from.

## TL;DR

- **A Sync is slow because it waits, not because it computes.** The crawler sleeps 100 ms before every page, so 500 pages take at least 50 s. The measured cold Sync took 64 s and used only 6.7 s of CPU.
- **A Sync uses 114 MB at peak, most likely because the whole crawl sits in RAM** until the last page arrives.
- **Multi-core is barely used for the heavy work.** The runtimes are multi-threaded, but page conversion, file reading and the watcher each run on one thread, one item at a time (see [Concurrency and multi-core](#concurrency-and-multi-core)). It only pays off after finding 1, because today a Sync mostly waits.
- **Queries are already fast** (median 6.7 ms through MCP). Query-path items below are low priority.
- **Start with findings 5, 7, 8 and 16**: small, safe, no behaviour change.
- **Biggest wins need a decision first**: findings 1 (crawl speed against politeness) and 2 (conditional GET).

## How to read this

Every finding has the same four lines: **Where**, **Cost**, **Fix**, **Watch out**.

| Tag | Meaning |
|---|---|
| 🟢 measured | A number backs it (from `Docs/resource-usage.md` or a test run for this review) |
| 🟡 read in code | The code clearly does it; the cost was not timed |
| 🔴 hypothesis | Plausible, needs a measurement before any work |

Effort: **S** = under an hour, **M** = half a day, **L** = a day or more, or a design decision.

> The baseline numbers come from `Docs/resource-usage.md`, measured on v0.1.0. Re-measure on the current build before and after each change (see [How to verify](#how-to-verify)).

## Summary

| # | Finding | Helps | Effort | Evidence |
|---|---|---|---|---|
| 1 | Crawl delay caps a Sync at 10 pages/s | Sync time | S + decision | 🟢 |
| 2 | Stored ETags are never sent back | Sync time, CPU, bandwidth | L | 🟢 |
| 3 | Whole crawl is held in memory | Peak memory | M | 🟢 |
| 4 | Every page is converted to Markdown twice | Sync CPU | M | 🟡 |
| 5 | Conversion and DB writes run on async workers | Responsiveness of `serve` | S | 🟡 |
| 6 | Each sitemap seed starts a new crawl (and a new Chrome) | Sync time with `render_js` | M | 🟡 |
| 7 | URL patterns are recompiled for every URL | Sync CPU | S | 🟡 |
| 8 | Warm Sync does unneeded database work per page | Sync CPU, disk writes | S–M | 🟡 |
| 9 | One file save re-reads a whole local folder | Local Sync CPU, memory | M–L | 🟡 |
| 10 | Every DB open re-runs the full schema script | All paths, idle CPU | S–M | 🟡 |
| 11 | UI handlers block async workers on SQLite | UI under load | S | 🟡 |
| 12 | Small columns sit behind `content_md` | Sources list, page list | S | 🟢 |
| 13 | Page list is unbounded; fallback search scans all bodies | UI on large Sources | S–M | 🟡 |
| 14 | `sync_runs` grows forever | Scheduler tick, DB size | S | 🟡 |
| 15 | Idle daemon stays above the 25 MB budget | Idle memory | M | 🔴 |
| 16 | No `[profile.release]` tuning | CPU, binary size | S | 🔴 |
| 17 | MCP stdio uses a full multi-thread runtime | MCP memory, threads | S | 🔴 |
| 18 | Static UI assets are copied per request, never cached | UI page loads | S | 🟡 |
| 19 | Content is stored and indexed several times | DB size, write cost | L | 🔴 |
| 20 | Page conversion uses one core | Sync time (after finding 1) | M | 🟡 |
| 21 | Local folder Sync reads and parses files one by one | Local Sync time | M | 🟡 |
| 22 | Watcher and `sync --all` handle one Source at a time | Many Sources | S–M | 🟡 |
| 23 | Small network steps run one after another | Sync start-up time | S | 🟡 |

---

## Tier 1 — Sync time and memory

These are where the measurements show real cost.

### 1. Crawl delay caps a Sync at 10 pages/s 🟢

- [ ] **Where:** `src/ingest/web.rs:25-26` (`DEFAULT_CONCURRENCY = 4`, `DEFAULT_CRAWL_DELAY_MS = 100`), applied at `web.rs:431-432`.
- **Cost:** spider sleeps the delay before it starts each page (`spider-2.54.2/src/website.rs:4977`). Four pages can still be in flight, but no more than 10 start per second. 500 pages = at least 50 s. Measured: 64 s for 500 pages.
- **Fix:** make the delay a per-Source setting, or lower the default (for example 25 ms, or 0 with the concurrency limit of 4 as the only brake).
- **Watch out:** the delay is politeness toward the docs site. This is a product decision, not a bug. A new `sources` column means updating `schema.sql` and `Docs/SPEC.md` together.

### 2. Stored ETags are never sent back 🟢

- [ ] **Where:** `web.rs:747-764` reads `ETag` and `Last-Modified`; `local.rs:364-379` stores them. Nothing sends `If-None-Match` or `If-Modified-Since`.
- **Cost:** every Sync downloads and converts every page again. The measured warm Sync cost about 85 % of the cold Sync's CPU.
- **Fix:** send the stored validators and treat `304 Not Modified` as "unchanged".
- **Watch out:** a 304 has no body, so the crawler finds no links on that page. Either store each page's outgoing links, or only use conditional requests for pages found through `sitemap.xml` / `llms.txt`. spider has `cache` and `etag_cache` features; I did not check how they behave. This is the largest change in the list.

### 3. Whole crawl is held in memory 🟢

- [ ] **Where:** `web.rs:540-555` collects every `Page` (raw HTML) into a `Vec`; `web.rs:343-394` keeps them until the crawl ends; `web.rs:659-731` then builds every Markdown document and its sections before the first database write. Local folders do the same at `local.rs:220-279`.
- **Cost:** peak 114 MB footprint for 500 pages (7.3 MB of final Markdown). Each page is in memory as HTML, then as Markdown, then again as sections.
- **Fix:** convert each page when it arrives, keep only the result and drop the HTML. A further step is to write pages in batches instead of one transaction at the end.
- **Watch out:** batched writes change the "all or nothing" behaviour of a Sync. A stalled `render_js` crawl currently keeps the previously stored pages (`web.rs:575-583`); keep that.

### 4. Every page is converted to Markdown twice 🟡

- [ ] **Where:** `web.rs:766-805` always runs `transform_content` twice (readability on, then off) and keeps one result. `web.rs:767` copies the full HTML (`get_html()`), and `web.rs:1122` makes another lowercase copy of it only to find `<title>`.
- **Cost:** two HTML parses and two full copies per page. Not timed separately.
- **Fix:** run the second conversion only when the first one looks weak (empty, no heading, very few words). Find the title with a case-insensitive search on the first few KB instead of lowercasing the whole document.
- **Watch out:** `should_prefer_fallback` (`web.rs:807-820`) compares the word counts of both results. Skipping the second pass changes which version some pages keep, so their hashes change once and show up as "updated".

### 5. Conversion and DB writes run on async workers 🟡

- [ ] **Where:** `web.rs:394` (`build_page_outcomes`, all HTML parsing) and `web.rs:296-327` (`open_rw_db`, `persist_sync_outcomes`) run inside the `async fn` without `spawn_blocking`.
- **Cost:** a Sync blocks a Tokio worker thread for the whole conversion and write. In `citadel serve` those workers also answer UI and MCP requests.
- **Fix:** wrap both in `tokio::task::spawn_blocking`, as the local path already does (`sources/mod.rs:260-275`).
- **Watch out:** none; behaviour stays the same.

---

## Tier 2 — Sync inefficiencies

### 6. Each sitemap seed starts a new crawl (and a new Chrome) 🟡

- [ ] **Where:** `web.rs:361-388`. Every seed the first crawl did not reach gets its own `Website` and runs one after another. With `render_js`, each of those launches a new Chrome (`web.rs:529-535`, `browser.rs:54`).
- **Cost:** per seed: a full crawler set-up (its own HTTP client and robots.txt handling; I did not check whether spider re-downloads robots.txt each time) and, with `render_js`, a browser start. Also `count_scoped_pages` (`web.rs:362`, `646-657`) re-parses every collected URL for every seed, before the cheap "already visited" check on line 367.
- **Fix:**
  1. Move the visited check above `count_scoped_pages` and keep a running count.
  2. Give the seeds to the first crawl with `Website::set_extra_links` (exists in spider 2.54.2) so there is one crawl.
  3. Launch one `RenderBrowser` per Sync and reuse it for seed crawls.
- **Watch out:** the invariant in `AGENTS.md` says one private Chrome per `render_js` crawl. One per Sync fits its intent, but confirm the wording before changing it.

### 7. URL patterns are recompiled for every URL 🟡

- [ ] **Where:** `web.rs:156` (`is_excluded`) and `web.rs:175` (`is_included`) call `PatternMatcher::new`, which compiles the glob patterns, on every call.
- **Cost:** both run for every seed and every page through `should_crawl` / `should_store`, and again inside `count_scoped_pages`. Only matters for Sources with include or exclude patterns.
- **Fix:** build the include-only and exclude-only matchers once in `WebScope::new`. The existing `matcher` field (`web.rs:100`) is only used by a no-op at line 294.
- **Watch out:** none.

### 8. Warm Sync does unneeded database work per page 🟡

- [ ] **Where:** `src/ingest/local.rs:282-528` (`persist_sync_outcomes`, used by web and local Syncs).
- **Cost and fix, four small items:**
  - [ ] `local.rs:351-359` loads the old `content_md` for every page, but it is only needed when the hash differs (`local.rs:441`). Select `id, content_hash, status` first and fetch the body only on change.
  - [ ] Statements are prepared again for every page (`tx.execute`, `tx.query_row`). Use `prepare_cached`, as `insert_sections` already does (`local.rs:536`).
  - [ ] Every unchanged page gets its own `UPDATE` (`local.rs:420-437`). For local Sources it only sets `last_checked_at`, so the ids can be collected and updated in one statement. Web pages also refresh their HTTP status and validators per page, so they keep the per-page update (use `prepare_cached` there).
  - [ ] Sections are split for every page before the hash is compared (`local.rs:267`, `web.rs:716`). Split only pages that are new or changed.
- **Watch out:** keep everything in the one transaction so FTS5 triggers stay consistent with `pages` / `sections`.

### 9. One file save re-reads a whole local folder 🟡

- [ ] **Where:** the watcher (`daemon/mod.rs:556-557`) calls a full Sync, which walks, reads, normalizes, hashes and splits every Markdown file (`local.rs:220-279`).
- **Cost:** grows with folder size, not with the size of the edit. Each run also adds a `sync_runs` row (see finding 14).
- **Fix:** store file size and modified time per page and skip files where both are unchanged. Or sync only the paths the watcher reported.
- **Watch out:** modified times are not fully reliable, so keep a full pass for manual and `--prune` Syncs. New columns mean `schema.sql` + `Docs/SPEC.md`.

---

## Tier 3 — Database access and idle cost

### 10. Every DB open re-runs the full schema script 🟡

- [ ] **Where:** `src/db/mod.rs:18-27`. `open_rw_db` runs four pragmas and the complete `schema.sql` on every call. It is called from about 30 places, including pure reads (`ingest/mod.rs:181`, `203`; `ui/api.rs:297`, `320`, `392`, `458`), the watcher every 5 s (`daemon/mod.rs:525`, `609`) and the scheduler every 60 s (`daemon/mod.rs:486`). One Sync opens at least six connections (`sources/mod.rs:184-264`, `322`).
- **Cost:** per call it is small (not timed), but it is paid on every request and on every idle tick.
- **Fix:** apply the schema once per process (for example behind a `OnceLock`, or guarded by `PRAGMA user_version`). Give read paths a plain read-only open.
- **Watch out:** MCP `query` must keep going through `src/db/readonly.rs`. Re-run the read-only safety tests after any `src/db/` change.

### 11. UI handlers block async workers on SQLite 🟡

- [ ] **Where:** `src/ui/api.rs:188`, `297`, `320`, `392`, `445`.
- **Cost:** `/api/query` can hold a worker for the full 5 s query timeout. The MCP path already avoids this (`mcp/mod.rs:282`).
- **Fix:** wrap the database work in `spawn_blocking`.
- **Watch out:** none.

### 12. Small columns sit behind `content_md` 🟢

- [ ] **Where:** `src/db/schema.sql` — in `pages`, `word_count`, `status` and the timestamps come after `content_md`. SQLite has to walk past the large body to reach them.
- **Cost (test run for this review):** 5,000 synthetic pages of 15 KB each, warm cache.

  | Query | Now | With fix |
  |---|---|---|
  | `v_sources` totals (Sources list, `citadel list`) | 22 ms | 0.8 ms |
  | Read one small column from every page | 21 ms | 7 ms* |

  \* "now" reads `last_checked_at` (stored after `content_md`); "with fix" reads `title` (stored before it). Same query shape, same plan.
- **Fix:** add `CREATE INDEX … ON pages(source_id, status, word_count)` so `v_sources` never touches the page bodies. This can replace `idx_pages_source_status`. For the page list, moving `content_md` to the end of the table is the full fix, but that needs a migration.
- **Watch out:** `schema.sql` and `Docs/SPEC.md` must change together. The absolute numbers are small; this matters for vaults with thousands of pages or a cold disk.

### 13. Page list is unbounded; fallback search scans all bodies 🟡

- [ ] **Where:** `ui/api.rs:485-546`. No `LIMIT`. The `LIKE` fallback (`query_pages_like`) runs `content_md LIKE '%term%'` over every page of the Source.
- **Fix:** add paging (needs a matching change in `src/ui/assets/app.js`). Restrict the fallback to `title` and `uri`.

### 14. `sync_runs` grows forever 🟡

- [ ] **Where:** rows are added at `local.rs:158-162` and never deleted. `daemon/mod.rs:327-328` runs `MAX(unixepoch(started_at))` over all runs of every Source on each 60 s tick.
- **Cost:** grows fastest for watched local folders: one row per burst of file saves.
- **Fix:** keep the last N runs per Source (the UI shows 10, `ingest/mod.rs:224`). Read the latest run with `ORDER BY started_at DESC LIMIT 1`, which the existing index serves directly.

### 15. Idle daemon stays above the 25 MB budget 🔴

- [ ] **Where:** `Docs/resource-usage.md` — footprint 28–36 MB after Syncs against a 25 MB SPEC target; threads go from 12 to 24 and stay there.
- **Likely causes, in the order to test:**
  1. The allocator keeps the heap from the Sync peak. Fixing finding 3 lowers the peak and should lower this too.
  2. The daemon runtime uses one worker thread per core (`daemon/mod.rs:157`). `worker_threads(2)` is enough for its workload.
  3. Threads and buffers kept by spider and its HTTP client after a crawl.
- **Watch out:** measure after finding 3 before trying anything else, such as another allocator.

---

## Tier 4 — Smaller items

### 16. No `[profile.release]` tuning 🔴

- [ ] **Where:** `Cargo.toml` has no release profile, so builds use the defaults: no LTO, 16 codegen units, symbols kept. The Dockerfile strips by hand; `release.yml:36` and `make install` do not.
- **Fix:** try `lto = "thin"`, `codegen-units = 1`, `strip = true`. Measure binary size and Sync CPU.
- **Watch out:** longer release builds. Do **not** add `panic = "abort"`: the code turns task panics into errors (`web.rs:585-587`, `sources/mod.rs:305-307`, `368-370`).

### 17. MCP stdio uses a full multi-thread runtime 🔴

- [ ] **Where:** `mcp/mod.rs:155`. The loop handles one request at a time.
- **Fix:** a `current_thread` runtime should be enough for `query`; check that `manage` → Sync still works on it. Measured cost today is already low (7.6 MB, 2.7 ms CPU per query), so this is optional.

### 18. Static UI assets are copied per request, never cached 🟡

- [ ] **Where:** `ui/mod.rs:93-106`. `file.data.into_owned()` copies the asset on each request; no `Cache-Control` or `ETag` header is set.
- **Fix:** send an `ETag` (rust-embed exposes a content hash) and answer `304` on a match.

### 19. Content is stored and indexed several times 🔴

- [ ] **Where:** `schema.sql`. A page body lives in `pages.content_md`, again in `sections.content_md`, and is indexed by both `pages_fts` and `sections_fts`.
- **Cost:** about 20 MB of database for 7.3 MB of Markdown (measured), and every write is indexed twice.
- **Fix:** a design question for a new ADR, for example dropping the body from `pages_fts`. Not a quick change.

---

## Concurrency and multi-core

**Short answer:** the async part is real, the multi-core part is mostly unused.

| Area | Today | Uses several cores? |
|---|---|---|
| Tokio runtimes (`daemon/mod.rs:157`, `mcp/mod.rs:155`, `cli/mod.rs:265`) | Multi-thread, one worker per core | ✅ available |
| Page downloads | Up to 4 in flight, but started at most 10 per second (finding 1) | ➖ network-bound |
| HTML → Markdown conversion | One loop, one page at a time, after the crawl ends | ❌ |
| Local folder read + parse | One loop, one file at a time | ❌ |
| Database writes | One transaction, one writer | ❌ by design (SQLite allows one writer) |
| Database reads (MCP over HTTP) | `spawn_blocking` per query, WAL allows parallel readers | ✅ |
| Database reads (UI API) | Run directly on async workers (finding 11) | ➖ blocks other requests |
| MCP over stdio | One request at a time; batch items in sequence (`mcp/mod.rs:123-136`) | ❌ fine for one agent |
| Daemon Syncs | 2 web Sources at once (`daemon/mod.rs:32`) | ✅ |
| Watcher, `sync --all` | One Source at a time | ❌ |

> **Keep in mind:** a cold Sync used 6.7 s of CPU in 64 s. Spreading that CPU over more cores saves a few seconds at most today. It becomes the main lever only once the crawl delay (finding 1) is lowered.

### 20. Page conversion uses one core 🟡

- [ ] **Where:** `web.rs:659-731` (`build_page_outcomes`): a single `for` loop converts, normalizes, hashes and splits every page.
- **Cost:** all CPU work of a Sync runs on one thread. Each page is independent, so this is the easiest work to spread.
- **Fix:** convert each page when it arrives, on the blocking pool, with a small limit (for example the core count, capped at 4). This is the same change as finding 3, so do them together: one change gives lower memory and multi-core.
- **Watch out:** keep the output order stable (`seen_uris` de-duplication at `web.rs:686` depends on order). A limit is needed so two Syncs do not take every core of the machine.

### 21. Local folder Sync reads and parses files one by one 🟡

- [ ] **Where:** `local.rs:220-279` (`collect_local_outcomes`).
- **Fix:** split the file list into chunks and process them on a few threads (`std::thread::scope` needs no new dependency), then write in one transaction as today.
- **Watch out:** finding 9 (skip unchanged files) removes most of this work. Do 9 first and measure before adding threads.

### 22. Watcher and `sync --all` handle one Source at a time 🟡

- [ ] **Where:**
  - Watcher: `daemon/mod.rs:545` and `556-557` await each Sync inside the event loop. While one folder syncs, events for other folders, the 5 s refresh and shutdown all wait.
  - `sync_all`: `sources/mod.rs:159-171` runs Sources in a plain loop.
- **Fix:** spawn watcher Syncs as tasks behind a semaphore, the way the scheduler already does (`daemon/mod.rs:364-409`). Run `sync_all` with a small concurrency limit.
- **Watch out:** exclusion stays with the Sync claim (`AlreadySyncing`), never an in-memory set. Parallel Syncs each hold their pages in memory until finding 3 is fixed. Database writes still queue behind SQLite's single writer (`busy_timeout` is 5 s), so keep the limit low.

### 23. Small network steps run one after another 🟡

- [ ] **Where:** `web.rs:850-886` fetches `llms.txt` and each sitemap in sequence, each with a 10 s timeout. `web.rs:606-624` re-checks suspicious pages one request at a time.
- **Fix:** run them together (`futures::join_all` or a `JoinSet`) with a small limit.
- **Watch out:** low gain unless a site is slow or has many child sitemaps.

### What should stay single-threaded

- **SQLite writes.** One writer is a SQLite rule. Parallel work should produce results in memory and hand them to one writer.
- **MCP over stdio.** One agent sends one request at a time; parallelism adds threads and no speed.
- **Read-only queries.** Each already runs on its own thread with a 5 s limit, inside `src/db/readonly.rs`.

---

## Suggested order

1. **Quick and safe (no behaviour change):** 5, 7, 8, 11, 16.
2. **Quick, small visible change:** 14 (older Sync runs disappear from history).
3. **Decide, then one line:** 1.
4. **Memory and multi-core together:** 3 + 20, then re-measure 15.
5. **Sync CPU:** 4, 6.
6. **Database:** 12, 10, 13.
7. **More concurrency:** 22, 23, then 21 if 9 is not done.
8. **Bigger designs:** 2, 9, 19.

## How to verify

- The repo has a measuring skill: `.agents/skills/measuring-citadel-footprint/` (`sample.sh`, `mcp_load.py`, `summarize.py`).
- Re-run scenarios 2 (cold Sync), 3 (warm Sync) and 5 (settled idle) from `Docs/resource-usage.md` before and after each change.
- For query changes, compare `elapsed_ms` from `citadel query`.
- The checks gate still applies: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, plus the `--no-default-features` build.

## Not reviewed

- `src/update/`, `src/setup/`, `src/skill/`, `src/cli/`, `src/daemon/service.rs`: start-up and one-off commands, skimmed only.
- `src/ui/assets/app.js`: checked only that it does not poll the API on a timer (it does not).
- `render_js` runtime cost: read in code, never measured (also missing from `Docs/resource-usage.md`).
- Test code.
