# Stage 8: Intermediate Zoom Out Review

* **Target:** `Docs/egm/2026-11-24-sync-measurement-one-request-per-url-design.md` (449 lines)
* **Reviewer:** `egm-zoom-out`, in a fresh context, with the reports of stages 5 to 7
* **Date:** 2026-11-24
* **Result:** 0 Blocker, 3 Major, 4 Nit. Nothing was compiled or run.

**Overall:** the design is about the right size. The goals now trace through the plan, Alternative E records the trade-off, and the stale-page fix belongs in this design. spider exposes no "ran out of budget" signal (`CrawlStatus` at spider `website.rs:1256` has no such state, and the budget field is crate-private), so inferring it is not over-engineering. The fixes from stage 7 introduced two contradictions and one test plan that cannot pass.

## Major findings

### M1. The redirect tests for tasks 3 and 4 cannot produce a redirected page on the local test server
- **Lines:** 361-362, 394-397.
- **Problem:** Citadel uses the strict redirect policy for private hosts (`web.rs:421-425`), and the test server runs on `127.0.0.1`. Under that policy spider refuses any redirect to an internal address and does not follow a redirect to another host (spider `website.rs:2877-2935`, `2982-2983`, `2997-3005`); the loose policy makes the same internal-address check. So no served page can come back redirected off the Source, and none reaches `redirected_out_of_scope`. The "redirecting" pages come back as failed or stored results, which already count today. The claim that the task 3 test "fails today" is false, and the task 4 warning tests cannot reach the warning.
- **Fix:** make the cut-off decision and the warning pure functions of the accounting record and `max_pages`, and unit-test them with hand-made numbers. Use the `dlthub` numbers (first crawl 1,000 pages received, 756 results, `max_pages` 1,000) as the case that fails today. Keep one served-site test for the stale-marking wiring, driven by a drop reason a local site can produce. The planned 302 route in the test server is then not needed.
- **Evidence:** the redirect policy code was read; how a refused redirect becomes a page is inferred.

### M2. Condition 3 of the cut-off rule counts pages fetched twice, which the design says it must not
- **Lines:** 219, 222, 228, 267, 387, 396.
- **Problem:** each later crawl is a new crawler that knows nothing of earlier ones, follows links, and charges its limit for every page it admits, including pages the first crawl already collected; those are discarded only afterwards (`web.rs:383`). Condition 3 ("a later crawl delivered as many pages as the remainder it was given") therefore counts re-fetches. On a site with a navigation sidebar and one sitemap page the first crawl missed, the later crawl walks the site again, reaches its remainder, and the Sync counts as cut off on every run. That is the over-count Alternative E rejects. The error keeps pages, so it is the safe direction, hence Major and not Blocker.
- **Fix:** drop condition 3. After the later-crawl loop, check once whether `count_scoped_pages(&collected_pages, scope) >= max_pages`: the existing helper the loop's own budget check uses (`web.rs:362`, `646-657`). It counts distinct storable requested URLs among collected pages, and re-fetches cannot inflate it because they are discarded before collection. That covers both places the loop stops on its budget and the case where the last later crawl uses up the remainder. The rule becomes three conditions: the first crawl received `max_pages`, that count reaches `max_pages`, or today's condition. Alternative E's claim that distinct requested URLs "need URLs that spider does not report" must be reworded.
- **Evidence:** code path read; that it happens on real Sources is inferred.

### M3. The script's fallback host comes from a line written at the end of the Sync, but the script reads the file once
- **Lines:** 167, 271, 370.
- **Problem:** until a Sync has a `fetch` line, attribution uses the Origin from its `web_sync_summary` line, which is written after all its `fetch` lines. A single pass cannot use it. Also, a scheduled Sync that fails writes `scheduled web sync failed` (`src/daemon/mod.rs:464`), not a complete line, so under the stated rule it never closes.
- **Fix:** task 4 already changes the daemon. Add `origin=` to the daemon's start line (`src/daemon/mod.rs:460`) and close a Sync on either its complete line or its failed line. The summary line is then needed only for its numbers, and `--origin` only for old logs.

## Nits

1. **Lines 169, 359, 370:** the change-of-pace rule duplicates what G2's timings measure directly; it only helps on old logs. Drop it, or limit it to `--single` and logs with no summary line.
2. **Line 62:** the `developer-cisco-com-meraki` row is slightly off: "Before first line" 9.0 s (not 10.0), "After last line" 3.6 s (not 3.7), duration 186 s (not 187). The request count, crawl time and results are right.
3. **Line 238:** the warning example still divides by 1,003 logged requests while the rule divides by pages received (raised in stage 5, fix did not take).
4. **Line 218:** condition 1 leaves out pages lost when the receiving task falls behind; rare, none in the logged Syncs. One sentence in Risks.

## Verified and inferred

- **Read in code:** the cut-off and loop logic (`web.rs:301-388`, `646-729`); spider's per-page budget, redirect policies and internal-address check, crawl states, requested versus final URL; that `honors_max_pages_and_include_override` asserts 2 pages added; the daemon's start and failed lines; that no existing skill script parses logs.
- **Checked in the log:** the `developer-cisco-com-meraki` row; no failed-Sync lines so far.
- **Inferred:** how refused redirects become pages; re-fetch over-counting on real Sources; lag in the receiving task.
