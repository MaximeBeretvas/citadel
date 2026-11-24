# Faster full crawl of a static web Source, and obeying each site's robots.txt

* **Author:** MaximeBeretvas
* **Date:** 2026-11-24

## Overview

Citadel crawls documentation websites with the `spider` crate (2.54.2), converts each page to Markdown and stores it in SQLite. One crawl-and-store run of a Source is a Sync.

This brief started as one idea: make a Sync of a large static docs site at least 4 times faster. Two rounds of review by seven fresh-context reviewers, plus lookups in the author's vault and on the live sites, turned it into three separate pieces of work:

- **A. Remove duplicate work.** Measure a Sync, then make sure each URL is requested once. This is where a speed gain is most likely, and it is the only part ready for a design draft.
- **B. Obey the site's `Disallow` rules.** Reading the code showed Citadel today obeys neither the `Crawl-delay` nor the `Disallow` lines of the benchmark site, because spider's parser drops them. The author first decided to obey `Crawl-delay` in full, then reversed that after comparing with Firecrawl and crawl4ai: Citadel will obey `Disallow` and ignore `Crawl-delay`, as both of those tools do.
- **C. React to throttling.** Citadel's crawl rate already matches crawl4ai's. The part to copy is slowing down when a site answers 429 or 503.

The final direction for B and C is in "Direction from the author" near the end. Earlier sections on a 5 s delay and multi-hour Syncs describe the option that was dropped.

**Nobody has run or timed anything.** Every number below comes from the vault's `sync_runs` history, from files fetched once, or from reading code. The cause of the slow Syncs is still unknown.

Line numbers refer to Citadel commit `2216364` and spider 2.54.2. `W` means spider `src/website.rs`, `P` means spider `src/packages/robotparser/parser.rs`, `web` means Citadel `src/ingest/web.rs`.

## Analysis

### Terms

- **Page start:** the moment the crawler sends the first request for a page.
- **Delay:** the wait before each page start. Today 100 ms.
- **Seed crawl:** a new crawler that Citadel starts on a sitemap or `llms.txt` URL that the first crawl did not store. (`CONTEXT.md` lists "seed" as a word to avoid for Origin; the draft needs a better name.)
- **Dropped page:** a page the crawler fetched but Citadel's listener missed because it fell behind.
- **Host:** scheme, host name and port of a URL. Not the same as an Origin, which includes a path.

### Baselines from the vault (one run each)

| Source | Site sets `Crawl-delay`? | Pages stored | Time | Pages per second | Failed |
|---|---|---|---|---|---|
| `aws-graffana` | Yes, 5 s | 1,523 | 699 s | 2.2 | 0 |
| `dlthub` | No | 725 | 185 s | 3.9 | 2 (both HTTP 404) |
| `developer-cisco-com-meraki` | No | 538 | 157 s | 3.4 | 2 |
| `dbt-best-practices` | No | 59 | 7 s | 8.4 | 0 |

All have `render_js` off. No machine, network or entry point was recorded.

### What the code does today (confirmed in both review rounds)

1. **The delay caps page starts at 10 per second.** One dispatcher sleeps 100 ms, takes one of 4 permits, then starts the fetch (`W:8488-8490`, `8503`). Up to 4 fetches overlap; 4 never start together.
2. **Each seed crawl is a full crawl.** It gets the whole remaining page budget, the full depth and an empty "already visited" list (`web:361-388`). Pages already held are downloaded again and then thrown away. Each one also fetches `robots.txt` and probes `<url>/sitemap.xml`.
3. **spider runs its own sitemap pass after the link crawl,** with no delay (`W:7803-7804`, `13241-13354`). Citadel turns it off only for `render_js`. So the sitemap is read twice.
4. **Several requests are never delayed:** the `robots.txt` fetch, the first page of every crawler, retries, spider's sitemap pass, and Citadel's own sitemap and `llms.txt` discovery (up to about 14 requests, sent before `robots.txt` is read and without a robots check, `web:838-916`).
5. **No retries.** Any status of 400 or higher is stored as an Error page (`web:691`).
6. **No timing exists,** and a static crawl has no timeout at all.
7. **Everything is held in memory until the crawl ends,** then written in one transaction.

### Citadel ignores the AWS robots.txt today

`https://docs.aws.amazon.com/robots.txt` was fetched on 2026-11-24. It uses LF line endings and has truly empty lines after `User-agent: *` (line 2), after the `Sitemap:` line (line 4) and before `Crawl-delay: 5` (line 104).

