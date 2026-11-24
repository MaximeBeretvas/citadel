# Goldfish Review Report: Stage 9 re-audit, Implementation Readiness Test

**Target:** `Docs/egm/2026-11-24-sync-measurement-one-request-per-url-design.md` (lines 354-495, "## Detailed Implementation Plan"; lines 1-353 read as context)

**Standard:** `implementation-readiness-standard`. This is the last re-audit allowed. It checks the "Stage 9 resolution" in `goldfish_review_report.md` against the 9 Majors of the previous report.

## Reviewer Status

| Reviewer | Status |
| --- | --- |
| `egm-implementation-readiness` | completed |
| `egm-codebase-details` | completed |

Nothing was compiled or run. "Read in code" means a reviewer, or the synthesizer, read the cited source lines. "Inferred" means the point was reasoned from code or docs but not run.

**Verdict:** close to ready. There are 0 Blockers, 1 Major and 5 Nits. All 9 previous Majors hold. Both reviewers found the same new Major on their own: task 2's verbatim struct adds four fields that no code reads until task 3 or 4. The fix is to say which task adds which field. Once that is done, every task can be implemented from the plan as written.

## Do the previous Majors hold?

| Previous Major | Holds | Evidence |
| --- | --- | --- |
| M1: the served test passed on today's code | Yes | Line 441, named in the table at line 362. See trace A below: on task 2's code the test gives `pages_stale == 1`, and after task 3 it gives 0, through condition 1 alone. |
| M2: tests could not read the record | Yes | Line 436. `SourceLifecycle::add(params, false)` returns `Added { source, .. }` without syncing (`sources/mod.rs:88-96`, used as at `web.rs:2340-2354`). `start_sync_run` only needs that row (`web.rs:296-299`). The tests sit in `web.rs`'s own module, so they can call private functions. `SyncSummary` stays unchanged (`local.rs:14-25`). |
| M3: `WebSyncAccounting` was not defined | Yes | Lines 393-424 give the struct verbatim with types, name the four fields that are not printed, set task 2's `cut_off` from today's rule, and say an invalid Origin returns the default record and logs no line. The printed fields match the log line at lines 205-209. Fixing M3 caused the new Major 1. |
| M4: signatures and timing owners | Yes | Lines 424-427 give all four signatures. `execute_web_crawl` owns `conversion_ms`; `sync_web_source_with_accounting` owns `write_ms`. The stall path is decided. Every changed function is private to `web.rs` and has no outside caller (`web.rs:301, 353, 378, 394`). One compile detail remains (Nit 1). |
| M5: the drop test could not be written | Yes | Lines 438-439 follow option (a). `Page` derives `Default` (spider `page.rs:2424-2426`), `set_url` is `pub` (`page.rs:7061-7062`), and the five drop points are at `web.rs:670, 676, 682, 687, 707`. The identity holds: every page is either dropped once (at 380/383, or at 670-707) or becomes exactly one Ready or Failed outcome (`web.rs:668-731`). |
| M6: SPEC and CONTEXT stated the rejected rule | Yes | Lines 464 and 469 give the three-condition sentence. |
| M7: "every web Sync logs a line" | Yes | Lines 107 and 464 now say "with a valid Origin" / "whose Origin is a valid URL". |
| M8: "Dropped page" had two meanings in the brief | Yes | Line 479 renames it to "Lost page" and "seed crawl" to "later crawl". This covers `concepts_brief.md:29` and the "seed crawl" uses at 46, 77, 79, 129, 151 and 291. |
| M9: an extra live `dlthub` Sync | Yes | Line 491 removes it; the served red test covers the fix. |

## The focus checks

### A. Is `cut_off_sync_with_dropped_page_marks_nothing_stale` red today and green after task 3? Yes.

Both reviewers traced the fixture and got the same result. The synthesizer re-read the key lines.

