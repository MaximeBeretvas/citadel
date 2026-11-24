# Stage 6: Comprehension Test

* **Target:** `Docs/egm/2026-11-24-sync-measurement-one-request-per-url-design.md` (372 lines; feature part 1 to 272, implementation plan 273 to 372)
* **Reviewer:** `egm-comprehension`, in a fresh context, reading the design alone
* **Date:** 2026-11-24
* **Result:** 0 Blocker, 6 Major, 11 Nit. The reviewer could paraphrase every section. The main gaps are core terms that are never defined and three places where the parts do not connect.

## What this proposes

Feature part (lines 1 to 272): Citadel will not be made faster. It will be made to report where a Sync spends its time and why some requests store no page, so that later speed work has numbers. There are four pieces:

- A Python script that turns a daemon log into one row per Sync: requests, pace, time before, during and after the logged requests, and a second line when the last part of a Sync is much slower.
- Seven drop counters and five durations, written as one `web_sync_summary` log event when a web Sync ends.
- A warning when a Sync hit `max_pages` and at least 5 pages, and more than 10 % of received pages, were discarded because they redirected out of scope.
- A bug fix: "this Sync was cut off by its page limit" is decided from pages received (plus pages the listener lost), not from the number of results, so a cut-off Sync no longer marks unvisited pages stale.

Implementation part (lines 273 to 372): five tasks, one commit each, tasks 1 to 4 test-first.

1. The script, its unit test, and a section in the measuring skill.
2. A crate-private `WebSyncAccounting` struct created in `sync_web_source`, passed by `&mut` through `execute_web_crawl`, `run_website_crawl` and `build_page_outcomes`, incremented at the existing drop points, and logged on both success and failure.
3. One changed comparison at `web.rs:308`.
4. The warning sentence pushed to the existing `warnings` list, plus one `warn` line per warning in the daemon's `log_sync_summary`.
5. Measured numbers written into five documents after a scratch-vault run of `dlthub` and `bench-uv`.

Verification is build, tests, lint, the script reproducing the eight request counts on the existing log, and one real `dlthub` Sync started by the author.

## How the existing system works, per this document

Citadel crawls documentation sites with the `spider` crate, converts pages to Markdown and stores them in SQLite; one run for a Source is a Sync. A Sync has these steps:

- Discovery: Citadel's own sitemap and `llms.txt` requests.
- A first crawl through spider, at about one request per 100 ms.
- A loop that builds and runs a new crawler, one after another, for each discovered URL the first crawl did not store.
- `build_page_outcomes`, which drops some received pages (unparsable URL, redirect out of scope, not storable, duplicate, binary) and converts the rest.
- `persist_sync_outcomes`, which writes in one transaction.

`max_pages` is a page budget. When the number of results reaches it, the Sync counts as cut off and unvisited stored pages are kept; otherwise they are marked stale. Discarded pages use budget but are not results, so the `dlthub` Sync of 11-23 looked complete and marked 197 pages stale.

The daemon runs two Syncs at once and writes `daemon.log`. spider's `fetch <url>` lines in that log are the only request-level evidence today, and they carry no Source name.

## Major findings

### 1. "Stale", "unvisited" and `--prune` are never explained
- **Lines:** 13, 64-70, 77, 167, 171, 251.
- **Gap:** the bug fix is about pages "wrongly marked stale", but the document never says what stale means for a user (hidden from search, deleted later, flagged?), what an "unvisited page" is, or what `--prune` does.
- **Effect:** the reader cannot judge how bad the 197 wrong marks are, or what "become active again" undoes. It is also open whether the 197 pages are ever repaired if later Syncs are cut off too and reach other parts of the site.
- **Fix:** two sentences near line 64 defining the three terms, and one sentence under Rollout on the pages already marked.

### 2. "Source" and "Origin" are never defined
- **Lines:** 11, 163, 189, 195, 366.
- **Gap:** "Origin" first appears at line 163 and carries the script's whole matching rule, with no definition.
- **Fix:** define both in the Overview, for example "A Source is a configured site with a start URL (its Origin), `max_pages` and include/exclude patterns."

### 3. Is the page budget per crawl or per Sync?
- **Lines:** 62, 68, 167-170.
- **Gap:** the text says the first crawl made 1,000 requests, "which is the limit", and that spider's limit is fixed when a crawl starts. That reads as a limit per crawl. The Sync total was 1,003, so later crawls add requests beyond it. The new rule then compares a Sync-wide count with `max_pages`.
- **Effect:** the reader cannot tell what happens when the first crawl ends under the limit and the later crawls push the total to `max_pages`. By the stated rule that Sync counts as cut off although nothing was cut off.
- **Fix:** say whether `max_pages` bounds the first crawl or the whole Sync, and whether pages from later crawls count in the cut-off test.

### 4. Logged requests are presented as pages received, and an unproven cause as fact
- **Lines:** 60, 116, 169, 191.
- **Gap:** "1,003 received" is the number of logged `fetch` lines; elsewhere the document says the two can differ. The example warning says "247 of 1,003 fetched pages redirect outside this Source", while line 60 says it is not established that all 247 are redirects.
- **Fix:** write "about 1,003" or "at least 1,000", and mark the example sentence as illustrative.