spider's parser treats an empty line after `User-agent: *` as the end of that group and discards it (`P:446-451`). Every later `Disallow:` and `Crawl-delay:` line is then ignored (`P:485-490`, `496-513`). Read in code, not run.

Consequences:

- **The 5 s `Crawl-delay` is not in force.** Citadel crawled this site at 100 ms. An earlier version of this brief explained the 699 s by a mix of a 5 s link crawl and an undelayed sitemap pass. That was wrong: 763 pages reached only by links at 5 s each would be 3,815 s.
- **The site's `Disallow` rules are not in force either.** This affects any site whose robots.txt has an empty line inside a group.
- **The 699 s is unexplained again.** At 100 ms the floor for 1,523 pages with no duplicate requests is about 152 s.
- If the site's 5 s is the rule, today's Sync runs about 11 times faster than the site asks (0.46 s per stored page), with up to 4 requests in flight.

Even when spider's parser does keep a `Crawl-delay`, three things stop it from meaning "obey in full":

- spider caps it at 60 s (`W:2860`).
- It replaces Citadel's delay instead of taking the larger value, so `Crawl-delay: 0` switches off Citadel's own delay (`W:8417`).
- The undelayed requests in item 4 above stay undelayed.

### Why the cause of the slowness is still unknown

Two readings fit the numbers (3.4 to 3.9 pages per second on the large Sources, against a cap of 10):

- **Latency-bound:** each fetch takes about 1 s, so 4 permits give about 4 pages per second. A smaller delay then changes nothing; only more permits help.
- **Duplicate-bound:** Citadel sends about 2.5 requests for every page it stores, because of seed crawls and the double sitemap read. Then removing duplicates (work A) is the fix.

What separates them: requests per stored page, seed crawls started, and median and tail fetch time. Requests cannot be counted from the page stream (retries, redirects and robots fetches are invisible there); they have to be counted on a test server or inside the fetch layer.

### The 4× target forces a rate change

`dlthub` at 4× is 725 pages in 46 s, which is 15.7 page starts per second. Today's cap is 10. So the target cannot be met with a 100 ms delay whatever the measurement shows: it needs a delay of about 60 ms or less, and probably more than 4 permits. With 100 ms the best case is about 72 s, or 2.5×.

At 25 to 50 ms with 4 permits, the 4 permits become the limit. In practice that is the "no delay, fixed concurrency" option the author rejected.

### `dlthub` is not a stable benchmark

Two Syncs a day apart: 725 pages stored, then 225 added and 529 unchanged. The Source now has 754 active, 197 stale and 2 error pages.

- The 197 stale pages do not pair with the added ones: 0 match by URI after trimming the trailing slash, 9 match by content. So this is not a URI-spelling defect.
- Both the stale and the added pages are under `/docs/pipelines/<source>/load-data-with-python-from-<source>-to-<destination>`, a large generated set. Each run stores a different part of it.
- So the rule "stored URI set equal to the baseline" cannot be judged on `dlthub`, and one run cannot prove a 4× gain.

### What a very long Sync does to the rest of Citadel

If Citadel obeys a 5 s `Crawl-delay`, `aws-graffana` takes at least 2 h 07 min for 1,523 pages, and 6.9 h if it reached its `max_pages` of 5,000.

| Mechanism | Effect |
|---|---|
| Sync claim | No heartbeat. Another process can take over a claim older than 6 h (`src/sources/claim.rs:21`, ADR-0001), giving two crawls of one host. |
| Daemon slots | 2 web Syncs at once, each held for the whole Sync (`src/daemon/mod.rs:32`). Two slow Sources block all other scheduled web Syncs for hours. |
| Shutdown | No cancel point. `stop_daemon` gives up after 5 s; a kill loses the whole crawl. |
| Memory and loss | All pages are held until the end. A crash at 1 h 59 min stores nothing. |
| CLI | `citadel add` runs the first Sync inline with no progress output. `add --no-sync` exists. |
| MCP and UI | `manage add`, `manage sync` and the UI's "create source" wait for the Sync. A 2-hour tool call times out on the client while the Sync continues. |
| Scheduler | After a killed Sync, a Source with a refresh interval shorter than the Sync restarts from zero on the next tick. |

### Can one pacing clock per host be built on spider?

Read in code, not run:

