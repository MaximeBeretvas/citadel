# Sync accounting: where a web Sync spends its time and which fetched pages it drops

*   **Author:** MaximeBeretvas
*   **Status:** Proposed
*   **Date:** 2026-11-24

--------------------------------------------------------------------------------

## Overview

Citadel crawls documentation websites with the `spider` crate, converts each page to Markdown and stores it in a local SQLite database, the vault. Large Sources take minutes to sync, and the cause differs per Source.

This design makes the cause visible and fixes one defect found while looking:

1. A script that turns a log into one row per Sync (G1).
2. Counters and timings inside Citadel for what the log cannot show (G2, G3).
3. A correct "cut off by the page budget" decision, so a cut-off Sync stops marking pages stale (G4).
4. A warning when a Sync spends its page budget on pages it then drops (G5).
5. The measured numbers on record (G6).

It does not make a Sync faster. It gives the speed work numbers per Source.

### Terms

The project glossary is `CONTEXT.md`. This document uses:

- **Source:** one registered documentation website and its settings. **Origin:** the URL that identifies a Source and where its first crawl starts.
- **Sync:** one run that fetches a Source's pages and stores them. The daemon runs at most two Syncs at a time on a schedule; `citadel sync` runs one from the command line.
- **Page budget:** a Source's `max_pages` setting, which applies to one Sync. **spider's limit:** the number of requests one spider crawler may hand out. Citadel derives it from the page budget.
- **Crawlable** and **storable:** Citadel checks a URL twice. *Crawlable* means a crawler may fetch it (include and exclude patterns, same site, not a static asset). *Storable* is the stricter check on a page's own URL before storing it.
- **Result:** what a Sync records for a page: stored, or failed with an error. **Dropped page:** a page a crawler fetched that gets no result. **Lost page:** a page spider fetched that never reached Citadel because Citadel's receiving task fell behind.
- **Complete Sync** and **cut-off Sync:** a cut-off Sync stopped because it used up its page budget; a complete Sync did not.
- **Stale page:** a stored page with no result in a complete Sync. It is kept, not deleted; its status changes from active to stale. **`--prune`:** an option of `citadel sync` that deletes such pages instead, even in a cut-off Sync.
- **Static crawl:** fetching pages with plain HTTP requests. Sources with `render_js` on are crawled through a headless Chrome instead and are out of scope here.

A web Sync has five steps:

1. **Discovery:** Citadel fetches the site's `sitemap.xml` and `llms.txt` itself and collects the URLs they list.
2. **First crawl:** one spider crawler starts at the Origin and follows links. spider's limit is the page budget.
3. **Later crawls:** for each discovered URL that no earlier crawl of this Sync collected, Citadel starts one more crawler, one after another, with spider's limit set to what is left of the budget.
4. **Conversion** of each kept page to Markdown.
5. **One database write.**

Citadel tells each crawler to wait 100 ms before handing out each request and to run at most 4 fetches at once, so a crawler hands out at most 10 requests per second. At the end of each crawler, spider also runs a sitemap pass of its own, separate from step 1.

## The Problem

### What the daemon log shows

spider writes one `fetch <url>` line with a timestamp each time it hands out a request: for each crawler's first page after its response arrives, and for every other page before the request is sent. The table covers the nine scheduled web Syncs in `~/Library/Application Support/com.citadel.citadel/daemon.log` as of 2026-11-24 (log lines 1 to 3439). The author's assistant split the lines by host; no Sync was run for this. All times are UTC.

| Source | Started (2026) | Duration | Logged requests | Before first line | First to last line | After last line | Median gap | Requests per second |
|---|---|---|---|---|---|---|---|---|
| `bench-uv` | 11-22 19:19 | 30 s | 139 | 0.7 s | 28.1 s | 0.9 s | 0.103 s | 5.0 |
| `dlthub` | 11-22 19:39 | 64 s | 500 | 4.3 s | 53.9 s | 5.8 s | 0.102 s | 9.3 |
| `dlthub` | 11-22 19:46 | 58 s | 500 | 2.1 s | 52.2 s | 4.0 s | 0.102 s | 9.6 |
| `bench-uv` | 11-22 19:50 | 27 s | 139 | 1.0 s | 24.7 s | 0.9 s | 0.103 s | 5.6 |
| `dlthub` | 11-22 19:50 | 59 s | 500 | 3.1 s | 52.3 s | 3.8 s | 0.102 s | 9.6 |
| `dbt-best-practices` | 11-23 20:45 | 7 s | 59 | 0.7 s | 6.1 s | 0.5 s | 0.104 s | 9.7 |
| `dlthub` | 11-23 20:50 | 192 s | 1,003 | 5.2 s | 164.6 s | 22.0 s | 0.104 s | 6.1 |
| `dbt-semantic-layer` | 11-23 20:59 | 3 s | 19 | 0.6 s | 1.9 s | 0.5 s | 0.104 s | 9.9 |
| `developer-cisco-com-meraki` | 11-24 13:39 | 186 s | 540 | 9.0 s | 173.5 s | 3.6 s | 0.254 s | 3.1 |

- "Duration" runs from the daemon's start line to its complete line. "Requests per second" is logged requests divided by "First to last line".
- "Before first line" includes discovery and the download of the first page. "After last line" includes requests still in flight, spider's sitemap pass, conversion and the database write.
- Two reviewers re-checked the request counts and the time columns against the log. The median gaps were computed by script only.
- No URL string appears twice in the `fetch` lines of any Sync. That does not prove no page was fetched twice: some requests write no line (see "What is still unknown").