### 5. The warning does not cover the side effect it is said to cover
- **Lines:** 171, 175-181.
- **Gap:** the text says the warning fires "in exactly that situation" (a Source that keeps reaching its limit and so never marks removed pages stale). The warning counts only redirected pages, with two thresholds. A Sync that reaches its limit through other drop reasons, or with few redirects, has the side effect and no warning. A Sync that reaches its limit with no drops has the same effect today. The reason given for counting only redirects ("known to use a budget slot in both spider's and Citadel's counting") is not explained.
- **Fix:** say when the user is and is not told, and add one sentence on which drop reasons use budget.

### 6. Task 4's daemon log line has no fixed "done" check
- **Lines:** 282, 323, 359-368.
- **Gap:** the plan says it is tested "if the module has a test logger; otherwise by the manual check in the Verification Plan". The Verification Plan has no such check.
- **Fix:** decide the test approach in the document, or add an explicit manual step.

## Nits

7. **Lines 13, 58, 75, 133:** what a "discovered URL" is. The Overview says "sitemap pages", cause 2 says `llms.txt`. "First crawl" is used before the structure of a Sync is explained. Three sentences on a Sync's steps in the Overview would fix it; the diagram would serve better placed earlier.
8. **Lines 43-44, 78:** "Citadel's own sitemap requests" and "spider's own sitemap pass" appear without saying why both exist.
9. **Lines 49, 93, 242:** the 100 ms delay, the cap of 10 and "4 fetches at once" are not introduced as Citadel settings. The rates of Firecrawl and crawl4ai are not given.
10. **Lines 84, 132, 139-140, 148, 154:** "listener" is not explained; pages lost by the listener are by construction not "received", which contradicts G3's wording; the difference between "not crawlable" and "not storable" is not stated; counter 4 covers more than "redirect outside this Source".
11. **Lines 4, 13, 70, 78, 187, 195, 199, 230, 249, 253:** project terms used without a definition or a path: "review stage 5", "the lesson in `AGENTS.md`", "`render_js` re-checks", "the measuring skill", "scratch vault", "the Sync claim", "ADR-0001", "stdout purity", "Lite build".
12. **File name, title, lines 13, 203-207:** the file name says "one-request-per-url", the title says "Sync accounting". The first version of the design is described in terms the reader does not have.
13. **Line 77:** "why Syncs of the same Source store different page sets" is listed as unknown but followed by an explanation.
14. **Lines 22, 74, 195:** the Overview says the design "records which of three causes each Source has", but the measurement covers two Sources, and it is not stated how causes 1 and 3 are read from the output.
15. **Lines 75, 100:** "its sitemap lists 760 pages, so cause 2 is the likely one" skips a step: cause 2 applies only to discovered URLs the first crawl did not store.
16. **Lines 87, 283:** G6 names one file; task 5 writes to five.
17. **Line 315:** the "crate-private entry point that returns the record" is used by a test but not listed under the task 2 production changes.

--------------------------------------------------------------------------------

# Stage 6, second run: after the fixes

* **Target:** the same file after the fixes (389 lines; feature part 1 to 288, implementation plan 289 to 389)
* **Result:** 0 Blocker, 6 Major, 10 Nit. The reviewer could paraphrase every section. The Major findings are places where the reader had to guess to connect two parts.
* This is the one re-run the review allows for stage 6. None of the first run's Major findings came back; these six are new.

## What this proposes

Feature part (1 to 288): Citadel does not get faster under this design. It gets five things:

- A Python script that reads the daemon log and prints one row per Sync: requests, pace, time before, during and after the logged requests, and a split when the last part runs slowly.
- A per-Sync record of counts and timings inside Citadel: pages received, seven reasons a received page was not stored, number of later crawls, and five durations. It is written as one `web_sync_summary` log event.
- A warning added to the Sync result and the daemon log when a Sync hit its page limit and at least 5 pages, and more than 10 % of those received, redirected out of the Source.
- A fix to the "was this Sync cut off" test: it compares pages received (plus pages the listener lost) with `max_pages`, instead of comparing the number of results. A Sync that hit its limit while discarding pages then stops marking unvisited pages stale.
- A measurement run on a scratch vault, with the numbers written into the project's documents.

Implementation part (289 to 389): five tasks, one commit each, the first four test-first: the script; `WebSyncAccounting` threaded through four functions in `src/ingest/web.rs` plus the event; the one-line change of the cut-off condition at `web.rs:308` plus a `CONTEXT.md` edit; the warning plus a new `sync_warning_lines` function in the daemon; measured numbers in five documents.

## How the existing system works, per this document

