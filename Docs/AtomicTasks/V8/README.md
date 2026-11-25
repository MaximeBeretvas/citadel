# Atomic Tasks — V8: `citadel doctor`

> **Stage:** `AtomicTasks (V8) -> LLMCode`
> **Context:** [Docs/SPEC.md](../../SPEC.md) §6 | [Design](../../superpowers/specs/2026-11-25-doctor-command-design.md) | [Plan](../../superpowers/plans/2026-11-25-doctor-command.md) | [AGENTS.md](../../../AGENTS.md)

## Why

Answering "is Citadel running, and is it set up right?" takes `citadel daemon status`, `citadel update --check` and reading several agent config files by hand. One read-only command should say it.

## Settled Design

See the [design](../../superpowers/specs/2026-11-25-doctor-command-design.md).

- Doctor never writes files, creates the data directory or starts processes.
- The daemon writes `daemon.json` next to `daemon.pid` so doctor can compare its version with the CLI. `daemon.pid` is unchanged.
- Exit code 1 only when a check fails.

## Tasks

| Task | Goal |
|---|---|
| [01](./01-doctor-command.md) | Add `citadel doctor [--json]` with Version, Running, Setup and Data checks |

## Follow-ups

- `citadel serve` does not take the `daemon.pid` lock, so a native `serve` and daemon on one vault both run scheduled Syncs. Doctor flags it; fixing it is separate.
