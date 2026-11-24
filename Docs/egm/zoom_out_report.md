# Stage 5: Zoom Out Review

* **Target:** `Docs/egm/2026-11-24-sync-measurement-one-request-per-url-design.md` (391 lines; feature part 1 to 289, implementation plan 290 to 391)
* **Reviewers:** `egm-zoom-out`, `egm-inventive-catalyst`, each in a fresh context
* **Date:** 2026-11-24
* **Result:** 0 Blocker, 7 Major, 8 Nit, plus one piece of evidence found after the review that changes the design's premise

Nothing was compiled or run by the reviewers. Claims about spider come from reading its 2.54.2 source. `web.rs` and `local.rs` are in `src/ingest/`; `website.rs` and `page.rs` are in the spider crate.

## Evidence found after the review: the answer to Q1 is already in the daemon log

Reviewer finding Major 2 said spider already logs one `fetch <url>` line per request, so Q1 might be answerable with no new code. The author's assistant checked `~/Library/Application Support/com.citadel.citadel/daemon.log`. The lines are there, with timestamps. No Sync was run for this.

**Scheduled Sync of `dlthub`, 2026-11-23 20:50:07 to 20:53:19 (192 s):**

| Measure | Value | Meaning |
|---|---|---|
| `fetch` lines | 1,003 | Requests spider started through its link queue |
| Distinct URLs | 1,003 | **No URL was requested twice** |
| Time from Sync start to first request | 5.7 s | Sitemap and `llms.txt` discovery |
| Time from first to last request | 164.6 s | 6.1 requests per second |
| Median gap between requests | 0.104 s | **The 100 ms delay sets the pace** |
| 90th-percentile gap | 0.295 s | |
| Gaps over 1 s | 5, adding up to 6.1 s | No long pauses |
| Time from last request to Sync end | about 22 s | Conversion and the database write |
| Pages stored or failed | 756 | **247 requests produced no stored page** |

What this shows for `dlthub`, from one run:

- **The "duplicate requests" explanation does not hold.** Every logged URL was requested once.
- **The "slow responses" explanation does not hold either** as the main cause. Requests start at almost exactly the configured delay.
- **The Sync is slow for three plain reasons:** the 100 ms delay (about 100 s of the 165 s), about a quarter of the requests going to pages that are not stored, and about 28 s outside the crawl (discovery, conversion, write).
- "3.9 pages per second" in the design's table is 6.1 requests per second with a quarter of them not stored.

Limits of this evidence:

- One run, one Source. `aws-graffana` and `developer-cisco-com-meraki` were synced from the command line, so their requests are not in this log.
- The `fetch` line is written for links spider admits from its queue. spider's own sitemap pass, the first page of each crawler, redirects and `robots.txt` fetches do not write it, so the true request count is somewhat higher.
- Whether any later crawl started during this Sync cannot be read from the log.
- Why 247 requested pages were not stored is unknown (out of scope, redirects, non-HTML, or pages lost by the listener).

**Consequence for the design:** phase 2 (one crawl with injected URLs) has no supporting evidence on the benchmark Source. Most of phase 1 (the stats record, the generated site, the benchmark) was designed to answer a question the existing log already answers.

## Major findings

### Major 1. `fetch_time` does not measure a fetch, and its test cannot fail
- **Raised by:** both reviewers, independently. **Lines:** 75, 132, 137, 153, 313, 337.
- **Problem:** `Page::get_duration_elapsed()` returns the time since a stored start moment, so it keeps growing until it is read. The design reads it after the crawl, so each value is "time since that page's request began". The test asserts only "at least 200 ms", which a late read always meets. Q5 would be closed on a false pass.
- **Fix:** drop `fetch_time`, or read it in the listener when each page arrives, store a fixed duration and give the test an upper bound.
- **Evidence:** read in code (`page.rs:2453-2455`, `7377-7382`).

### Major 2. A zero-code way to answer Q1 was missing from the alternatives
- **Raised by:** `egm-zoom-out`. **Lines:** 39, 47-52, 188-216, 294-299.
- **Problem:** spider logs `fetch <url>` at info level for every link it admits. Counting those lines per URL answers Q1 before any code is written.
- **Status:** confirmed and carried out; see the evidence section above. Line 39 of the design ("Nothing in Citadel can tell them apart today") is wrong.