1. **No later crawls.** The fixture server returns 404 for every path it was not given (`web.rs:1343`). So `/llms.txt`, `/docs/llms.txt` and both `sitemap.xml` candidates return 404, `discover_seed_urls` (`web.rs:838-889`) finds no seeds, and no later crawl runs.
2. **Spider admits the right pages.**
   * The Origin `/docs/` is crawlable through the exception at `web.rs:188` and the allow-list regex `^auth/docs/?($|\?|#)` (`web.rs:240-248`).
   * `docs/p*` becomes `^auth/docs/p.*` (`web.rs:1199-1231`).
   * `with_limit(3)` sets a `"*"` budget (spider `configuration.rs:1788-1789`). The seed is not charged (`website.rs:4506-4508`, `2223-2240`). The wildcard counter admits N-1 links (`website.rs:2335-2346`).
   * So the first crawl delivers the Origin plus 2 p pages: 3 pages. Two existing tests agree: `max_pages` 2 gives `pages_added == 2` (`web.rs:2376-2392`), and `max_pages` 3 gives 3 (`web.rs:2143-2156`).
3. **The Origin is dropped.**
   * The Origin is not storable: `"docs/"` does not match the glob `docs/p*`, and its Origin-relative path `""` is skipped at line 145 (`web.rs:139-150, 198`; `local.rs:41-53`). So it is dropped at `web.rs:682`.
   * That leaves 2 outcomes.
4. **On task 2's code:** `2 >= 3` is false at `web.rs:308`, so the Sync uses `MarkStale`. `reconcile_missing_pages` marks every missing `active` row stale, with no scope filter (`local.rs:555-594`). So `/docs/archived` becomes stale: `pages_stale == 1`.
5. **After task 3:** `is_cut_off(3, 2, 2, 3)` is true by condition 1 only, so the Sync uses `Preserve` (`local.rs:488`) and `pages_stale == 0`.

Because only condition 1 fires, the test also catches an `is_cut_off` that is never wired in, and `first_crawl_*` fields that are never set.

"Red today" in practice means "red at the start of task 3". Before task 2 the test does not compile. This is not a finding.

### B. Do the new signatures compile in principle? Yes, apart from Nit 1.

* **The collector.** `tokio::spawn(async move { ... (pages, lost) })` gives a `JoinHandle<(Vec<Page>, usize)>`, since the tuple is `Send + 'static`. The `collector.await.map_err(...)` at `web.rs:585-587` still works.
* **Type conversions.** `skipped` is a `u64` (`web.rs:548`) and needs a conversion to `usize`. `get_delay()` returns a `Duration` (spider `website.rs:2714-2716`) and needs `.as_millis()` converted to `u64`. Both are trivial.
* **`&mut WebSyncAccounting` across `.await`** in `execute_web_crawl` is fine.
* **Callers.** `sync_web_source` keeps its signature, so `sources/mod.rs:279`, the daemon and the re-export at `ingest/mod.rs:21` do not change. This matches the not-changed list at line 483.
* **Task 4's warning reaches the daemon.** `persist_sync_outcomes` copies `warnings` into `SyncSummary.warnings` (`local.rs:289-295`), so the warning reaches `sync_warning_lines`.

### C. Is `already_collected >= 2` sound? Yes, with room to spare.

* **The setup.** The first crawl collects the Origin, `a` and `b`.
* **Two later crawls run.** `h1` and `h2` are not yet visited (`web.rs:366-369`). The budget used is 3 or 4, far below the default `max_pages` of 500 (`ingest/mod.rs:23`). The default `max_depth` of 5 allows the hop back to the Origin (`web.rs:419`).
* **Each re-fetches 3 pages.** Each later crawl is a new `Website` with an empty visited set (`web.rs:376-378`). It follows the back-link to the Origin and then `a` and `b`, and all three are discarded at `web.rs:383`.
* **Expected values.** `later_crawls_started == 2`, `already_collected == 6`, `pages_received == 11`, `results_stored == 5`, and the identity gives 5 + 6 = 11.

### D. Do the `is_cut_off` inputs isolate their conditions? Yes.

* **Each test makes only its named condition true.**
  * `(1000, 759, 756, 1000)`: condition 1 only
  * `(40, 100, 90, 100)`: condition 2 only
  * `(2, 2, 3, 3)`: condition 3 only
  * `(40, 60, 60, 100)`: no condition
