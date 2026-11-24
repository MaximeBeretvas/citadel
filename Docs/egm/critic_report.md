# Stage 7: Critic Review, round 1

* **Target:** `Docs/egm/2026-11-24-sync-measurement-one-request-per-url-design.md`, feature part (lines 1 to 324)
* **Reviewers:** `egm-design`, `egm-structural`, `egm-style`, each in a fresh context; all three completed on the first attempt
* **Standard:** feature-spec standard (design and structural); the style reviewer's own rules
* **Date:** 2026-11-24
* **Result:** 0 Blocker, 18 Major, 29 Nit after merging duplicates

Finding ids are `<reviewer>-1-<n>`. `W` means spider `src/website.rs` (2.54.2); `web.rs` means `src/ingest/web.rs`; "log" means the daemon log the design cites.

The 18 Majors fall into three groups: 6 that change what gets built, 4 about the structure of the document, and 8 about wording.

## Majors that change what gets built

### design-1-1 (with structural-1-4): "Nothing is downloaded twice" does not hold
- **Lines:** 25, 42-51, 72, 88, 168, 257-259.
- **Problem:** the claim rests on `fetch` lines, which do not show spider's own sitemap pass. That pass runs after every crawl and requests `<start URL>/sitemap.xml`. On a 404 it sends a second request to the crawl's own start URL (`W:12376-12387`, `13101-13152`). So each later crawl whose probe returns 404 fetches its page twice, with no log line and no delivered page. The new counters cannot see it. The design also contradicts itself: it reports three second spellings of URLs and calls that "a second download", while saying elsewhere that nothing is downloaded twice.
- **Fix:** say "no URL string appears twice in the `fetch` lines". Define "twice" once (identical string: 0 seen; second spelling: 3). List the probe re-fetch as a known unlogged request, and measure it or mark it "not measured".
- **Evidence:** code path read; the three second spellings found in the log; that the `…/index.md` probes return 404 is inferred.

### design-1-3 (with structural-1-3, style-1-8): the "cut off" rule over-counts and cannot be checked from the output
- **Lines:** 106, 168, 174, 194-199, 208-216, 220-228, 241.
- **Problem:** `pages_received` includes pages later dropped as "already collected" or "same URI". A later crawl is a fresh crawler that can download pages again. A Source well under its limit in distinct pages could then count as cut off on every Sync and never mark pages stale, with no warning. The log event has no `max_pages`, "cut off" or policy field, so goal G5 cannot be checked. The design never states that pages received equals stored plus failed plus the drop counters. It compares no other rules.
- **Fix:** state that identity; subtract "already collected" and "same URI" from the count, or say why not; add the three fields to the event; log the silent case; add an alternatives entry; add a test where later crawls re-fetch collected pages.
- **Evidence:** code read. The over-count is inferred; no Sync in the log has such a later crawl. The proposed rule is safe in one direction: it never marks pages stale where today's rule keeps them.

### design-1-2: the script's Origin-prefix rule fails on a Source already in the log
- **Lines:** 102, 236-238, 289.
- **Problem:** the `dbt-semantic-layer` Sync fetched URLs under four paths with no common Origin prefix. Most of its 19 lines would go unassigned. The design's own table was made by splitting by host.
- **Fix:** assign by time window when one Sync is open, by host when two overlap, and use the Origin prefix only as a tie-break. Or log a start and end line per crawler, which also replaces the change-of-pace guess.
- **Evidence:** URL spread found in the log.

### design-1-4: comparing logged requests with pages received is confounded
- **Lines:** 23, 55, 98, 211, 241.
- **Problem:** when budget remains, spider's sitemap pass fetches unvisited sitemap URLs and delivers them, with no log line and no delay (`W:13217-13262`). Pages received can then exceed logged requests, which hides the shortfall the check is meant to find. The cap of 10 per second holds for the link crawl only.
- **Fix:** count sitemap-pass pages separately or state the limit of the check; say "caps the link crawl".
- **Evidence:** read in spider's code; not seen in the log.

### design-1-5: the delay actually in force is not recorded
- **Lines:** 23, 34, 63, 80, 194-199.
- **Problem:** spider replaces Citadel's delay with a `Crawl-delay` from `robots.txt` when its parser keeps one. A Source paced that way matches none of the three patterns.
- **Fix:** record the delay in force per Sync (`Website::get_delay()`).