- Citadel crawls registered documentation sites (Sources) with the `spider` crate, converts pages to Markdown and stores them in a SQLite vault. A daemon runs two Syncs at a time on a schedule; `citadel sync` runs one by hand.
- A web Sync has five steps: Citadel fetches `sitemap.xml` and `llms.txt` itself; one crawler starts at the Origin and follows links; one further crawler runs, in sequence, for each discovered URL the first crawl did not store; pages are converted; one database write follows.
- spider waits 100 ms between requests with at most 4 at once, so a crawl tops out at 10 requests per second. `max_pages` is a budget for the whole Sync; spider charges one slot per request handed out.
- Received pages can be dropped at several points. Today nothing counts these drops.
- A stored page missing from a completed Sync is marked stale, unless the Sync was cut off by `max_pages`. Today "cut off" is judged by the number of results, so a Sync that used its budget on discarded pages looks complete. That is how one `dlthub` Sync marked 197 pages stale.
- The log of eight Syncs shows no URL requested twice. It shows three patterns: pace pinned at the 100 ms delay, a slow tail from one crawler per page, and requests that store nothing.

## Major findings

### M1. The link between "requests handed out" and "pages received" is never stated, and the fix depends on it
- **Lines:** 80-82, 181-184, 197, 207.
- spider's budget counts requests handed out. The new cut-off test counts pages the listener received plus pages it lost. The design bridges the two with "so about 1,003 pages received", which is an inference.
- The reader cannot tell whether a request that times out or fails at network level yields a received page. If it does not, a Sync can hit spider's limit with fewer pages received than `max_pages`, and goal G5's "never" does not hold.
- **Fix:** state which request outcomes produce a received page, and either show that received plus lost equals requests charged, or weaken G5.

### M2. The `dlthub` Sync made 1,003 requests against a budget of 1,000, and the document does not say how
- **Lines:** 80, 82, 197.
- Later crawls get "what is left, counted in pages that are in scope", and a redirected page is "charged in both counts". Read together, nothing is left after the first crawl, yet 3 more requests appear. The paragraph that explains why only redirects are counted mixes two arguments.
- **Fix:** give the formula for "what is left" with the `dlthub` numbers, say where the 3 extra requests come from, and split that paragraph in two.

### M3. Cause 3 is listed as a cause of slow Syncs, but the document never says how it makes a Sync slow
- **Lines:** 32, 57, 70-74.
- The text for cause 3 is about budget slots. The one statement about pace (a quarter of gaps over 0.2 s, about 6 per second) has no stated link to the redirects.
- **Fix:** say that cause 3 costs coverage and, if it does, time; or say the drop in pace is unexplained and list it under "What is still unknown".

### M4. "Missing", "unvisited" and "not received" are used for the stale rule without saying whether they are the same set
- **Lines:** 18, 78, 100, 136, 181.
- A stored page that the Sync received and then dropped (it now redirects out of scope) is received but has no result. The reader cannot tell whether it is marked stale in a completed Sync, or protected in a cut-off one.
- **Fix:** one sentence defining the set the stale rule acts on, and what happens to a received-then-dropped page.

### M5. The log line formats the script must parse are not given, and task 1 is built before the event it reads exists
- **Lines:** 38, 177, 205, 295, 306, 311.
- No example of a `fetch` line, of the daemon's start and complete lines, or of the `web_sync_summary` event's field names. Task 1's test contains a hand-written summary line whose format is only fixed by task 2.
- **Fix:** one example line of each kind, and the event's field names.

### M6. The script's assignment rule has unspecified cases
- **Lines:** 205, 306.
- What happens to a `fetch` line whose URL matches no open Sync's Origin? Is "same Origin prefix" identical Origins, or one being a prefix of the other? `--single` has no stated use.
- **Fix:** state the rule for unmatched lines, define "not separable" exactly, and say when to use `--single`.

## Nits

1. **Lines 150, 155, 162:** counter 3 ("already collected") and counter 6 ("same URI seen already") are not told apart; "URI" is not defined.
2. **Lines 4, 23, 223-259, 275:** "review stage 5", "rejected in review" and "rejected by author" refer to a process the document does not describe.
3. **Lines 40, 51-55:** the table columns "Total" and "Per second" are not explained.
4. **Lines 19, 92, 110, 215, 269:** "static crawl" and `render_js` are used before being explained.
5. **Lines 23, 68:** the Overview says the slow part "fetches sitemap pages"; the evidence says the slow URLs come from `llms.txt`.
6. **Lines 100, 136, 181:** "`--prune` still wins" does not say what it wins; G5 and CUJ 3 do not say "without `--prune`".
7. **Line 187:** "only in one of these cases" is hard to parse; name the three cases.
8. **Line 322:** after the refactor it is unclear which function writes the event, how the record comes back when the Sync fails, and what a duration that was never reached logs.
9. **Lines 89, 107, 130, 203, 322-323, 339:** "the measuring skill", `SyncSummary`, `MissingPagePolicy::Preserve`, Firecrawl and crawl4ai are used before they are introduced; the source of the `aws-graffana` figure is not given; the warning says "fetched pages" where the rule uses pages received.
10. **Lines 353, 364:** the "corrected spider line reference" does not say what was wrong; "each in its own commit" for the SPEC edits conflicts with "five tasks, one commit each".
