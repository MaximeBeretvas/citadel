# Task V5-09: Update the performance findings from the measurement

- **Phase:** V5
- **Dependencies:** V5-08
- **Suggested Commit:** `docs: update performance findings from the Sync measurement`

## Goal
`PERFORMANCE.md` and the brainstorm record contain the measured patterns and the corrected references. That way the next design (replacing the later crawls, the delay) starts from measured facts. Done when both files carry the changes listed below.

## Exact Files
- `PERFORMANCE.md`:
  - **Finding 1:** add the measured pace. Correct the reference: it cites spider `website.rs:4977`, which sits in a function behind spider's `cmd` feature, and Citadel does not enable that feature. The sleep Citadel uses is at `website.rs:8488-8490`.
  - **Finding 6:** add the `bench-uv` evidence. After the first crawl, 55 `…/index.md` requests from `llms.txt` ran at 2.9-3.5 requests per second.
  - **New findings:**
    - the page budget is spent on pages that redirect out of the Source;
    - pages are stored twice under two spellings (with and without a trailing slash).
- `Docs/egm/concepts_brief.md`:
  - Under "Open, needs a measurement", record the four patterns and which Sources show them.
  - Rename the Term "Dropped page" to "Lost page".
  - Replace "seed crawl" with "later crawl" throughout.
  - Point to the design's Terms section.

## Acceptance Criteria
- `grep -n "seed crawl" Docs/egm/concepts_brief.md` returns nothing.
- Every number added comes from task 08's scenario in `Docs/resource-usage.md`.

## Checks Gate
Documentation only: no code checks.