### Four patterns

**Paced by the delay.** In five of the nine Syncs, requests are handed out about 0.10 s apart for the whole crawl, at 9.3 to 9.9 per second against a cap of 10.

**A slow last part.** Both `bench-uv` Syncs change pace part-way:

| Sync | First part | Last part |
|---|---|---|
| 11-22 19:19 | 82 requests in 8.3 s (9.9 per second) | 57 requests in 19.8 s (2.9 per second) |
| 11-22 19:50 | 82 requests in 8.3 s (9.8 per second) | 57 requests in 16.3 s (3.5 per second) |

The last part is 40 % of the requests and about 70 % of the crawl time. 55 of its 57 URLs are `…/index.md` pages listed in the site's `llms.txt`. The order and pace match step 3, one crawler per discovered URL, run one after another. That these are later crawls is inferred; no log line labels them. If so, their cost is setting up a crawler per page, not page downloads.

**Dropped pages.** In the `dlthub` Sync of 11-23, 1,003 requests were logged and 756 pages got a result. Of the 247 others, 246 are `/docs/pipelines/…` URLs and one is under `/docs/blog`. One was fetched by hand: it redirects to `https://dlthub.com/context/pipeline/…`, outside the Source, and Citadel drops such a page after downloading it (`src/ingest/web.rs:673-679`). Whether all 247 are redirects is not established. The dropped and the stored pipeline pages share their URL shapes: of 64 pipeline groups (`/docs/pipelines/<name>/`), 29 contain both. So no URL pattern separates them. What dropped pages cost for certain is page budget: up to 247 other pages were not reached. Whether they also cost time is not shown.

**Slow throughout.** `developer-cisco-com-meraki` runs at about 3.1 requests per second from start to end, with a median gap of 0.25 s and no change of pace. Slow responses keeping all 4 fetches busy would explain it; so would a Sync made mostly of later crawls. The cause is not identified.

### A defect: 197 pages wrongly marked stale

`sync_web_source` treats a Sync as cut off when the number of results reaches the page budget: `outcomes.len() >= source.max_pages` (`src/ingest/web.rs:308`). Dropped pages have no result, so they do not count.

The `dlthub` Sync of 11-23 had a page budget of 1,000. Its first crawl handed out 1,000 requests, which used up spider's limit. It had 756 results. 756 is under 1,000, so Citadel treated the Sync as complete and marked 197 stored pages stale. The daemon's complete line for that Sync reads `stale=197`.

This breaks the rule in `CONTEXT.md` ("A Sync cut short by its page limit leaves unvisited Pages as they were") and the lesson in `AGENTS.md` ("Preserve unvisited pages (`MissingPagePolicy::Preserve`) when a web crawl reaches `max_pages` without `--prune`"). The 197 pages are still stored; nothing shows that the site removed them.

The same Sync handed out 3 more requests in later crawls. A likely reason, inferred from the code and not observed: Citadel works out what is left of the budget by counting distinct storable URLs among the collected pages, and spells them without a trailing slash, so three pairs of URLs counted once each.

### What is still unknown

