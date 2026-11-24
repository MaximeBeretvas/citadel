# Task V5-08: Measure where a web Sync spends its time

- **Phase:** V5
- **Dependencies:** V5-01, V5-04, V5-07
- **Suggested Commit:** `docs: record where a web Sync spends its time`

## Goal
`Docs/resource-usage.md` records, for `dlthub` and `bench-uv`, how long each Sync step took and why fetched pages were not stored. The numbers come from Syncs on a scratch vault, never the user's vault. Done when the scenario "Where a web Sync spends its time" is written with these numbers.

## Steps
Follow the rules in `.agents/skills/measuring-citadel-footprint/SKILL.md` and design §Technical Plan item 5.
1. Run `sync_log_report.py` on the existing `daemon.log` (lines 1-3439) with `--origin` for the five Sources. The request counts must be 139, 500, 500, 139, 500, 59, 1,003, 19 and 540.
2. Make a scratch data directory and start a foreground daemon on it, with stderr captured.
3. **The author starts this step, because it sends requests to third-party sites.** Add:
   - `https://dlthub.com/docs` with `max_pages` 1,000 and the exclude patterns `*.md` and `*/devel/*`;
   - `https://docs.astral.sh/uv/` with its default settings.

   Together these are about 1,150 requests at today's rate.
4. When both Syncs are done, run the script on the captured stderr. It must read their `web_sync_summary` lines.
5. Write the results into `Docs/resource-usage.md`:
   - the nine-row table and how it was made;
   - the script output;
   - the step durations and drop counts.
6. **Q1 (open):** only if the author says yes, repeat steps 2 to 5 for `aws-graffana`. That is more than 1,500 requests to `docs.aws.amazon.com`.

## Exact Files
- `Docs/resource-usage.md`: new scenario "Where a web Sync spends its time".
- `README.md` Resource Usage table: add a row for Sync time per page if the measurement gives one value that holds for both Sources. Otherwise, give the reason in the commit message.
- `Docs/SPEC.md` §9: the resource-budget row loses its "Planned" note. The commit says whether the figures near `SPEC.md` §9 (resource budget) changed.

## Acceptance Criteria
- No Sync ran against the user's vault.
- Every number in the scenario comes from the captured stderr or the existing log, and says which one.

## Checks Gate
```bash
python3 -m unittest discover -s .agents/skills/measuring-citadel-footprint/scripts -p 'test_*.py'
```
