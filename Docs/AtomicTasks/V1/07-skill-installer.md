# Task 07: Agent Skill Template & `skill install` CLI Command

- **Phase:** 1 (Prototype Vertical Slice — completes Prototype Milestone!)
- **Dependencies:** [01-project-scaffold.md](./01-project-scaffold.md)
- **Suggested Commit:** `feat(skill): add embedded citadel router SKILL.md and skill install command`

## Goal
Author the embedded static router skill (`src/skill/SKILL.md`) teaching LLMs when and how to ground answers using `citadel`'s `query` and `manage` MCP tools, and implement `citadel skill install` supporting CWD, `--path <dir>`, and `--global` (`--target agents|claude|copilot|opencode|all`) per [Docs/SPEC.md](../../SPEC.md#L394-L404).

## Exact Files
- `src/skill/SKILL.md`
- `src/skill/mod.rs`
- `src/cli/mod.rs` (wire `skill install`)

## Red-First Test Cases (Write Before Implementation)
1. `skill::tests::default_installs_into_cwd_agents_skills`: no flags writes `<cwd>/.agents/skills/citadel/SKILL.md`.
2. `skill::tests::custom_path_installs_into_target_dir`: `--path <tmp>/custom` writes `<tmp>/custom/citadel/SKILL.md`.
3. `skill::tests::global_install_writes_canonical_and_symlinks_claude`: with a mocked home dir, `--global --target all` writes `<home>/.agents/skills/citadel/SKILL.md` and creates symlink `<home>/.claude/skills/citadel -> <home>/.agents/skills/citadel`.
4. `skill::tests::refuses_overwrite_without_force_and_overwrites_with_force`: existing `SKILL.md` returns an error unless `--force` is set.

## Acceptance Criteria
- `src/skill/SKILL.md` is embedded via `include_str!("SKILL.md")` (zero external runtime files).
- `SKILL.md` includes concise frontmatter (`name: citadel`, sharp trigger `description`), the SQLite schema summary (`v_sources`, `pages`, `sections`, `sections_fts`, `v_recent_changes`), 2-step FTS5 query recipes, citation rules, and `manage` usage.
- Symlink creation handles existing symlinks cleanly when `--force` is passed.

## Prototype Milestone Verification (Manual Smoke Test After Checks)
```bash
cargo run -- add /Users/maxime/Dev/books/markdown/fundamentals-of-data-engineering --name fundamentals-de
cargo run -- query "SELECT name, active_pages FROM v_sources"
cargo run -- skill install --path /tmp/wv-skill-test
```

## Checks Gate
```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```
