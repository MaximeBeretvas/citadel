# Goldfish Annotated Review Report

**Target:** `Docs/egm/2026-11-24-sync-measurement-one-request-per-url-design.md` (feature part; stage 7, round 1)
**Findings:** `critic_report.md` (0 Blocker, 18 Major, 29 Nit)
**Mode:** one resolution pass, as agreed with the author: fix the findings that change what gets built and the structural ones, fix wording where it was wrong, record the remaining style points as declined. No further round of stage 7.

## Majors that change what gets built

| Finding | Decision | What changed in the design |
|---|---|---|
| design-1-1 "nothing is downloaded twice" | PROPOSE FIX, applied | The claim now reads "no URL string appears twice in the `fetch` lines". spider's 404 probe re-fetch of a crawler's start URL is listed as an unlogged request (verified in spider `website.rs:12376-12387`, `13096-13156`). |
| design-1-3 cut-off rule over-counts | PROPOSE FIX, applied differently | The count-based rule is replaced by four events that record a crawler running out of budget, OR'd with today's condition. Checked against three cases (`dlthub` 11-23, `honors_max_pages_and_include_override`, a re-fetching Source under budget). `max_pages` and `cut_off` are in the log line. The identity "received = results + drops" is stated and tested. Count-based rules are recorded as Alternative E. |
| design-1-2 Origin-prefix attribution fails | PROPOSE FIX, applied | Attribution is now by time window, then by host; same-host overlaps are marked "not separable". Verified that `dbt-semantic-layer` spans four paths. |
| design-1-4 sitemap pass confounds the check | PROPOSE FIX, applied | The comparison of logged requests with pages received is dropped from the measurement; the sitemap pass is listed as delivering pages with no log line and no delay. |
| design-1-5 effective delay not recorded | PROPOSE FIX, applied | `delay_ms` added to the log line. |
| design-1-6 G4 and the advice promise too much | PROPOSE FIX, applied | Checked offline: 29 of 64 `dlthub` pipeline groups contain both dropped and stored pages, so no URL pattern separates them. G5 (was G4) is narrowed to redirects; the warning suggests raising `max_pages` or excluding the section if not wanted, and does not promise a targeted pattern. The redirect-policy and higher-limit options are added to Alternative C. |

## Majors about structure

| Finding | Decision | What changed |
|---|---|---|
| structural-1-1 no risks section | PROPOSE FIX, applied | "Risks and Mitigations" table added. |
| structural-1-2 author rejections not linked | PROPOSE FIX, applied | Rejections now cite `concepts_brief.md` or `zoom_out_report.md`. Both files are still untracked; they must be committed with the design. |
| structural-1-5 goals, plan and journeys do not trace | PROPOSE FIX, applied | Overview, goals, plan items and tasks now use one order and cite goal numbers; CUJ 2 covers the counters; the measurement is numbered steps ending in `Docs/resource-usage.md`. |
| structural-1-6 "eight Syncs" out of date | PROPOSE FIX, applied | Ninth row added (`developer-cisco-com-meraki`, 540 requests, 3.1 per second, steady). Its cause is recorded as not identified. The log snapshot is dated (lines 1 to 3439). |

## Majors about wording

| Finding | Decision | What changed |
|---|---|---|
| style-1-1 several words for a page with no result | PROPOSE FIX, applied | "Dropped page" and "lost page" defined once and used throughout. |
| style-1-2 "in scope" covers two checks | PROPOSE FIX, applied | "Crawlable" and "storable" defined; counters renamed `url_not_storable` and `duplicate_result`. |
| style-1-3 "crawl" used for Sync; several names for the limit | PROPOSE FIX, applied | "Page budget" and "spider's limit" defined; "complete Sync" and "cut-off Sync" defined. |
| style-1-4 "did not store" | PROPOSE FIX, applied | "That no earlier crawl of this Sync collected". |
| style-1-5, style-1-6 claims firmer than the caveats | PROPOSE FIX, applied | "Three causes" became "four patterns"; dropped pages are stated as a budget cost, time cost not shown; the 25 % figure is removed. |
| style-1-9 interrupted causal chain | PROPOSE FIX, applied | The 197-stale explanation runs straight; the "why 1,003" detail follows as a separate, marked inference. |
| style-1-11 script rules unclear | PROPOSE FIX, applied | Rewritten with design-1-2. |
| style-1-13 process history too early | PROPOSE FIX, applied | Moved to Alternative A and the intro of Alternatives. |

## Corrections not raised by the critics

- **Second spellings are stored, not dropped.** The `dlthub` Source has 953 rows but 950 distinct URLs once a trailing slash is ignored. The design now says so and lists it as a separate, unfixed finding; counter 6 is renamed `duplicate_result` and no longer claimed to catch these.
- **Redirects land in `redirected_out_of_scope` for static crawls.** spider sets the redirect target from the HTTP response in its non-Chrome page builder (read in code, not observed at run time).