### Major 3. Injecting all discovered URLs changes who gets the page budget
- **Raised by:** `egm-inventive-catalyst`, who suggests treating it as a Blocker. **Lines:** 168-170, 178-182, 329.
- **Problem:** today the Origin crawl runs first, so pages reached by links take the budget and discovered URLs get the rest. With injection, the discovered URLs are the first wave, taken from an unordered set. For a Source whose sitemap is larger than `max_pages`, an arbitrary part of the sitemap takes the budget, and the Origin page itself may not be fetched. Not listed under "Known behaviour changes"; conflicts with goal G8.
- **Fix:** two runs on one `Website`: crawl from the Origin as today, then inject only the discovered URLs not yet received and crawl again on the same `Website`. Each URL is still requested once, link pages keep priority, and the `max_pages + 1` offset is not needed.
- **Evidence:** today's order and spider's first-wave handling read in code; that a second run on the same `Website` re-reads nothing is inferred.

### Major 4. The "crawl was cut off" flag does more than its reasons support
- **Raised by:** both reviewers, with opposite fixes. **Lines:** 76, 170, 174, 250-254, 300, 327.
- **Problem:** "any discovered URL never received" switches the whole Source to "keep unvisited pages". One permanently blocked sitemap URL turns stale detection off for every page of that Source. The design names this as Q6 and leaves it open. Line 174 also says "either of these" and lists three conditions.
- **Fix, author decides:**
  - Smaller (zoom-out): keep the existing count check, adjusted by an offset; add other conditions only when a test shows a cut-off crawl being missed; move "sitemap fetch failed" to its own task.
  - Per URL (catalyst): keep whole-Source "keep" only for "budget ran out" and "sitemap fetch failed"; for "discovered but not received", exempt only those URLs and mark everything else stale as normal. This changes `src/ingest/local.rs`, which the design lists as unchanged.

### Major 5. Pages lost by the listener are counted but not acted on
- **Raised by:** `egm-inventive-catalyst`. **Lines:** 53, 130, 174.
- **Problem:** a page the listener lost is absent from the results, and an absent page is marked stale. The design counts lost pages but does nothing with the count.
- **Fix:** treat "pages lost is greater than zero" as a cut-off condition and add a Sync warning. This pulls in the opposite direction from the smaller fix for Major 4.

### Major 6. Phase 2 is specified in implementation detail although it may never be built
- **Raised by:** `egm-zoom-out`. `egm-inventive-catalyst` disagrees: detail belongs in the implementation part. **Lines:** 71, 292, 300-303, 327-330, 339-341, 357-367.
- **Problem:** line 292 says tasks 5 to 8 are written out only after task 4, but the same section gives them return types, seven named tests and line numbers.
- **Fix:** keep phase 2 in the Technical Plan; reduce tasks 5 to 8 in the implementation plan to their table rows.

### Major 7. The local benchmark may not reproduce the duplicates it is built to show
- **Raised by:** `egm-zoom-out`. **Lines:** 37, 143-146, 148, 338.
- **Problem:** the generated site puts its sitemap at `/docs/sitemap.xml` under Origin `/docs/`. spider's own sitemap pass may read that same file and deliver the sitemap-only pages in the first crawl, so the later-crawl loop skips them all. The benchmark would show about one request per URL whatever live Sources do.
- **Fix:** state which sitemap location spider's pass reads; give the generated site a variant that spider's pass does not cover (an `llms.txt` list or a host-root sitemap); state the expected baseline before running.

## Nits

1. **Lines 170, 174, 253:** Alternative J and item 8 use `max_pages - 1`, the arithmetic from before the `+1` offset in item 6. Rewrite both once the offset is settled. (`egm-zoom-out` grades this Major, as part of its case against the flag.)
2. **Lines 232-236:** Alternative G's rejection reason is inaccurate. A page limit of 1 skips `robots.txt` by default, but spider has `with_respect_robots_txt_single_page(true)`. Base the rejection on the other drawbacks.
3. **Lines 54, 176, 235:** today's loop already enters that single-page mode when one page of budget remains, and so skips `robots.txt` for that page. Phase 2 removes this for static Sources. Add it to "Known behaviour changes".
4. **Line 292:** task 6 before task 7 creates an intermediate commit that is slower than today. Swap the order: inject first, then turn off spider's sitemap pass.
5. **Lines 176, 247:** `AGENTS.md:52` says "one private instance per `render_js` crawl". One crawl with one Chrome meets that wording, so Alternative I's stated drawback is wrong; the other reasons for deferring it stand.
6. **Lines 329, 341:** the 25,000-URL truncation helper and its unit test look speculative; `max_pages` tops out at 5,000 and child sitemaps at 10. A one-line warning is enough.
7. **Lines 48, 131, 307-320:** a new module for one struct is not needed if Majors 1 and 2 shrink the struct. `delay_in_force_ms` only helps if a measured site sets a `Crawl-delay`.
8. **Lines 106, 362:** `execute_web_crawl` starts at `web.rs:330`, not 329. "seed" stays in code identifiers although `CONTEXT.md` lists it as a word to avoid; state that as a known gap.