- **Which pattern each Source has, and why.** Only `bench-uv` shows the slow last part, only one `dlthub` Sync shows dropped pages, and the `developer-cisco-com-meraki` cause is open.
- **`aws-graffana`** (1,523 pages in 699 s, from the vault's record of that Sync) was synced from the command line, so it is not in this log.
- **How the time after the last request splits** between conversion and the database write: 4 to 6 s for 500 pages, 22 s in the one 1,003-request Sync.
- **Requests that write no `fetch` line:** Citadel's own discovery requests; spider's sitemap pass, which fetches sitemap URLs the crawler did not visit and delivers them as pages, with no delay; spider's sitemap probe, which on a 404 for `<start URL>/sitemap.xml` requests the start URL again to look for a sitemap link (spider `website.rs:12376-12387` and `13096-13156`, read in code), so each later crawl may fetch its page twice; redirect hops; `robots.txt` fetches.
- **Pages stored twice under two spellings.** The `dlthub` Source has 953 stored rows but 950 distinct URLs once a trailing slash is ignored. Not fixed here.

## Goals

*   **G1. One command turns a log into the per-Sync table above,** including the change of pace, and marks rows it cannot attribute with certainty.
*   **G2. Every web Sync with a valid Origin logs how long each step took,** how many later crawls it started, and the delay in force.
*   **G3. Every page a crawler delivered is accounted for.** Pages received equals results (stored plus failed) plus dropped pages, and each dropped page is counted under one of six reasons. Lost pages are counted separately.
*   **G4. A cut-off Sync does not mark stored pages stale,** unless run with `--prune`, whatever happened to the pages it received.
*   **G5. A cut-off Sync that spent much of its budget on pages that redirect out of the Source says so** in its Sync summary and in the daemon log.
*   **G6. The measured numbers are on record** in `Docs/resource-usage.md`, from Syncs on a scratch vault.

## Non-Goals

*   **No speed-up.** No pattern above is removed here.
*   **No replacement of the later crawls.** It is the first candidate for the next design; its reason is the slow last part.
*   **No change to the delay or to the 4 fetches at once.** The author chose to keep Citadel's rate in line with Firecrawl and crawl4ai (`concepts_brief.md`, "Direction from the author").
*   **No avoidance of requests that redirect out of the Source.** See Alternative C.
*   **No fix for pages stored twice under two spellings.**
*   **No change to `robots.txt` handling, conversion, the database write, the schema, dependencies, or `render_js` Sources.**

## Open Questions

*   **Q1. Measure `aws-graffana`?** One Sync on a scratch vault would show its pattern. It sends over 1,500 requests to `docs.aws.amazon.com`, whose `robots.txt` asks for 5 s between requests; Citadel does not apply that today. The author decides.
*   **Q2. Are the warning thresholds right?** A first guess: at least 5 redirected pages and more than 10 % of pages received. The author sets them after the measurement in G6.

## Critical User Journeys (CUJs)

### CUJ 1: See where Syncs spent their time

*   **Given**: a daemon log with several web Syncs, some of them overlapping.
*   **When**: the user runs the report script on it.
*   **Then**: one row per Sync with the columns of the table above; a second row for a slow last part when the Sync has no summary line; rows that could not be attributed with certainty are marked.

### CUJ 2: See why a Sync stored fewer pages than it fetched

*   **Given**: a Sync that drops pages.
*   **When**: it ends.
*   **Then**: its `web_sync_summary` log line gives pages received, results, the six drop counts, lost pages, the step durations, the delay in force, and whether the Sync was cut off.

### CUJ 3: A cut-off Sync leaves stored pages alone

*   **Given**: a Source with stored pages; a Sync without `--prune` that uses up its page budget and drops some of the pages it receives.
*   **When**: that Sync ends.
*   **Then**: no page is marked stale.

### CUJ 4: Learn that a Source spends its budget on redirects

*   **Given**: a Source whose Sync is cut off while many fetched pages redirect outside it.
*   **When**: the Sync ends.
*   **Then**: the Sync summary and the daemon log carry a warning that gives the count and up to three example URLs, says these pages count against the page budget, and suggests raising `max_pages` or excluding that section of the site if it is not wanted.

## Technical Plan (Architecture)

### 1. Report script (G1)

`sync_log_report.py` joins the scripts of the existing measuring skill, `.agents/skills/measuring-citadel-footprint/`. It reads three kinds of line:

```text
2026-11-22T19:19:56.024972Z  INFO citadel::daemon: starting scheduled web sync source=bench-uv
2026-11-23T20:52:57.284186Z  INFO spider::utils: fetch https://dlthub.com/docs/api_reference/dlt/pipeline/__init__
2026-11-22T19:20:25.692008Z  INFO citadel::daemon: scheduled web sync complete source=bench-uv added=139 updated=0 unchanged=0 stale=0 failed=0
```

plus the new `web_sync_summary` line from item 2.

- **Opening and closing.** A Sync is open from its start line to its complete line, or to its `scheduled web sync failed` line. The daemon's start line gains `origin=<url>` (task 2), so the script knows each Sync's host from its first line and reads the file once. For logs written before that change, `--origin name=url` gives the Origin.
- **Attribution.** A `fetch` line goes to the only open Sync. When two Syncs are open, it goes to the one whose Origin has the same host. When two open Syncs share a host, their lines cannot be told apart, and both rows are marked "not separable". Lines that fit no open Sync are counted and reported.
- **`--single`** treats the whole file as one Sync: the stderr of one `citadel sync` run, which has no daemon start or complete lines.
- **Numbers per step.** When a Sync has a `web_sync_summary` line, the script prints its durations and counts next to the row; they say directly how long the first crawl and the later crawls took.
- **Change of pace, old logs only.** For a Sync without a summary line, the script reports a second part when, from some point on, the median gap over the rest of the Sync is more than twice the median gap before it. This is how the `bench-uv` Syncs above were split.

The script depends on spider's log line format. If a spider update changes it, the script finds no `fetch` lines and says so.

### 2. Accounting record and log line (G2, G3)

A crate-private struct, `WebSyncAccounting`, is created at the start of a web Sync and passed by mutable reference through the crawl steps, so a Sync that fails part-way still has its numbers. Where each page can leave the pipeline:

```text
discovery                                  → discovery_ms
first crawl  → receiving task              → first_crawl_ms, first_crawl_received
      task fell behind                     → lost_by_listener
later crawls, one per discovered URL       → later_crawls_ms, later_crawls_started
      no usable URL         (web.rs:380)   → dropped: unparsable_url
      already collected     (web.rs:383)   → dropped: already_collected
build_page_outcomes                        → conversion_ms
      URL cannot be parsed  (web.rs:670)   → dropped: unparsable_url
      redirect target not crawlable (676)  → dropped: redirected_out_of_scope
      URL not storable      (web.rs:682)   → dropped: url_not_storable
      URL already has a result (687)       → dropped: duplicate_result
      status 400 or higher  (web.rs:703)   → result: failed
      binary file           (web.rs:707)   → dropped: binary_file
      otherwise                            → result: stored
database write                             → write_ms
```

- `pages_received` counts every page the receiving task got from any crawler. It always equals results plus the six drop counts; a test checks this.
- `redirected_out_of_scope` is the same check the code makes today, so it also covers redirects to excluded patterns and to static assets. For static crawls, spider sets the redirect target on each page from the HTTP response (`spider` `page.rs`, read in code; not observed at run time).
- A page that fits two reasons is counted under the first it meets.
- `delay_ms` is the delay spider reports after the first crawl (`Website::get_delay()`). It differs from 100 ms when the site's `robots.txt` sets a `Crawl-delay` that spider's parser keeps.

When the Sync ends, successfully or not, one line is logged at `info` level. It is one line in the log, wrapped here:

```text
<time>Z  INFO citadel::ingest::web: web_sync_summary source="<name>" origin="<url>" ok=<true|false>
  max_pages=<n> cut_off=<true|false> delay_ms=<n> pages_received=<n> results_stored=<n> results_failed=<n>
  lost_by_listener=<n> unparsable_url=<n> already_collected=<n> redirected_out_of_scope=<n>
  url_not_storable=<n> duplicate_result=<n> binary_file=<n> later_crawls_started=<n>
  discovery_ms=<n|-> first_crawl_ms=<n|-> later_crawls_ms=<n|-> conversion_ms=<n|-> write_ms=<n|->
```

Text values are quoted; `-` marks a step the Sync never reached. If the Sync fails before the record exists (the Origin is not a valid URL), no line is written.

### 3. Deciding "cut off" (G4)

A Sync is cut off when any of these holds:

1. **The first crawl used up spider's limit:** the pages it delivered plus the pages lost on the way reach `max_pages`. The first crawl is a single crawler, so it cannot fetch a page twice.
2. **The collected pages fill the budget:** after the later crawls, the number of distinct storable URLs among the collected pages reaches `max_pages`. This is `count_scoped_pages` (`web.rs:646-657`), the helper the later-crawl loop already uses for its own budget check. Pages a later crawl fetched again are discarded before collection (`web.rs:383`), so they do not count.
3. **Today's condition:** `outcomes.len() >= max_pages`. Keeping it means every Sync that is cut off today stays cut off.

The decision is one pure function of these numbers and `max_pages`, so it can be tested without a served site. Checked against three cases:

| Case | Today | With this rule |
|---|---|---|
| `dlthub` 11-23: first crawl delivered about 1,000 pages, 756 results | complete, 197 marked stale | cut off by condition 1, nothing marked stale |
| Existing test `honors_max_pages_and_include_override` (`max_pages` 2) | cut off | cut off, by conditions 1, 2 and 3 |
| A Source under its budget whose later crawls fetch pages again | complete | complete: the first crawl stayed under its limit and the distinct URLs stay under the budget |

That the first crawl delivers exactly `max_pages` pages when it is cut short is read from spider's budget code, not observed; the `honors_max_pages_and_include_override` test pins it. The result is written to `cut_off` in the log line, and `sync_web_source` chooses `MissingPagePolicy::Preserve` (keep unvisited pages) from it. With `--prune`, missing pages are deleted as today.

Side effect: a Source that is cut off on every Sync never marks removed pages stale, until a Sync completes or runs with `--prune`. That is already true today for a Source that uses up its budget without dropping pages.

### 4. Budget warning (G5)

When a Sync is cut off, at least 5 pages were dropped as `redirected_out_of_scope`, and they are more than 10 % of `pages_received`, one sentence is added to the Sync's existing `warnings` list, for example:

> `<redirected>` of `<received>` pages received redirected outside this Source and were not stored, for example `<up to three URLs>`. They count against `max_pages` (`<max_pages>`), so other pages were not reached. Raise `max_pages`, or exclude that section if you do not need it.

The warning is one pure function of the accounting record and the cut-off decision, so it is tested without a served site.

The CLI already prints the warnings. The UI and MCP receive them in the Sync summary. The daemon does not log them today; it will log each one at `warn` level.

Only redirects are counted because they are the drop reason seen in volume. The warning does not promise that an exclude pattern can target only the redirected pages: on `dlthub` no pattern separates them from stored pages.

### 5. Measurement (G6)

Following the measuring skill's rules (never sync against the user's vault):