## Nits (29)

- **Fixed along the way:** the two `###` headings under Non-Functional Requirements; Q2 (the delay) removed as settled; Q2 (thresholds) names the author as decider; the duplicated `aws-graffana` unknown; "Sync result" renamed "Sync summary"; "site" in the Source definition; budget counting explained once; `--prune` in the CUJ "Given"; early-return case of the log line; "unreached" shown as `-`, not 0; quoting of text values; the "becomes active again" claim now cites `persist_sync_outcomes` (verified in `src/ingest/local.rs`); dates with year and UTC; "Total" renamed "Duration".
- **DECLINE (recorded, not fixed):** renaming the file (listed under Future Improvements, since links to it exist in the reports); the 11-page gap between 715 and 726 stored pages across days (not needed for this design); remaining sentence-length and bold-label style points.

---

# Stage 9 resolution (implementation readiness)

**Findings:** `implementation_readiness_report.md` (0 Blocker, 9 Major, 15 Nit). One resolution pass, as in stage 7.

| Finding | Decision | What changed in the plan |
|---|---|---|
| Major 1: served test green today | PROPOSE FIX, applied | Replaced by `cut_off_sync_with_dropped_page_marks_nothing_stale`: Origin `/docs/` with include `docs/p*`, so the Origin is dropped as `url_not_storable`; 3 pages, 2 results; an inserted `active` row is marked stale today and kept after task 3. Checked in code: `should_crawl` exception (`web.rs:188`), `should_store` (198), `matches_pattern_candidates` (139-150) does not match `docs/` against `docs/p*`. |
| Major 2: tests cannot read the record | PROPOSE FIX, applied | Tests call `SourceLifecycle::add(params, false)` then `sync_web_source_with_accounting` directly; `SyncSummary` unchanged. |
| Major 3: struct undefined | PROPOSE FIX, applied | Struct given verbatim with types; printed vs not-printed fields; task 2 sets `cut_off` with today's rule; invalid Origin returns the default record and logs no line. |
| Major 4: signatures and timing owners | PROPOSE FIX, applied | New signatures for `sync_web_source_with_accounting`, `execute_web_crawl`, `run_website_crawl` (returns lost count) and `build_page_outcomes`; `conversion_ms` owned by `execute_web_crawl`, `write_ms` by `sync_web_source_with_accounting`. Stall path stays an error (render_js only, unchanged by this design). |
| Major 5: drop test impossible | PROPOSE FIX, applied (option a) | Hand-made test covers the five reasons in `build_page_outcomes`; the served `llms.txt` test covers `already_collected` (hidden pages link back to the Origin) and asserts the identity end to end. |
| Major 6: SPEC/CONTEXT state rejected rule | PROPOSE FIX, applied | One three-condition sentence in both; CONTEXT gains a "Cut-off Sync" entry. |
| Major 7: "every web Sync logs" | PROPOSE FIX, applied | SPEC sentence and G2 say "whose Origin is a valid URL". |
| Major 8: brief's "Dropped page" | PROPOSE FIX, applied | The brief edit renames it to "Lost page" and "seed crawl" to "later crawl". |
| Major 9: extra live `dlthub` Sync | PROPOSE FIX, applied | Removed; the served red test covers the fix. |
| Nits 1-11, 13-15 | PROPOSE FIX, applied | unittest discover command; fixed `is_cut_off` inputs and renamed tests; 10 % threshold tests and named constants with the `u32` cast; `llms.txt` fixture rules; red test named; script test cases and table aligned with file list; daemon clone before line 449 and second caller; "cut off" / "page budget" terms; SPEC per task 2-4; README rule and `SPEC.md:562`; `cmd` feature wording; five Sources; conditions 1, 2 and 3; real summary line checked in task 5; spider claim marked "read in code". |
| Nit 12: `AGENTS.md` lesson | DECLINE | `AGENTS.md` stays on the not-changed list: the lesson's rule (Preserve on a capped crawl without `--prune`) still holds; it becomes broader, not wrong. Revisit in the Telemetry → learn step. |

## Stage 9 re-audit (cap reached)

All 9 previous Majors hold. One new Major (struct fields unread in tasks 2 and 3 fail `clippy -D warnings`): fixed, each field now lands in the task that reads it. Nits applied: inner `async fn` keeps `?`; `delay_ms` prints `-`; the `llms.txt` test uses default `max_pages`, hidden pages under the Origin path, exact `already_collected == 6` and `pages_received == 11`; `redirect_examples` stores requested URLs. Nit 2 (single-input `is_cut_off` cases) and Nit 4 (spider does not charge the Origin; same count) accepted as residual, no change.