| Approach | Works? | Reason |
|---|---|---|
| Share one concurrency semaphore between crawlers | No | It limits fetches in flight, it does not space starts. If another crawl holds every permit, spider ends the crawl early and drops pages (`W:8461`, `9269`). |
| spider's link callbacks | No | They are synchronous, and one runs after the fetch. |
| `with_shared_queue` | No | One process-wide pool, not per host. |
| Custom HTTP client | No | No per-request hook under Citadel's features; bypasses the redirect policy. |
| One URL per crawler | No | A page limit of 1 skips `robots.txt` entirely (`W:1778-1786`). |
| `with_fetch_engine` or `with_remote_fetcher` | Yes, static crawls only | Citadel does the HTTP send itself and so owns pacing, retries, redirects and the private-host rule. Estimated 4 to 6 days. Open risk: whether a 5 to 60 s wait inside the hook survives spider's first-byte watchdog. |
| Run same-host Syncs one after another | Yes | spider's own delay is then the host clock. About one day. Does not pace retries or the undelayed requests. |

When one clock is the limit, N page starts take N × delay whether two Sources interleave or run back to back. An interleaving clock buys fairness between Sources, not speed.

### Retries conflict with pacing

spider's retry waits on its own back-off, not on the delay (`W:8969-8972`). After a 500 or 503 the first retry can fire almost at once; after a 429 it waits for `Retry-After` (capped at 30 s) or 2.5 s. So "no two page starts closer than the site's delay, including retries" cannot be met with `with_retry`. An alternative: collect failed URLs and fetch them in a second paced crawl.

### One crawl instead of seed crawls: what changes

The method works: `persist_links()` then `set_extra_links` with the Origin included, then `crawl_raw()`. Injected links pass spider's visited, scope, robots, depth and budget checks. Side effects:

- **The budget comes out one short.** With injection the Origin page is charged to the budget, so a cut-off crawl returns at most `max_pages - 1`. Citadel's check `outcomes.len() >= max_pages` (`web:308`) is then never true, and stored pages would be marked stale. The existing test `honors_max_pages_and_include_override` would fail.
- **Deep sitemap URLs are dropped silently** by spider's depth check. spider counts path segments from the host root and Citadel adds the Origin's segments (`web:418-419`); `max_depth` has never been a count of link hops.
- **Discovery gets narrower in places:** Citadel stops after 10 child sitemaps, and a failed discovery fetch yields no URLs and no error. It gets wider in others: Citadel also reads the host-root sitemap.
- spider's sitemap pass ignores the page budget today, so turning it off can shrink Sources where it delivered pages past `max_pages`.
- `set_extra_links` stops at 25,000 entries with no error.
- spider gives no reliable "the crawl was cut off" signal; Citadel has to compute it.

### Rollout risks

- Sources on a host with a `Crawl-delay` go from minutes to hours on their next scheduled Sync.
- Obeying `Disallow` lines that were ignored until now marks stored pages stale.
- The single-crawl change alters which pages are reached, so some pages go stale on the first Sync after an upgrade.
- A binary downgrade does not undo stale marks.

## Goals vs. Non-Goals

* **In-Scope Goals (work A, ready for a draft):**
  * Extend the test server: request timestamps, response sequences (429 then 200), an in-flight counter, `Retry-After`, and a robots.txt with the exact AWS byte layout.
  * Measure a Sync: time per phase, requests per stored page, seed crawls started, dropped pages, the delay in force after `robots.txt`, and whether the raw `robots.txt` held a `Crawl-delay` or `Disallow` line.
  * A repeatable local benchmark site. It is the gate for "same pages stored", "each URL requested once" and pacing. Wall time on live sites is reported as an observation over at least 3 runs.
  * Request each URL at most once per Sync, not counting retries.
  * Store the same set of page URIs as before on the local site, or explain each difference. Zero dropped pages. No new failed pages.
  * Keep the page-scope rules, the Private-origin rule, the Sync claim, the single write transaction and "a failed Sync keeps stored pages".
* **Goals that need author decisions first (works B and C):**
  * Obey each site's `Crawl-delay` and `Disallow` rules.
  * One pacing clock per host inside one Citadel process.
  * A smaller delay for sites that set no `Crawl-delay`, and a speed target.
  * A message that says why a Sync is long.
* **Out-of-Scope Non-Goals:**
  * Conditional GET / ETags.
  * A separate fast code path for first Syncs.
  * `render_js` (Chrome) crawl speed. Each change must still say whether it touches `render_js`.
  * Converting each page to Markdown as it arrives. Decided by the author: a separate memory task. It returns as a speed item only if the measurement shows conversion takes a large share of a Sync.
  * Local Markdown folder Sources, query speed, idle memory budget.
  * Schema changes, including a per-Source delay or concurrency column.

