# Atomic Tasks — V6: One Page per document (Markdown twins)

> **Stage:** `PRD -> SPEC -> [AtomicTasks (V6)] -> LLMCode`
> **Context:** [Docs/SPEC.md](../../SPEC.md) §4.1 | [CONTEXT.md](../../../CONTEXT.md) | [AGENTS.md](../../../AGENTS.md)
> **Issue:** [mberetvas/citadel#4](https://github.com/mberetvas/citadel/issues/4)

## Why

- **Duplicate Pages.** Docs sites now serve a raw Markdown copy of each page (`X.md` next to `X`). On `dlthub`, 195 of 715 Pages were such twins (about 270k words); on `docs.getdbt.com`, 58 of 118. `query` results then return the same section twice and use up the row and byte caps.
- **Query-string duplicate.** On `docs.getdbt.com`, the Origin `/best-practices?version=2` and `/best-practices` were both stored, though the former's `rel=canonical` names the latter.

## Settled Design

- **Keep one Page per document, at the HTML URI, with the Markdown twin's content** (no navigation or version banner; the frontmatter `title` wins). Chosen by the owner on 2026-11-24.
- Pair after every crawl, so the order in which `llms.txt` seeds and links arrive does not matter.
- Merged copies become `Superseded` results: their stored rows go stale (deleted with `--prune`), even in a cut-off Sync.
- **Unchanged:** which pages are requested; the schema; local Markdown Sources.

## Tasks

| Task | Goal |
|---|---|
| [01](./01-markdown-twin-pairing.md) | Store one Page per document for Markdown twins and canonical duplicates |