* **Wrong implementations fail.** The implementation-readiness reviewer tried the obvious ones by hand: a dropped condition, `>` for `>=`, swapped arguments, and always-true. Each fails at least one test.
* None of the cut-off cases differs from the baseline in only one input. That is optional hardening (Nit 2).

## Prioritized Findings (Blockers -> Major -> Nits)

### Major 1. Task 2's struct has fields nothing reads, so its commit fails `clippy -D warnings` - Severity: Major
* **Reviewer Lens:** `egm-implementation-readiness`, `egm-codebase-details`. Both found it on their own.
* **Affected Scope:**
  * Lines 393-421: the struct, all of it added in task 2.
  * Lines 401, 402, 404 and 413: the four "not printed" fields.
  * Lines 428 and 429: tasks 3 and 4, which first read those fields.
  * Line 489, and the checks gate in `CLAUDE.md` step 3.
* **Discrepancy Description**:
  * `WebSyncAccounting` is `pub(crate)`, so rustc's `dead_code` lint covers its fields. A field that is written but never read triggers "field is never read". A derived `Debug` is explicitly ignored by that analysis.
  * **After task 2:** `first_crawl_received` and `first_crawl_lost` are written but not read. `collected_storable_urls` (line 404, "set in task 3") and `redirect_examples` are not touched at all. None of the four is printed. Reads in the test module would not help, because clippy also lints the lib target without `cfg(test)`.
  * **After task 3:** `redirect_examples` is still unread, so that commit fails too.
  * The result is that tasks 2 and 3 each leave a red checks gate. That breaks line 356's "one commit each" and the standard's rule that every phase leaves the build green.
* **Rationale & Recommendation**: Keep the verbatim struct as the end state, and say which task adds which field:
  * `first_crawl_received`, `first_crawl_lost` and `collected_storable_urls` in task 3;
  * `redirect_examples` in task 4;
  * every other field in task 2.

  Do not use `#[allow(dead_code)]`.
* **Evidence:** Read in code: there is no `allow(dead_code)` in `src/**/*.rs` and no `[lints]` table in `Cargo.toml`, and the crate has both `src/lib.rs` and `src/main.rs`. Inferred, not compiled: that the lint fires on these fields.

### Nit 1. "Holds today's body" cannot compile under the tuple return type, and "not a valid URL" is ambiguous - Severity: Nit
* **Reviewer Lens:** both. The synthesizer checked it too.
* **Affected Scope:** Line 424.
* **Discrepancy Description**:
  * Today's body uses `?` at `web.rs:282-284`, 292, 297-298 and 303. A function that returns `(Result<SyncSummary>, WebSyncAccounting)` cannot use `?`.
  * The line also does not say which early checks count as "not a valid URL" and so log no line. There are two of them: the parse at 282 and the scheme/host check at 285-290. Errors from `WebScope::new` (292) and the database (297-298) should log a line with `ok=false`, since line 464 promises a line for every valid Origin.
* **Rationale & Recommendation**:
  * The outer function runs the checks at `web.rs:282-290`. On failure it returns `(Err, WebSyncAccounting::default())` and logs nothing.
  * Otherwise it calls an inner `async fn sync_web_source_inner(db_path, source, prune, &origin, acc: &mut WebSyncAccounting) -> Result<SyncSummary>` that keeps the `?`s. It then logs the line and returns the tuple.
* **Evidence:** Read in code: `web.rs:277-328`.

### Nit 2. The `is_cut_off` positive tests do not change one input at a time - Severity: Nit
* **Reviewer Lens:** both.
* **Affected Scope:** Line 442.
* **Discrepancy Description**: Each test isolates its condition (see D). Against the baseline `(40, 60, 60, 100)`, though, the three cut-off cases change 4, 2 and 4 inputs.
* **Rationale & Recommendation**: This is optional. Add `(100, 60, 60, 100)`, `(40, 100, 60, 100)` and `(40, 60, 100, 100)`, and keep the `dlthub` tuple as an extra case.
* **Evidence:** Read in the document.