## Areas with no findings

- "Measure first, build phase 2 only if Q1 says so" is the right shape (both reviewers).
- Logging through `tracing`, no schema change, the lite build, and leaving `render_js` out all fit the project's rules.
- Alternatives A to F hold. spider's line and function references in the design are accurate.
- The description of today's later-crawl loop matches the code.

## Not verified

- Which sitemap URL spider's own pass requests; that `max_pages + 1` is the right offset; Alternative H's claims about `Website::queue()`.
- The reviewers did not read `PERFORMANCE.md`, `WEB_INGEST_REVIEW.md`, ADR-0001 or `Docs/resource-usage.md`, and did not check the performance table at lines 25 to 30.

--------------------------------------------------------------------------------

# Stage 5, second run: on the reworked design

* **Target:** the same file after the rework (281 lines; feature part 1 to 200, implementation plan 201 to 281)
* **Reviewers:** `egm-zoom-out`, `egm-inventive-catalyst`, each in a fresh context, with read access to `daemon.log`
* **Result:** 0 Blocker, 7 Major, 9 Nit. Six of the seven Majors were raised by both reviewers independently.
* This is the one re-run the review allows for stage 5.

**Overall:** the design is about the right size, and its evidence table matches the log wherever it could be checked (request counts, totals, the before/crawl/after split). The problems are in what it concludes from the table. Three statements are contradicted by the log or by spider's source. The log also shows two things the design does not mention.

## Major findings

### M1. The log shows the one-after-another crawl loop running, and it explains the slow `bench-uv` runs
- **Raised by:** both. **Lines:** 38-40, 47, 62, 72, 146-147, 197, 257.
- **Problem:** the design guesses that the slower runs are 4 busy fetches, and says the log cannot show whether a Source enters the loop. The `bench-uv` Syncs show the loop directly.
- **Checked again by the author's assistant against the log:**

  | Sync | First part | Last part |
  |---|---|---|
  | `bench-uv` 11-22 19:19 | 82 requests in 8.3 s (9.9 per second) | 57 requests in 19.8 s (2.9 per second); 55 of them are `…/index.md` URLs |
  | `bench-uv` 11-22 19:50 | 82 requests in 8.3 s (9.8 per second) | 57 requests in 16.3 s (3.5 per second); 55 are `.md` URLs |

  The last part is 40 % of the requests and about 70 % of the crawl time. Its order and pace match Citadel's loop that starts one new crawler per discovered URL (`web.rs:361-388`). That these are loop crawls is inferred; no log line labels them.
- **Consequences:** "no URL requested twice" is the wrong reason to drop the single crawl. The loop costs set-up time per crawler, not duplicate downloads. The planned note on `PERFORMANCE.md` finding 6 would record the wrong lesson. Conclusion 1 also holds for exact URL strings only: 55 of the 139 `bench-uv` requests are the Markdown twin of an HTML page already fetched, and both were stored.
- **Fix:** rewrite conclusion 3 and line 47. Split the crawl duration in the log event into "first crawl" and "later crawls" and log how many later crawls started. Have the script report requests and time on each side of the change of pace. Change the condition for bringing back the single crawl to "duplicates, or a large share of time in later crawls".

### M2. The `dlthub` Sync that hit its page limit marked 197 pages stale, and the design does not mention it
- **Raised by:** both. **Lines:** 41-42, 64, 116, 140, 156-160, 180, 194.
- **Problem:** Citadel decides "the crawl was cut off by `max_pages`" from the number of results (`web.rs:308`). Pages dropped after download are not results. This Sync made 1,000 requests in its first crawl but had 756 results, so Citadel treated the crawl as complete and marked 197 unvisited pages stale. That goes against `CONTEXT.md:45` ("A Sync cut short by its page limit leaves unvisited Pages as they were") and the lesson in `AGENTS.md`.
- **Fix:** at the least, record it as a finding and an explicit non-goal or open question. Better: a small fourth task that decides "limit reached" from pages received. The drop counters supply that number.
- **Side effect of the fix:** a Source that drops many pages keeps removed pages active until a Sync finishes under budget or runs with `--prune`.