### design-1-6 (with structural-1-5a, style-1-10): goal G4 and the warning's advice promise more than the rule delivers
- **Lines:** 31, 74, 80, 105, 134, 136, 218-228, 267-271.
- **Problem:** G4 speaks of "discarded pages"; the rule warns only for redirects, at 5 pages and 10 %. The advice "add an exclude pattern" assumes a pattern can separate the 247 unstored `dlthub` URLs from about 463 stored ones under the same path prefix. That is not shown. Two alternatives are missing: refuse out-of-scope redirect targets in the redirect policy, and give spider a limit above `max_pages` while capping stored pages.
- **Fix:** narrow G4; test an exclude pattern on a scratch vault; put several example URLs in the warning; add the two alternatives.

## Majors about the structure of the document

- **structural-1-1 (with design-1-7b), lines 120, 211, 215-216, 236-238, 299-305:** no "Risks and mitigations" section. Risks are spread over five places, and these have no mitigation or are not named: the silent case of the cut-off rule; dependence on spider's log line format; misassigned `fetch` lines; `--prune` during a cut-off Sync; credentials in a logged Origin or example URL; rotated or very large logs; the 1,500-request measurement against a host that asks for 5 s between requests.
- **structural-1-2 (with design-1-7a), lines 113, 253, 259, 295:** rejections are attributed to "the author" with no link to where the decision was made. `zoom_out_report.md` and `concepts_brief.md` are untracked files.
- **structural-1-5 (with style-1-7, style-1-12), lines 27-32, 98, 100-107, 124-142, 170-245:** goals, plan and user journeys do not trace. G6 has no plan content; item 6 has no actor, no values and no recording step; G5 and item 3 state different conditions; the Overview says "four things" and leaves out the measurement; the three lists use different orders; no journey covers the counters.
- **structural-1-6, lines 40, 42-51, 63, 94, 258, 293:** "all eight scheduled web Syncs" is out of date. The log now holds a ninth, `developer-cisco-com-meraki` on 2026-11-24, about 540 `fetch` lines with irregular gaps.

## Majors about wording

- **style-1-1:** four words for a received page with no result ("discarded", "dropped", "lost", "not stored"). "Not stored" also covers failed pages, so G3 misreads.
- **style-1-2:** "in scope" covers two different checks in the code (may be crawled, may be stored).
- **style-1-3:** "crawl" is used for Sync, which `CONTEXT.md` lists as a word to avoid, and the limit has several names.
- **style-1-4, lines 19, 72:** "did not store" misdescribes the later-crawl condition; the code skips URLs that any earlier crawl collected, whatever the outcome.
- **style-1-5 and style-1-6:** claims firmer than the design's own caveats: "three causes of slow Syncs" although the time cost of cause 3 is not shown; "`dlthub` is at 25 %" assumes all 247 are redirects.
- **style-1-9, lines 86-88:** the explanation of the 197 stale pages is interrupted by the "why 1,003" detail.
- **style-1-11, lines 236-240:** the script's rules: "not separable" is defined twice; the host fallback is circular; "open Sync" is undefined.
- **style-1-13, lines 4, 25, 253, 258, 265:** process history ("review stage 5", the first version) appears before it is explained.

## Nits (29)

- **Structure:** two `###` headings nest under Non-Functional Requirements; Q2 is already settled as out of scope; Q3 names no decider; the `aws-graffana` unknown is repeated; "Sync result" collides with "Result"; "site" and "database lock" are on the glossary's avoid list; the glossary bullets come before the summary; the budget counting is explained four times; `--prune` belongs in the "Given" of the journey; the file name still says "one-request-per-url".
- **Design:** 529 unchanged + 197 stale = 726, but the earlier Syncs added 715, so 11 pages are unexplained; "successfully or not" fails on early returns; an unreached part logging 0 cannot be told from 0 ms; the warning and the cut-off test are computed in different functions; values with spaces break the event's parsing; "Cost" and "Privacy" have no pass criterion; the "becomes active again" claim has no code reference; spider's sitemap pass looks under the crawl's start URL, not the host root.
- **Style:** one term per glossary bullet; "Page" capitalisation; "at most two" Syncs; dates lack year and zone; "Total" should be "Duration"; bold only the labels; overlong bullets; vague words ("accepted", "enough for now", "a handful").

## Claims the reviewers checked and found correct

- **In code:** the 100 ms delay and 4 fetches at once; when a `fetch` line is written; how the budget is charged; the drop points in the diagram; that `--prune` takes precedence; that the daemon runs 2 Syncs at once and does not log warnings; that ADR-0001 records no migration mechanism.
- **In the log:** all eight durations and request counts; the `dlthub` row of 11-23 (1,003 requests, 756 results, `stale=197`); cause 2 (55 consecutive `…/index.md` requests).

## Not checked by any reviewer

The "median gap" and "same URL twice" columns; the 247/463 split and the 372 KB redirect; the `aws-graffana` figures; whether every failed request reaches the listener; the Firecrawl and crawl4ai rates.
