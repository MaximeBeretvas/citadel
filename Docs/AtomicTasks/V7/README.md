# Atomic Tasks — V7: One module decides what each Collected page becomes

> **Stage:** `[Architecture review] -> AtomicTasks (V7) -> LLMCode`
> **Context:** [Docs/SPEC.md](../../SPEC.md) §4.1 | [CONTEXT.md](../../../CONTEXT.md) (Collected page, Cut-off Sync) | [AGENTS.md](../../../AGENTS.md)

## Why

- **Three copies of one filter.** Three places decide whether a Collected page becomes a Page, an Error page or a merged copy, or is dropped: `scoped_urls`, `pair_documents` and `build_page_outcomes` in `src/ingest/web.rs`. `pair_documents`' own comment says it must mirror `build_page_outcomes`' first-per-URI rule.
- **The cut-off rule is fed from three call sites.** `is_cut_off` is three comparisons. All of its knowledge is in which numbers reach it: first-crawl counts at `execute_web_crawl`, the storable count after the later crawls, and `outcomes.len()` at `sync_checked_web_source`. Two bugs were in those call sites, not in the function: 73c3137 (cut-off judged on results after pages were dropped) and d2909eb (a step's duration lost on `?`). The `is_cut_off` tests check only its boolean logic.
- **Counting is spread out.** `WebSyncAccounting` is a mutable record passed `&mut` through four functions, and `unparsable_url` is counted in two places. SPEC §4's invariant (`pages_received` = results + the eight drop counts) can only be checked through a full Sync against a fixture server.
- `src/ingest/web.rs` changed in 15 of the last 25 commits. Every recent web-ingest bug was in this logic.

## Settled Design

Settled in a grilling session with the owner on 2026-11-24.

- **Pure refactor.** The Page outcomes, warnings, missing-page policy and every count on the `web_sync_summary` line stay identical. That line is a contract: SPEC §4 specifies it, and `.agents/skills/measuring-citadel-footprint/scripts/sync_log_report.py` parses it. The existing fixture-server tests pass unchanged, which proves it.
- **A collector the crawls feed.** `CollectedPages` lives in `src/ingest/web/collected.rs`; `web.rs` keeps its name and gains a `web/` child folder.
  - The first crawl and each later crawl hand it their pages and their lost count.
  - It answers what the next later crawl may do: skip the seed, stop, or crawl with the remaining document budget.
  - `finish(prune)` returns either the Page outcomes, the missing-page policy, the warnings and the page counts (including `cut_off`), or the "nothing reachable" error.
- **spider `Page`s in, finished outcomes out.** Conversion stays inside `finish`. A cleanly fetched Markdown twin replaces the HTML conversion, and `transform_content` needs a spider `Page`, so splitting conversion out would leak the twin rule across the seam.
- **Cut-off counts Retired results.** The "results reach `max_pages`" condition counts stored results, failed results and merged copies, as SPEC:278 already says. The `is_cut_off` docstring and SPEC:280 said "stored or failed" and are corrected.
- **Render status confirmation moves to the rendered crawl path.** It now runs per crawl, before the pages are handed over, so the collector stays pure. Outcomes and counts stay the same. Two things change slightly:
  - A 4xx page that a later crawl then drops as already collected is also re-checked (one extra GET, render_js only).
  - The confirmation time is now inside `first_crawl_ms` / `later_crawls_ms`. Before, it wasn't timed at all.
- **The "nothing reachable" fallback message prints the parsed Origin.** `finish` has no `SourceRecord`, so it reads the Origin from the scope. Every Origin stored through the Source lifecycle is already a normalized URL, so the text is the same. Only a row stored in another form would print differently.
- **Who owns which fields.**
  - The collector counts every page: `pages_received`, `lost_by_listener`, `later_crawls_started`, the eight drop reasons, `results_stored`, `results_failed` and `cut_off`.
  - The caller keeps the clock: `delay_ms` and the five step durations. `conversion_ms` now times `finish`.
- **Web-only output.** `persist_sync_outcomes` and `src/ingest/local.rs` are unchanged. Whether the output becomes a Sync plan shared with local Syncs is left to the follow-up that gives Sync run state one owner.
- **Unchanged:** which pages are requested; the schema; local Markdown Sources; `NO_CHROME_WARNING`; the persistence of outcomes.

## Tasks

| Task | Goal |
|---|---|
| [01](./01-collected-pages.md) | Move collection, classification, cut-off and counting into `CollectedPages` behind one interface |

## Follow-ups (kept as-is by this pure refactor)

- **First and later crawls de-duplicate differently.** The first crawl keeps `/p` and `/p/` as two results (exact-URI check, SPEC §4.1 "Known, not fixed"). A later crawl drops the second as `already_collected` (dedup key). The two intake methods on `CollectedPages` make this visible.
- **Redirected-out pages count toward cut-off.** The cut-off's storable-URL count includes pages whose requested URL is storable but that redirected outside the Source. SPEC:278 calls this deliberate ("erring toward cut-off"). Confirm, or exclude them.
- **Next architecture candidate:** one owner for Sync run state. `fail_sync_run` and `record_sync_failure` record the same failure twice, and a failed web persist skips the first.