## Alternatives Weighed

| Alternative | Why it was rejected |
|---|---|
| Fetch missed sitemap URLs with plain HTTP, many at once (as axon does) | Rejected by reviewers: bypasses spider's robots check, scope patterns, private-host redirect policy and page budget. |
| `crawl_sitemap()` → `persist_links()` → `crawl_raw()` as the only seeding (as axon does) | Rejected by reviewers: reads only `<origin>/sitemap.xml`, needs an `<?xml` prolog, has no `llms.txt` support and no child-sitemap limit. |
| Rely only on spider's built-in sitemap pass | Rejected by reviewers: same gaps, applies no delay, ignores the page budget. |
| Seed crawls with a page limit of 1 | Rejected by reviewers: a limit of 1 is single-page mode and skips `robots.txt`. Usable only as a diagnostic against a local site. |
| `set_extra_links` without `persist_links`; `Website::queue()` | Rejected by reviewers: links are cleared during set-up, or not seen before the crawl starts. |
| Enable `adaptive_concurrency` | Rejected by reviewers: nothing in spider feeds the controller. |
| spider `auto_throttle` | Rejected by reviewers: driven by latency, does not react to 429/503. |
| A concurrency semaphore shared between crawlers, as a limit or as a clock | Rejected by reviewers: does not space starts and can end a crawl early. |
| 64 or more concurrent requests with no delay (axon's defaults) | Rejected by reviewers, author to confirm: Citadel re-Syncs on a schedule, and the PRD requires polite crawling. |
| No delay with a fixed concurrency limit | Rejected by the author: small docs hosts would get the full rate on every scheduled re-Sync. Note: a 25 to 50 ms delay with 4 permits behaves the same way in practice. |
| One pacing clock per Source | Rejected by the author: two Sources on a 5 s site would send 2 requests per 5 s. |
| Obey `Crawl-delay` up to a cap; ignore `Crawl-delay` | Rejected by the author, who chose to obey it in full. |
| Refuse hosts that set a `Crawl-delay` | Rejected by reviewers: conflicts with FR-1.1 for a mainstream docs host. |
| Per-Source delay or concurrency column | Rejected by reviewers: the schema has no migration mechanism (ADR-0001). |
| Write pages in batches during the crawl; a staging table in SQLite | Rejected by reviewers: breaks the all-or-nothing Sync stated in `Docs/SPEC.md:269-271`; needs a schema change. |
| One write transaction held open for the whole crawl | Rejected by reviewers: blocks every other writer. |
| A time budget per Sync that continues in the next Sync | Rejected by reviewers: each run starts from the Origin with an empty visited list, so the next run has no reason to reach new pages. |
| A heartbeat on the Sync claim | Rejected in ADR-0001; reopened only if Syncs may exceed 6 h. |
| `llms-full.txt` as a one-request content source | Rejected by reviewers: splitting it into pages with correct URIs is guesswork. |
| Conditional GET / ETags | Rejected by the author: out of scope. On a paced host a 304 is still a request. |

## Key Decisions & Staging for Next Steps

### Decisions so far

| # | Decision | Status | Note from the second review |
|---|---|---|---|
| 1 | Measurement ships first, as its own task | Decided | Aim it at the local site and `dlthub`, not at `aws-graffana`. |
| 2 | ~~Obey `Crawl-delay` in full~~ → ignore `Crawl-delay`, obey `Disallow`, as Firecrawl and crawl4ai do | Reversed by the author | See "Direction from the author" below. |
| 3 | One pacing clock per host, in one process | Decided by the author; its reason is gone, to be confirmed | The one-day estimate covers only "same-host Syncs one after another". An interleaving clock is 4 to 6 days. See open question 3. |
| 4 | Smaller fixed delay for sites with no `Crawl-delay` | Decided by the author | Not shown to matter; the 4× target forces about 60 ms or less. See open question 8. |
| 5 | Turn off spider's own sitemap pass on static crawls | Follows from 2 | Removes one undelayed path; the others remain. |
| 6 | Speed benchmark `dlthub`, politeness test `aws-graffana` | Confirmed by the author | `dlthub` cannot carry the "same pages" rule; `aws-graffana` tests nothing until work B exists. |
| 7 | Per-page conversion is a separate task | Decided by the author | |
| 8 | Timing output stays in the logs | Proposed by the assistant | Keep one in-memory progress record that both the log and the long-Sync message read. |
| 9 | Explicit "crawl was cut off" signal | Proposed by the assistant | Citadel must compute it; err toward keeping pages. |
| 10 | ~~Sitemap-listed pages are stored whatever their depth~~ → `max_depth` applies to sitemap pages too | Reversed by the author while approving `design_draft.md` | `max_depth` means path depth below the Origin. A Source with deeper sitemap pages loses them after the single-crawl change. |

While approving `design_draft.md` the author also kept the draft's narrower scope on three points this brief had listed for work A: the test server gets request times, counts and a wait only (response sequences, an in-flight counter and the AWS `robots.txt` layout move to works B and C); the raw `robots.txt` is not inspected during measurement; exact request counts come from the local site, and live sites get an estimate.

### Safe to carry into `/egm-draft` (work A)

- Test-server extensions first, then measurement with a stats value a test can assert.
- One crawl with injected URLs: `persist_links()` plus `set_extra_links` with the Origin included; correct the budget by one.
- A "crawl was cut off" signal computed in Citadel.
- Never share one concurrency semaphore between two crawlers.
- The Sync claim table is the existing choke point for any rule across Syncs.
- Retries and pacing are designed together, not as separate tasks.
- No schema change. `Docs/SPEC.md` changes in the same commits as the behaviour.
- Do not copy axon's concurrency numbers.

Not safe yet: the 25 to 50 ms range, any 4× or "under 47 s" target, the one-day clock estimate, and any comparison of stored URIs on `dlthub`.

### Direction from the author (2026-11-24)

- **Do work A now.** The brief is split: A goes to a design draft (`design_draft.md`).
- **For B and C, do what Firecrawl and crawl4ai do.** Their source was read on 2026-11-24 (Firecrawl commit `4924105`; crawl4ai 0.9.5, the author's installed copy). Nothing was run.
- **This reverses decision 2.** Neither tool applies `Crawl-delay`: Firecrawl parses and logs it, and the line that would apply it is commented out (`apps/api/src/controllers/v2/crawl.ts:397-400`); crawl4ai never reads it. Asked to choose between "obey in full" and "the same as those tools", the author chose **the same: ignore `Crawl-delay`, obey `Disallow`.**

**What the two tools do**

| | Citadel today | Firecrawl | crawl4ai 0.9.5 |
|---|---|---|---|
| `robots.txt` checked by default | Yes, but spider drops rule groups that contain an empty line | Yes | No (`check_robots_txt = False`) |
| Parser and empty lines | spider's own; an empty line ends the group | `robots-parser` 3.0.1 and Rust `texting_robots` 0.2.2; both skip empty lines | Python `urllib.robotparser`; skips empty lines |
| `Crawl-delay` | Applied only when the parser keeps it | Parsed, logged, not applied | Not read |
| `Disallow` covers | Links and injected URLs | Start URL, final URL after redirects, links, sitemap URLs | Every URL, when switched on |
| A blocked URL is | Skipped silently | Recorded and listed in the crawl's errors | Returned as a failed result, "Access denied by robots.txt" |
| `robots.txt` fetch fails | — | Treated as "no rules" (8 s timeout) | Treated as "allowed" (2 s timeout) |
| `robots.txt` cache | Per crawler | Per crawl, 24 h | 7 days |
| Fetches at once | 4 | Self-hosted: 5 workers shared by all crawls (inferred) | 5 |
| Delay between requests | 100 ms | None by default | 100 to 400 ms, random, per site |
| On 429 / 503 | Stores an Error page | Nothing | Doubles the delay with ±25 % jitter, up to 60 s; multiplies by 0.75 after a success |
| `Retry-After` | Not used | Not used | Not used |
| Re-fetch of a failed page | None | 6 attempts through different fetch methods, no wait | None by default |
| One limit per host across crawls | No | No | No; the per-site state lasts for one call only |
| Pages saved as they arrive | No, one write at the end | Yes, each page is its own job | Only in stream mode |

**What "the same" means for work B (robots.txt)**

- Read `robots.txt` with a parser that skips empty lines, so the `Disallow` rules of sites like AWS take effect. Candidate: the `texting_robots` crate, which Firecrawl uses.
- Apply `Disallow` to the Origin, to the final URL after redirects, to followed links and to sitemap and `llms.txt` URLs.
- Do not apply `Crawl-delay`. Log it when a site sets one.
- Record blocked URLs and show them to the user, as Firecrawl does.
- If `robots.txt` cannot be fetched, crawl as if it had no rules.
- Consequence: no multi-hour Syncs. The section "What a very long Sync does to the rest of Citadel" no longer applies. `aws-graffana` stays near today's duration and stops being a politeness test.
- Risk accepted by this choice: a site that set a `Crawl-delay` may throttle or block Citadel.
- Rollout risk that remains: pages under a `Disallow` rule that were stored until now become stale.

**What "the same" means for work C (crawl rate)**

- Citadel's rate (4 at once, 100 ms) already matches crawl4ai's (5 at once, 100 to 400 ms). Firecrawl is not slower-and-politer; self-hosted it has no delay.
- The one behaviour to copy is crawl4ai's reaction to 429 and 503: double the delay up to 60 s, ease off after a success.
- So work C no longer contains a rate increase. Decision 4 (a smaller delay) has no support from either tool. A 4× gain, if it exists, has to come from work A.

**Left open by this comparison**

- Decision 3 (one pacing clock per host) was made to support "obey `Crawl-delay` in full". That reason is gone, and neither tool has such a clock. The author has not withdrawn it.
- Decision 4 (smaller delay): keep, or drop in favour of today's rate.
- Whether Citadel re-fetches a page after a 429 or 503. crawl4ai only slows down; it does not re-fetch.

### Open, the author decides

1. ~~Split this brief?~~ Decided: split, A first.
2. **Who reads robots.txt:** spider's parser, which drops the AWS rules, or a parser in Citadel. Covers `Disallow` too.
3. **Per-host rule:** an interleaving clock (4 to 6 days), or one Sync per host at a time (about one day). For the second: a host check on the Sync claim also covers a second process.
4. **What the politeness rule counts:** page starts only, or also retries, redirects, the `robots.txt` fetch and Citadel's sitemap and `llms.txt` fetches.
5. **May a site lower Citadel's delay,** or does Citadel use the larger of the two.
6. **Very long Syncs:** what happens when delay × pages exceeds the 6 h claim age (refuse to start, or change the claim rule); whether `add`, MCP and UI calls stay blocking; whether there is a cancel path; whether a crash may lose hours of work.
7. **Does the per-host rule cover `render_js` Sources.**
8. **Speed target:** a multiple of wall time or pages per second; judged on the local site, on live sites, or both; and acceptance that 4× needs a delay near 60 ms or more than 4 permits.
9. **"Zero failed pages":** the `dlthub` baseline has 2 pages that return 404. Proposed wording: no new failed pages, and none with 429, 5xx or transport errors.
10. **Confirm or reject proposals 8, 9 and 10** in the table above, and define `max_depth` (path depth or link hops).

### Open, needs a measurement

1. A unit test that feeds the saved AWS `robots.txt` to spider's parser and checks `Crawl-delay` and a disallowed path. Confirms or refutes the code reading.
2. The delay in force after a crawl of `aws-graffana`: 100 ms or 5,000 ms.
3. Requests per stored page, seed crawls started, and median and tail fetch time, on `dlthub` and the local site.
4. Two back-to-back runs of `dlthub` on fresh vaults, compared by stored URIs.
5. Whether any dropped-page warning occurs today.
6. On the test server: gap between requests after 503 then 200 with one retry; `max_pages` N with injected URLs delivers N-1; an over-depth injected URL is dropped.
7. Whether a 5 to 60 s wait inside a fetch hook survives spider's first-byte watchdog.
8. Peak memory of a long Sync.

### Suggested task order

- **Fixed:** T1 test-server extensions → T2 phase timing and counters → T3 repeatable local benchmark site.
- **Conditional on T2's numbers:** T4a turn off spider's sitemap pass → T4b inject sitemap URLs into one crawl → T4c "crawl was cut off" signal → T4d depth rule.
- **After the author's decisions, with a new ADR on crawl rate and robots.txt:** robots.txt parsing in Citadel → per-host rule together with retries → delay value → long-Sync message and limits.

### Related existing findings

`WEB_INGEST_REVIEW.md` gaps 3 (lagging channel), 4 (no retries) and 9 (one crawl per sitemap URL); `PERFORMANCE.md` findings 1, 3, 5, 6, 20 and 23.

### Not verified

- The descriptions of axon, `spider_cli`, swiftide and centralized-docs come from one reading of their source by the author's assistant.
- The AWS `robots.txt` was fetched with a generic user agent, not as `citadel/0.2.0`.
- The "760" sitemap URLs were counted from `<origin>/sitemap.xml`, the file spider reads.