1. Run the script on the existing `daemon.log` with `--origin` for the five Sources. The request counts must match the table above.
2. Make a scratch data directory and start a foreground daemon on it with stderr captured.
3. Add `https://dlthub.com/docs` with `max_pages` 1,000 and the exclude patterns `*.md` and `*/devel/*` (the author's settings); add the `bench-uv` Origin (`https://docs.astral.sh/uv/`) with its default settings.
4. When both Syncs are done, run the script on the captured stderr.
5. Write the table, the step durations and the drop counts into `Docs/resource-usage.md`.
6. Only if the author answers yes to Q1: repeat steps 2 to 5 for the `aws-graffana` Origin.

The author starts steps 3 and 6, because they send requests to third-party sites.

### What does not change

Which pages are requested, stored or failed; the delay and the 4 fetches at once; `robots.txt` handling; the Sync claim (`Docs/adr/0001-database-sync-claim.md`); the single write transaction; the database schema; `render_js` Sources. Which pages are marked stale changes only for a Sync that is cut off by the new rule and not by today's.

## Risks and Mitigations

| Risk | Mitigation |
|---|---|
| The cut-off rule keeps a page stale-free that the site removed | Only for Syncs whose first crawl used up its limit or whose distinct collected URLs filled the budget; `--prune` and any complete Sync still mark it. |
| The receiving task falls behind and loses pages | Lost pages count toward condition 1. None were lost in the logged Syncs; the `lost_by_listener` field shows it if it happens. |
| A spider update changes the `fetch` line or the budget behaviour | The script reports "no fetch lines found"; the `honors_max_pages_and_include_override` test fails if the budget changes. |
| The script attributes lines to the wrong Sync | It marks rows it cannot attribute; the new summary line gives exact counts per Sync. |
| An Origin or example URL in the log contains credentials | Citadel Origins are public documentation URLs; the log stays on the user's machine, as today. |
| Large or rotated logs | The script streams the file line by line and reads only the files it is given. |
| The measurement hits third-party sites | Only `dlthub` and `bench-uv`, about 1,150 requests at today's rate; `aws-graffana` only if the author agrees (Q1). |

## Alternatives Considered

This design went through a staged review by fresh-context reviewers. "Rejected by author" marks a decision the author made, recorded in `concepts_brief.md` or `zoom_out_report.md` in this repository. "Rejected in review" marks an option a reviewer or the drafting pass set aside from reading code or logs.

### Alternative A: The first version: statistics record, generated test site, one crawl instead of later crawls

*   **Pros**: Would remove the later crawls, which cost about 70 % of the crawl time in the `bench-uv` Syncs.
*   **Cons**: It assumed pages were downloaded several times, which the `fetch` lines do not show. Review found that its per-page fetch time could not work, that its test site might not reproduce the problem, and that injecting URLs into one crawl changes which pages get the budget.
*   **Reason for Rejection**: Rejected by author after review stage 5 (`zoom_out_report.md`). One crawl stays the first candidate for the next design.

### Alternative B: The script only, no change in Citadel

*   **Pros**: No production code.
*   **Cons**: The log cannot say why a fetched page got no result, cannot split the time after the last request, and does not label later crawls. The stale-mark defect stays.
*   **Reason for Rejection**: Rejected in review.

### Alternative C: Avoid fetching pages that redirect out of the Source

*   **Pros**: Up to 246 more pages reached on `dlthub` for the same budget.
*   **Cons**: Three ways, each with a cost. Refusing such redirects in the redirect policy still sends the first request and so still uses a budget slot. Remembering redirected URLs between Syncs needs storage and a re-check rule. Giving spider a higher limit than `max_pages` and capping stored pages changes what `max_pages` means to users.
*   **Reason for Rejection**: Rejected in review: a design of its own once the drop counts exist for more Sources.

### Alternative D: Record the stale-mark defect and fix it later

*   **Pros**: Keeps this design to measurement.
*   **Cons**: Every further cut-off Sync with dropped pages marks more pages stale.
*   **Reason for Rejection**: Rejected in review: the fix needs the counters this design adds anyway.

### Alternative E: Decide "cut off" from a page count

*   **Pros**: One comparison instead of three conditions.
*   **Cons**: Counting results misses dropped pages (the defect). Counting pages received, or comparing each later crawl with the remainder it was given, over-counts pages that later crawls fetched again, so a Source under its budget could count as cut off on every Sync.
*   **Reason for Rejection**: Rejected in review: the chosen rule counts the first crawl, which cannot fetch a page twice, and distinct collected URLs, which re-fetches cannot inflate.

### Alternative F: Store the numbers in `sync_runs`

*   **Pros**: History per Source in the UI and through MCP.
*   **Cons**: New columns, and `Docs/adr/0001-database-sync-claim.md` records that the schema has no migration mechanism.
*   **Reason for Rejection**: Rejected in review.

### Alternative G: Tag every log line with the Source name

*   **Pros**: Exact attribution of overlapping Syncs, including two Sources on one host.
*   **Cons**: Needs each crawl to run inside a `tracing` span and a log format that prints span fields; whether spider's lines carry the span was not checked.
*   **Reason for Rejection**: Rejected in review: the summary line gives exact per-Sync numbers; the script is for older logs and quick reading.

### Alternative H: Lower the delay now

*   **Pros**: Five of nine Syncs are paced by it.
*   **Cons**: Citadel re-syncs every Source on a schedule; at a shorter delay the 4 fetches at once may become the limit.
*   **Reason for Rejection**: Rejected by author (`concepts_brief.md`, "Direction from the author"): keep the rate in line with Firecrawl and crawl4ai.

--------------------------------------------------------------------------------

## Non-Functional Requirements

*   **Clean stdout:** all new output goes through `tracing` to stderr, or to `daemon.log` in the daemon. `citadel mcp` keeps stdout for protocol messages only (`AGENTS.md`).
*   **Cost:** about ten clock reads and a dozen counters per Sync; no extra request.
*   **Rollout:** three visible changes: the summary log line, the warning, and that a Sync cut off by the new rule marks no page stale. A page already wrongly marked stale becomes active again when a later Sync stores it (`persist_sync_outcomes` sets the status of every stored page).
*   **Rollback:** install the previous binary; no stored data needs converting.
*   **Lite build:** compiles and passes with `--no-default-features` (no Chrome support).

## Additional Resources

*   `zoom_out_report.md`, `comprehension_report.md`, `critic_report.md`: the review so far.
*   `concepts_brief.md`: the brainstorm and the author's decisions on `robots.txt` and crawl rate.
*   `PERFORMANCE.md` findings 1 (the delay) and 6 (one crawl per sitemap URL).
*   `.agents/skills/measuring-citadel-footprint/SKILL.md`: rules for measuring on a scratch vault.

## Future Improvements

*   **Replace the later crawls** with one crawl or run them several at a time.
*   **The delay**, with the numbers from G6.
*   **Conversion and write time**, once G2 shows which grows.
*   **Pages stored twice under two spellings.**
*   **Rename this file**: its name still reflects the first version.

--------------------------------------------------------------------------------

## Detailed Implementation Plan

Five tasks, one commit each, in order 1 to 5: task 3 needs task 2's counters, and task 4 needs task 3's `cut_off`. Tasks 1 to 4 start with a failing test. Tasks 2 to 4 each change `Docs/SPEC.md` in their own commit.

| Task | Goal | What | Done when |
|---|---|---|---|
| 1 | G1 | Report script | The tests in file 2 pass |
| 2 | G2, G3 | `WebSyncAccounting`, drop counters, durations, `delay_ms`, summary line, `origin=` on the daemon's start line | `dropped_pages_are_counted_by_reason` and `accounting_times_every_step_and_counts_later_crawls` pass |
| 3 | G4 | "Cut off" from the three conditions | `cut_off_sync_with_dropped_page_marks_nothing_stale` (red today: it marks 1 page stale) and the `is_cut_off` unit tests pass; `honors_max_pages_and_include_override` still passes |
| 4 | G5 | Warning, also in the daemon log | The `budget_warning` and `sync_warning_lines` unit tests pass |
| 5 | G6 | Measurement and records | The steps in Technical Plan item 5 are done; files 7, 8, 11 and 12 are updated |

### Target Files to Create/Modify

#### 1. `.agents/skills/measuring-citadel-footprint/scripts/sync_log_report.py` (Create)

*   **Type / Scope**: Python script, standard library only. Task 1.
*   **Proposed Changes**: Takes a log path, optional `--origin name=url` (repeatable) and `--single`. Streams the file line by line. Opens Syncs on the daemon's start line (reading `origin=` when present) and closes them on the complete or failed line; attributes `fetch` lines as in Technical Plan item 1; reads `web_sync_summary` lines (quoted values, `-` for unreached steps); for Syncs without a summary line, finds a slow last part with the rule from item 1. Prints one row per Sync with the columns of the table in "The Problem", a second row for a slow last part, the summary line's numbers when present, and the count of unattributed lines. Prints "no fetch lines found" when there are none.

#### 2. `.agents/skills/measuring-citadel-footprint/scripts/test_sync_log_report.py` (Create)

*   **Type / Scope**: Python `unittest`. Task 1.
*   **Proposed Changes**: Hand-written logs in the formats shown in Technical Plan items 1 and 2, one test each:
    *   two overlapping Syncs on different hosts with `origin=` on their start lines are split correctly, and one of them is closed by its failed line;
    *   two overlapping Syncs on one host are both marked "not separable";
    *   an old-format log (no `origin=`, no summary line) with `--origin` gets its rows, and a slow last part is reported;
    *   `--single` on a log with no daemon lines gives one row;
    *   a `fetch` line outside any open Sync is counted as unattributed;
    *   a log with no `fetch` lines prints "no fetch lines found".

#### 3. `.agents/skills/measuring-citadel-footprint/SKILL.md` (Modify)

*   **Type / Scope**: Skill instructions. Task 1.
*   **Proposed Changes**: A section on `sync_log_report.py`: when to use it, how to capture a log on a scratch vault, and the requests a `fetch` line does not show (the list under "What is still unknown").

#### 4. `src/ingest/web.rs` (Modify), production code

*   **Type / Scope**: Web ingest. Tasks 2, 3 and 4.
*   **Proposed Changes**:
    *   **Task 2.** Add this crate-private struct. It is shown in its final form: task 2 adds every field except the four marked "not printed"; task 3 adds `first_crawl_received`, `first_crawl_lost` and `collected_storable_urls` together with their only reader, `is_cut_off`; task 4 adds `redirect_examples` together with `budget_warning`. Adding a field before anything reads it fails `cargo clippy -- -D warnings` (dead code), and no `allow(dead_code)` is used. The summary line prints every field except those four; `delay_ms` prints as `-` when `None`.

        ```rust
        #[derive(Debug, Default)]
        pub(crate) struct WebSyncAccounting {
            pub delay_ms: Option<u64>,
            pub pages_received: usize,
            pub lost_by_listener: usize,
            pub first_crawl_received: usize,      // not printed; task 3
            pub first_crawl_lost: usize,          // not printed; task 3
            pub later_crawls_started: usize,
            pub collected_storable_urls: usize,   // not printed; set in task 3
            pub results_stored: usize,
            pub results_failed: usize,
            pub unparsable_url: usize,
            pub already_collected: usize,
            pub redirected_out_of_scope: usize,
            pub url_not_storable: usize,
            pub duplicate_result: usize,
            pub binary_file: usize,
            pub redirect_examples: Vec<String>,   // not printed; task 4; at most 3 requested URLs
            pub cut_off: bool,
            pub discovery_ms: Option<u64>,
            pub first_crawl_ms: Option<u64>,
            pub later_crawls_ms: Option<u64>,
            pub conversion_ms: Option<u64>,
            pub write_ms: Option<u64>,
        }
        ```

        A `None` duration prints as `-`. Signatures and who sets what:
        *   `sync_web_source_with_accounting(db_path: &Path, source: &SourceRecord, prune: bool) -> (Result<SyncSummary>, WebSyncAccounting)` creates `WebSyncAccounting::default()` and calls an inner `async fn` that holds today's body of `sync_web_source` with `?` intact, taking `&mut WebSyncAccounting` and returning `Result<SyncSummary>`. If the Origin fails today's checks (URL parse or `WebScope::new`), it returns the default record and logs no line. Otherwise it logs the summary line just before returning, on the success and the error path. It times `persist_sync_outcomes` into `write_ms`. In task 2 it sets `cut_off` with today's rule, `outcomes.len() >= max_pages`, so behaviour does not change. `sync_web_source` calls it and returns the `Result`.
        *   `execute_web_crawl(source, origin, scope, acc: &mut WebSyncAccounting)`, same return type. It times discovery, the first crawl, the later crawls and the `build_page_outcomes` call (`conversion_ms`); sets `later_crawls_started` and `delay_ms` (and, from task 3, `first_crawl_received` and `first_crawl_lost`); counts the drops at lines 380 and 383; and adds each crawl's pages and lost count to `pages_received` and `lost_by_listener`.
        *   `run_website_crawl(website: &mut Website, render_js: bool) -> Result<(Vec<Page>, usize)>`. The spawned collector (lines 540-555) returns its pages together with the sum of its `skipped` values, converted to `usize`; the caller adds them up. The stall path (lines 575-583) returns an error as today; it only occurs with `render_js`, which this design does not change.
        *   `build_page_outcomes(pages: Vec<Page>, scope: &WebScope, acc: &mut WebSyncAccounting)`, same return type. It counts the drops at lines 670, 676, 682, 687 and 707, and sets `results_stored` and `results_failed`; the `continue` at line 703 is a failed result, not a drop.
    *   **Task 3.** Add a pure function `is_cut_off(first_crawl_delivered_or_lost: usize, collected_storable_urls: usize, outcomes: usize, max_pages: usize) -> bool` implementing the three conditions of Technical Plan item 3. `execute_web_crawl` sets `collected_storable_urls` to `count_scoped_pages(&collected_pages, scope)` after the later-crawl loop. `sync_web_source_with_accounting` calls `is_cut_off(acc.first_crawl_received + acc.first_crawl_lost, acc.collected_storable_urls, outcomes.len(), source.max_pages as usize)`, stores the result in `accounting.cut_off`, and chooses `MissingPagePolicy::Preserve` from it instead of line 308's comparison.
    *   **Task 4.** Add a pure function `budget_warning(accounting: &WebSyncAccounting, max_pages: usize) -> Option<String>` that returns the sentence of Technical Plan item 4 when `cut_off`, `redirected_out_of_scope >= MIN_REDIRECTS_FOR_WARNING` (5) and `redirected_out_of_scope * 100 > pages_received * REDIRECT_SHARE_PERCENT` (10). `build_page_outcomes` pushes the first three redirected URLs to `redirect_examples`. `sync_web_source_with_accounting` passes `source.max_pages as usize` and pushes the sentence to the Sync's warnings before `persist_sync_outcomes`.

#### 5. `src/ingest/web.rs` (Modify), test module

*   **Type / Scope**: Tests in the existing `mod tests`. Tasks 2, 3 and 4.
*   **Proposed Changes**:
    *   No change to the test server. Citadel uses the strict redirect policy for the loopback test server (`web.rs:421-425`), and spider refuses redirects to internal addresses (spider `website.rs:2877-2935`, read in code, not observed), so a served site cannot produce an off-Source redirect. Redirect cases are covered by the hand-made inputs below.
    *   **How tests read the record:** `SyncSummary` stays unchanged. A served-site test adds the Source with `SourceLifecycle::add(params, false)` (no Sync), then calls `sync_web_source_with_accounting(&cfg.db_path(), &added.source, false).await` and asserts on both the `Result<SyncSummary>` and the record.
    *   **Task 2:**
        *   `dropped_pages_are_counted_by_reason` calls `build_page_outcomes` with hand-made pages (spider's `Page` derives `Default`; `set_url`, `status_code`, `final_redirect_destination` and `binary_file` are public): one page for each of the five reasons counted there, one failed page (status 404) and one stored page. It asserts each counter is 1 and `results_stored`, `results_failed` are 1. A served site cannot produce every reason: spider blocks plain out-of-scope links before fetching them, and loopback redirects (see above).
        *   `accounting_times_every_step_and_counts_later_crawls` uses the default `max_pages` and serves an Origin linking to two pages `a` and `b`, and an `llms.txt` listing two hidden pages `h1` and `h2` under the Origin's path. The hidden pages are not linked from the Origin, do not link to each other, and each links back to the Origin; there is no `sitemap.xml`. It asserts `later_crawls_started == 2`, `already_collected == 6` (each later crawl fetches the Origin, `a` and `b` again), `pages_received == 11`, every duration is `Some`, and `pages_received == results_stored + results_failed + ` the six drop counts.
    *   **Task 3:**
        *   `cut_off_sync_with_dropped_page_marks_nothing_stale`, the red test. Origin `http://{addr}/docs/` with `include_patterns: ["docs/p*"]` and `max_pages` 3, linking to `/docs/p1` to `/docs/p5`. The Origin is crawlable (the exception at `web.rs:188`) but not storable (`web.rs:198`: no include matches `docs/`), so it is dropped as `url_not_storable`. The first crawl delivers 3 pages and the Sync has 2 results. Insert an `active` row for `/docs/archived`, which is never served (as at `web.rs:2405-2412`), then sync. Today the rule at line 308 chooses `MarkStale` and `pages_stale == 1`. After task 3: `cut_off == true`, `pages_stale == 0`, and the row is still `active`. (That spider counts the Origin page against `with_limit` is read from its budget code, not observed.)
        *   Unit tests on `is_cut_off(first_crawl_delivered_or_lost, collected_storable_urls, outcomes, max_pages)`: `first_crawl_at_limit_is_cut_off` with the `dlthub` numbers, `(1000, 759, 756, 1000) == true` (condition 1 only); `collected_urls_at_limit_is_cut_off`, `(40, 100, 90, 100) == true`; `todays_condition_still_counts`, `(2, 2, 3, 3) == true`; `under_budget_is_complete`, `(40, 60, 60, 100) == false`.
    *   **Task 4:** unit tests on `budget_warning`: `warns_when_budget_went_to_redirects` (5 of 20, cut off: one warning naming the 3 examples); `no_warning_below_five_redirects` (4 of 20); `no_warning_at_ten_percent` (5 of 50); `warns_above_ten_percent` (5 of 49); `no_warning_when_not_cut_off` (5 of 20).
    *   **Guard tests that must stay green:** every existing test in the module, in particular `honors_max_pages_and_include_override`, `recrawl_records_diff_marks_removed_stale_and_500_error` and `seeds_from_sitemap_respects_robots_and_filters_patterns`.

#### 6. `src/daemon/mod.rs` (Modify)

*   **Type / Scope**: Daemon logging. Tasks 2 and 4.
*   **Proposed Changes**: Task 2: the "starting scheduled web sync" line (line 460) gains `origin = %origin`, where `origin` is cloned from `source.origin` before line 449, where the Source moves into `claim_record` (as `name` is at line 448). Task 4: a new pure function `sync_warning_lines(summary: &SyncSummary) -> Vec<String>` returns one line per warning, each starting with the Source name. `log_sync_summary` (line 469) logs each at `warn` level; it is also called by the local-folder watcher (line 590), which then logs its warnings too. Unit test `sync_warning_lines_name_the_source`: no warnings gives an empty list; two warnings give two lines containing the Source name.

#### 7. `Docs/resource-usage.md` (Modify)

*   **Type / Scope**: Measurement record. Task 5.
*   **Proposed Changes**: A scenario "Where a web Sync spends its time": the nine-row table and how it was made, then the script output, step durations and drop counts from the scratch-vault Syncs, then `aws-graffana` if Q1 is yes.

#### 8. `README.md` (Modify)

*   **Type / Scope**: Resource Usage table. Task 5.
*   **Proposed Changes**: The measuring skill (`SKILL.md` step 6) requires this table, and the `Docs/SPEC.md` lines that cite resource figures (around line 562), to change with `Docs/resource-usage.md`. Add a row for Sync time per page if the measurement gives one value that holds for both Sources; otherwise give the reason in the commit message. The commit says whether `SPEC.md:562` changed.

#### 9. `Docs/SPEC.md` (Modify)

*   **Type / Scope**: Section 4.1. Tasks 2, 3 and 4.
*   **Proposed Changes**: Task 2: every web Sync whose Origin is a valid URL logs a `web_sync_summary` line, whether it succeeds or fails. Task 3: a Sync is cut off when its first crawl used up `max_pages`, or when the distinct storable pages it collected, or the pages it recorded, reach `max_pages`, whether or not those pages were stored; a cut-off Sync marks no page stale. Task 4: a cut-off Sync warns when at least 5 pages, and more than 10 % of those received, redirected outside the Source.

#### 10. `CONTEXT.md` (Modify)

*   **Type / Scope**: Glossary. Task 3.
*   **Proposed Changes**: The "Stale page" entry's second sentence becomes: "A cut-off Sync leaves unvisited Pages as they were." A new entry, **Cut-off Sync**: "A Sync whose first crawl used up the page budget (`max_pages`), or whose distinct storable pages collected, or pages recorded, reach it, whether or not those pages were stored."

#### 11. `PERFORMANCE.md` (Modify)

*   **Type / Scope**: Findings list. Task 5.
*   **Proposed Changes**: Finding 1: the measured pace, and a corrected reference (it cites spider `website.rs:4977`, in a function behind spider's `cmd` feature, which Citadel does not enable; the sleep Citadel uses is at `website.rs:8488-8490`). Finding 6: the `bench-uv` evidence. New findings: page budget spent on redirected pages; pages stored twice under two spellings.

#### 12. `concepts_brief.md` (Modify)

*   **Type / Scope**: Brainstorm record. Task 5.
*   **Proposed Changes**: Under "Open, needs a measurement", record the four patterns and which Sources show them. In the same edit, rename the brief's Term "Dropped page" to "Lost page", replace "seed crawl" with "later crawl" throughout, and point to this design's Terms section.

#### Files deliberately not changed

`Cargo.toml`, `src/db/schema.sql`, `src/ingest/local.rs`, `src/ingest/mod.rs`, `src/sources/`, `src/cli/`, `src/mcp/`, `src/ui/`, `AGENTS.md`.

### Verification Plan

*   **Compilation/Build**: `cargo build` and `cargo build --no-default-features`.
*   **Unit Tests**: `cargo test`; `cargo test sources` (required by `AGENTS.md` after a change in `src/ingest/`); `python3 -m unittest discover -s .agents/skills/measuring-citadel-footprint/scripts -p 'test_*.py'`.
*   **Static Analysis & Linting**: `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings`.
*   **Test Coverage Requirement**: No percentage. Every "done when" test exists and passes; no existing test is changed.
*   **E2E/Integration**: Technical Plan item 5, steps 1 to 5. Step 1 must reproduce the request counts 139, 500, 500, 139, 500, 59, 1,003, 19 and 540. Step 4 also checks that the script reads the real `web_sync_summary` lines in the captured stderr. The stale-page fix is covered by `cut_off_sync_with_dropped_page_marks_nothing_stale`; no extra live Sync is run.

--------------------------------------------------------------------------------

*Drafted with EGM*