### Nit 3. Fixture and format gaps in the accounting test - Severity: Nit
* **Reviewer Lens:** both.
* **Affected Scope:** Lines 206, 398 and 439.
* **Discrepancy Description**:
  * **`delay_ms`.** It is `Option<u64>` (line 398), but line 206 prints `delay_ms=<n>`. It is `None` when the Sync fails before the first crawl, and line 423 says only that a `None` *duration* prints `-`.
  * **Page paths.** Line 439 does not place `h1` and `h2` under the Origin prefix. That placement is required, because `discover_seed_urls` keeps only seeds that pass `should_crawl` (`web.rs:853`).
  * **`max_pages`.** The line does not say to keep the default `max_pages`. A small value would let the budget check at `web.rs:363` stop the later crawls.
  * **What a later crawl re-fetches.** "(each later crawl reaches the Origin again)" understates it: each also re-fetches `a` and `b`.
* **Rationale & Recommendation**:
  * Say that `delay_ms` prints `-` when `None`, and change line 206 to `delay_ms=<n|->` (task 1's parser accepts `-` there).
  * Name the paths, for example `/docs/a`, `/docs/b`, `/docs/h1` and `/docs/h2` under Origin `/docs/`.
  * Say that the test keeps the default `max_pages`.
  * Optionally assert `already_collected == 6` and `pages_received == 11`.
* **Evidence:** Read in code: `web.rs:363, 853` and `ingest/mod.rs:23`. The rest read in the document.

### Nit 4. The spider budget note at line 441 names the wrong mechanism - Severity: Nit
* **Reviewer Lens:** `egm-codebase-details`.
* **Affected Scope:** Line 441, the closing parenthetical. Line 230 says the same.
* **Discrepancy Description**: The parenthetical says spider counts the Origin page against `with_limit`. In fact spider never charges the seed. It admits `max_pages` − 1 links besides it, because the wildcard counter reports "over" at 1. The outcome, 3 pages, is the same, so the test is unaffected.
* **Rationale & Recommendation**: "spider admits `max_pages` − 1 links besides the Origin (spider `website.rs:2335-2346, 4506-4508`; read in code, not observed)."
* **Evidence:** Read in code (spider 2.54.2) by `egm-codebase-details`. The implementation-readiness reviewer inferred the same count from the existing test at `web.rs:2376-2392`.

### Nit 5. `redirect_examples` does not say which URL it stores - Severity: Nit
* **Reviewer Lens:** `egm-codebase-details`.
* **Affected Scope:** Line 429.
* **Discrepancy Description**: The stored URL could be the page's own URL or its `final_redirect_destination`. The task 4 tests build the record by hand, so they do not settle it.
* **Rationale & Recommendation**: Name it, for example "the page's own URL (`page.get_url()`)".
* **Evidence:** Read in the document.

## Verified and inferred

**Read in code:**
* `web.rs`: 115, 139-150, 181-258, 277-408, 419, 529-588, 646-731, 838-907, 1068-1080, 1165-1231, 1307-1363, 2143-2156, 2306-2426
* `local.rs`: 14-25, 30-88, 125-132, 289-295, 487-495, 555-594
* `sources/mod.rs`: 88-96, 279, 384-416
* `daemon/mod.rs`: 448-479, 590
* `ingest/mod.rs`: 21-24
* spider 2.54.2:
  * `configuration.rs:1227-1228, 1788-1791`
  * `website.rs:2151-2155, 2208-2240, 2335-2346, 2714-2716, 4506-4508, 8471-8476, 13226-13235, 14767-14775`
  * `page.rs:2424-2426, 7061-7062`
* `concepts_brief.md`: 29, 46, 77, 79, 129, 151, 291

**Inferred (not run):**
* That rustc's dead-code lint fires on the write-only fields (Major 1).
* That the first crawl delivers exactly 3 pages in the new fixture. This is supported by spider's budget code and by two existing test assertions.
* That the full URL `http://…/docs/` does not match the glob `docs/p*`.
* That the tuple-returning collector compiles.
* That the set of p pages spider admits does not change the counts. Any 2 of the 5 give the same numbers.