### M3. The first page of each crawler does write a `fetch` line
- **Raised by:** both. **Lines:** 21, 34, 48, 226.
- **Problem:** the design says it does not. The first `fetch` line of every Sync in the log is the Origin. So "before crawl" includes the first page download, and "after crawl" includes requests still in flight plus spider's unlogged sitemap pass. The line marks when a request was handed out, not when it started.
- **Fix:** correct lines 21 and 48 and the planned `SKILL.md` text; define the two columns accordingly.

### M4. The cause of the 247 unstored pages is not established; conclusions 4, 5 and 6 are overstated
- **Raised by:** both. **Lines:** 41-43, 49, 88, 153, 158, 276.
- **Problem:** one page was fetched by hand and all 247 are attributed to the redirect. About 463 other `/docs/pipelines/` pages were stored in the same Sync, so those pages do not all redirect. "247 in-scope pages were never reached" is an upper bound on budget slots, not a page count. "This also explains why two Syncs stored different page sets" does not follow: three Syncs minutes apart on 11-22, with no drops, already stored different sets. "Time after the crawl grows faster than the page count" rests on one run.
- **Fix:** word each as what was observed; let the drop counters settle the cause.

### M5. The warning rule contradicts its own test and fires when no budget was lost
- **Raised by:** both. **Lines:** 56, 88, 126, 209, 233, 252.
- **Problem:** one place adds "at least 5 pages"; the test expects a warning for 2 of 10. The warning says dropped pages "count toward `max_pages`" even when the crawl ended well under the limit. That sentence is right for redirected pages and only partly right for out-of-scope pages. The example uses requests as the denominator; the rule uses pages received. The warning does not say what the user can do (exclude patterns stop spider from requesting a URL).
- **Fix:** state the rule once, with the floor; use test numbers that meet it; warn only when the limit was reached; base it on redirected pages; name exclude patterns.

### M6. The script cannot separate two Syncs that run at the same time
- **Raised by:** both. **Lines:** 130, 207, 216, 221, 275.
- **Problem:** the daemon runs two Syncs at once and `fetch` lines carry no Source name. The table's numbers came from splitting by host, which the design does not say. Two Sources on one host (`dbt-best-practices` and `dbt-semantic-layer`) cannot be split that way.
- **Fix:** state the rule (URL prefix of the Origin), add an overlapping pair to the test log, and flag rows that cannot be separated.

### M7. The drop counters miss a drop point
- **Raised by:** both. **Lines:** 49, 55, 99-107, 114, 232.
- **Problem:** pages from later crawls are dropped in `execute_web_crawl` before the counting loop runs, when an earlier crawl of the same Sync already collected them (`web.rs:383`). That is exactly the "downloaded twice" case the first version of the design asked about.
- **Fix:** add an "already collected" counter there.

## Nits

1. The text says five drop reasons, the diagram shows five plus the listener, the struct has six counters; one `continue` in the loop is the Failed outcome and must not be counted.
2. "Redirected out of scope" also covers redirects to excluded patterns and static assets.
3. The listener-loss counter duplicates an existing warning and was 0 in all eight Syncs; cheap to keep.
4. Logging the summary on a failed Sync needs a way to carry the timings out of a function that returns early; either log on success only or pass a record in.
5. `--json` on the script has no user yet; drop it.
6. The task 2 test may be unable to produce an "out of scope" page from a plain link, because spider blocks such links before requesting them. Use the Origin page with include patterns, or unit-test the loop with hand-made pages.
7. Other requests the log does not show: Citadel's own `llms.txt` and sitemap fetches, spider's sitemap pass, `render_js` re-checks.
8. The measuring skill forbids syncing against the user's vault, so the `dlthub` measurement needs a scratch data directory; its rules also require the README table to be updated; the daemon may not show Sync warnings at all.
9. "Halving the delay would roughly halve the crawl time" assumes 4 fetches at once would not become the limit; the design is labelled "reworked after stage 5" while this is stage 5.

## Claims checked and found correct

Eight Syncs, four Sources, 2,859 `fetch` lines in total; 756 pages stored or failed in the `dlthub` Sync of 11-23; the 100 ms sleep at spider `website.rs:8488-8490`; delay 100 ms and 4 fetches at once; the five drop points inside the loop and their line numbers; that a command-line Sync writes the same `fetch` lines to stderr.

## Not verified

The "median gap" and "requested twice" columns (the reviewers had no way to compute them; sampled gaps were 0.101 to 0.105 s); `max_pages` for `dlthub`; the manual redirect fetch and its 372 KB size.
