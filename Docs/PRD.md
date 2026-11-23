# PRD: Citadel

> **Status:** Draft v1  
> **Stage:** `Idea -> [PRD] -> TechSpec (SPEC.md) -> AgentRules -> AtomicTasks -> Prototype -> LLMCode`  
> **Scope note:** This document defines *what* **Citadel** does, *who* it serves, and *why*. Technical architecture, storage internals, crawling libraries, and UI implementation choices are intentionally deferred to `SPEC.md`.

---

## 1. Problem & Vision

Data and software engineers rely heavily on external documentation (APIs, frameworks, cloud platforms) and reference material (technical books, internal guides) while working with AI coding assistants. Today, grounding LLMs in these sources is fragmented and high-maintenance:

- **Manual crawling & staleness:** Engineers run ad-hoc crawl scripts into local folders and manually diff changes, fighting noisy metadata updates and silently disappearing pages.
- **Static, siloed knowledge:** Converted books and docs sit in disconnected Markdown folders or static skills that go out of date and require manual re-ingestion.
- **Bloated MCP tool surfaces:** Existing MCP servers expose a separate tool for every narrow operation, wasting LLM context tokens and forcing multi-turn tool-call chains.

**Vision:** **Citadel** is a local-first, ultra-lightweight knowledge daemon written in **Rust**. It collects documentation from the web and local Markdown folders, automatically keeps web sources fresh via diff-based background checks, exposes a **minimal composable MCP interface** so any LLM can ground its answers in one call, and provides a fast **CLI + lightweight UI** to manage everything.

---

## 2. Target User & Core Workflow

**Primary Persona:** Data & software engineers using MCP-compatible LLMs (Claude Code, Antigravity, Codex, etc.) in daily development.

**Core Grounding Loop:**
1. The engineer registers documentation URLs or local Markdown folders in **Citadel**.
2. **Citadel** runs quietly in the background, storing content and periodically refreshing web sources when real content changes.
3. The engineer runs `citadel setup` to walk through an interactive wizard that installs the **Citadel** agent skill and configures the MCP server (locally in the project folder or globally) for their LLM client(s) (Claude Code, Antigravity / `agy`, Codex).
4. When the engineer asks their LLM a question, the LLM recognizes that **Citadel** may hold authoritative docs, queries **Citadel** in a single composable MCP tool call, and grounds its response in the retrieved, up-to-date sources.

---

## 3. Product Goals & Non-Goals

### Goals
- **Unified local knowledge base:** Treat crawled web documentation and local Markdown folders as first-class, uniformly queryable sources.
- **Always-fresh web docs:** Automatically detect meaningful content changes on a schedule while surfacing removed, renamed, or failing pages.
- **Minimal MCP footprint:** Provide as few MCP tools as possible—ideally **one composable query tool** (e.g., SQL-like) rather than one tool per function.
- **Zero-friction agent onboarding:** Ship an interactive `citadel setup` wizard that configures both the **Citadel** agent skill and MCP server settings—locally in a project folder or globally—for Claude Code, Antigravity (`agy`), and Codex.
- **Effortless self-maintenance:** Automatically detect when a newer version of **Citadel** is available and let users upgrade cleanly with a single `citadel update` command.
- **Low-resource background operation:** Run continuously in the background with minimal CPU and RAM usage (Rust constraint).
- **Fast human visibility:** Offer a lightweight UI launched from a single CLI command (`ui`) to browse, inspect, and manage stored knowledge.

### Non-Goals (v1)
- Multi-user cloud hosting, team sync, or SaaS accounts (local-first only).
- General-purpose web search across the open internet (only user-curated sources).
- Automated LLM summarization or book-distillation pipelines (like `book-to-skill` / `cangjie-skill`); **Citadel** stores and serves faithful source content.
- Authentication-heavy scraping behind paywalls or interactive login flows.

---

## 4. Functional Requirements

### 4.1 Source Ingestion
| ID | Requirement | Priority |
|---|---|---|
| **FR-1.1** | **Web URL ingestion:** Accept a root URL, automatically discover related sub-URLs within the target documentation scope, and store each page's clean readable content. | Must |
| **FR-1.2** | **Scope controls:** Allow configuring crawl boundaries per source (e.g., path prefix, max depth/pages, include/exclude patterns) so crawls stay focused on relevant docs. | Must |
| **FR-1.3** | **Local Markdown folder ingestion:** Accept a local directory containing Markdown files (e.g., converted books or exported docs), preserving folder hierarchy and document titles. | Must |
| **FR-1.4** | **Source metadata:** Track user-friendly source names/tags, origin (URL or folder path), page counts, last sync time, and sync status. | Must |

### 4.2 Background Sync & Diff-Based Freshness
| ID | Requirement | Priority |
|---|---|---|
| **FR-2.1** | **Background daemon:** Run continuously as a low-resource background process managing scheduled refreshes. | Must |
| **FR-2.2** | **Configurable refresh cadence:** Support a default periodic check interval and per-source overrides (including manual-only for static sources). | Must |
| **FR-2.3** | **Meaningful diff detection:** Compare fetched content against stored content and only record an update when substantive content differs (ignoring volatile noise like crawl timestamps). | Must |
| **FR-2.4** | **Stale & broken page visibility:** Track pages that disappear, move, or fail to fetch during a refresh instead of silently deleting them. | Must |
| **FR-2.5** | **Local folder sync:** Detect added, modified, or deleted Markdown files in registered local folders (on demand or automatically). | Should |
| **FR-2.6** | **Manual refresh:** Allow triggering an immediate refresh for a single source or all sources via CLI or UI. | Must |

### 4.3 Minimal Composable MCP Server
| ID | Requirement | Priority |
|---|---|---|
| **FR-3.1** | **MCP compliance:** Run as a standard MCP server usable by Claude, Antigravity, and any other MCP-compatible LLM client. | Must |
| **FR-3.2** | **Minimal composable tool philosophy:** Expose the smallest possible number of tools—ideally **one primary query tool** (such as a read-only SQL query interface) that lets an LLM combine source discovery, keyword/content search, filtering, pagination, and content retrieval in a single tool call, rather than one tool per narrow function. | Must |
| **FR-3.3** | **Manage vault via MCP:** Allow an LLM using the MCP server to manage vault sources (add a web URL or local Markdown folder, trigger a sync, update settings, or remove a source) through a single minimal management tool without switching to the CLI or UI. | Must |
| **FR-3.4** | **Self-describing query surface:** Tool descriptions and schema guidance must make it effortless for an LLM to write valid queries and register sources on the first try without extra round-trips. | Must |
| **FR-3.5** | **Grounded citations:** Query results must include provenance metadata (source name, page URL or file path, last-updated timestamp) so LLMs can cite and verify freshness. | Must |
| **FR-3.6** | **Token-conscious retrieval:** Support querying metadata/snippets/sections before pulling full pages so LLMs do not blow their context window. | Must |

### 4.4 CLI
| ID | Requirement | Priority |
|---|---|---|
| **FR-4.1** | **Source management:** Commands to add (URL or folder), list, inspect, refresh, and remove sources. | Must |
| **FR-4.2** | **Daemon & MCP control:** Commands to start/stop/check the background daemon and launch the MCP server endpoint. | Must |
| **FR-4.3** | **Direct querying:** Ability to run the same composable query from the terminal for debugging or human use. | Should |
| **FR-4.4** | **Skill installer:** A dedicated CLI command to install the **Citadel** agent skill (`SKILL.md`), supporting three target modes: (1) current working directory, (2) an explicit user-provided path via argument/flag, and (3) standard global agent locations (e.g., `~/.claude/skills`, `~/.agents/skills`, and similar cross-agent roots). | Must |
| **FR-4.5** | **UI launcher:** A single `ui` CLI command that starts/opens the lightweight management UI. | Must |
| **FR-4.6** | **Interactive setup wizard (`citadel setup`):** Launch a guided terminal wizard that creates both the **Citadel** agent skill (`SKILL.md`) and the MCP server configuration in one flow, letting the user choose **local** (current project folder) or **global** scope for **Claude Code**, **Antigravity (`agy`)**, and **Codex**. Safely merges into existing MCP config files without overwriting unrelated servers, remains idempotent on re-run, and supports non-interactive flags for automation. | Must |
| **FR-4.7** | **Self-update (`citadel update`):** Upgrade **Citadel** to the latest release via a single CLI command, verifying release integrity before replacing the binary (and showing clear upgrade instructions when running inside an OCI container). | Must |
| **FR-4.8** | **Automatic update awareness:** Automatically check in the background (with local caching) whether a newer version of **Citadel** is available and surface a non-intrusive notice in interactive CLI sessions—never blocking startup, failing silently when offline, never polluting `citadel mcp` `stdio` or machine-readable output, and supporting an opt-out. | Must |

### 4.5 Lightweight Management UI
| ID | Requirement | Priority |
|---|---|---|
| **FR-5.1** | **Instant, lightweight launch:** Starts quickly via the `ui` CLI command with minimal resource overhead. | Must |
| **FR-5.2** | **Source overview:** View all registered web and local sources with sync status, page count, last-checked time, and error/stale indicators. | Must |
| **FR-5.3** | **Knowledge browser:** Browse and search stored pages/documents and read their stored content. | Must |
| **FR-5.4** | **Change & health visibility:** Inspect recently updated pages (what changed) and flagged stale/failed URLs. | Should |
| **FR-5.5** | **Management actions:** Add new URLs/folders, trigger manual refreshes, edit refresh schedules, and remove sources directly from the UI. | Must |
| **FR-5.6** | **Update indicator:** Display a subtle, non-intrusive indicator when a newer version of **Citadel** is available, showing the available version and how to upgrade (`citadel update`). | Should |

---

## 5. Non-Functional Requirements

- **Resource Efficiency (Rust):** Implemented in Rust as a lightweight native binary; idle background service must consume minimal RAM and negligible CPU.
- **Deployment Flexibility (Container Default + Native Option):** Default deployment as a lightweight OCI container run with Docker (Apple's **`container`** CLI, `https://github.com/apple/container`, remains a supported option) with persistent volume storage and optional host Markdown bind mounts, while also supporting direct native installation on macOS/Linux.
- **Fast Startup & Query Latency:** CLI commands, `ui` launch, and MCP queries should feel instantaneous for local knowledge bases spanning thousands of pages.
- **Distribution & Release Quality:** Every release ships prebuilt binaries for macOS (Apple Silicon and Intel) and Linux (x86_64 and ARM64) so users can install and self-update without a Rust toolchain. Every change passes an automated checks gate before it can be merged. Windows is not yet tested; Windows users install with `cargo install` or build from source.
- **Local-First & Portable:** All stored knowledge and configuration live locally on disk in predictable user directories with no external service dependencies; automatic update checks are lightweight, cached, non-blocking, fail silently when offline, and can be opted out of.
- **Polite Crawling:** Respect basic web etiquette (rate limiting, concurrency caps, conditional requests where available) so background checks do not hammer documentation hosts.
- **Safe LLM Access & Non-Destructive Setup:** The composable MCP query interface must remain strictly read-only to prevent destructive SQL mutations or arbitrary filesystem access; adding sources via MCP must be explicitly validated and scoped; `citadel setup` must safely merge MCP settings without clobbering existing agent configuration.

---

## 6. Key User Journeys

1. **Add & Auto-Maintain Web Docs:** User runs CLI add command with `https://docs.example.com/` -> **Citadel** discovers sub-pages, stores clean content -> background daemon checks periodically and updates only changed pages, flagging any removed URLs.
2. **Import Local Markdown Books/Docs:** User runs CLI add command pointing to `/path/to/markdown/book-folder` -> **Citadel** indexes all `.md` files -> book becomes queryable alongside web docs.
3. **Set Up Agent Integration & Ground an LLM:** User runs `citadel setup` in a project folder -> interactive wizard asks which agent(s) (Claude Code, Antigravity / `agy`, Codex) and scope (local vs. global) to configure -> **Citadel** creates the skill and merges the MCP server config -> in the next LLM session, the user asks a technical question and the LLM queries **Citadel** via MCP to answer grounded in exact, fresh docs.
4. **Manage Knowledge Visually:** User runs `citadel ui` -> lightweight UI opens immediately -> user checks which doc sites updated recently, inspects a stale URL, adds a new doc source, and closes the UI while the daemon keeps running.
5. **Stay Up to Date:** **Citadel** automatically detects a newer release in the background and shows a subtle notice in the CLI or UI -> user runs `citadel update` -> **Citadel** downloads, verifies, and installs the new version cleanly.

---

## 7. Success Criteria

- An engineer can add a multi-page doc site and a local Markdown folder, run `citadel setup` to configure the skill and MCP server, and get a grounded LLM answer in under 5 minutes.
- LLMs consistently discover and retrieve relevant documentation in **1–2 MCP tool calls** using the minimal composable tool interface.
- Background refreshes run unattended without noticeable CPU/memory impact or noisy false-positive updates.
- The `ui` command launches in under 1 second and covers all day-to-day source management without touching config files manually.
- Users are notified of new releases with zero noticeable CLI/UI startup delay and can upgrade with a single `citadel update` command.

---

## 8. Handoff: Decisions Deferred to `SPEC.md`

The following technical choices are intentionally left open for the **TechSpec (`SPEC.md`)** stage:

1. **Storage & query engine:** SQLite + FTS5 vs. DuckDB vs. hybrid full-text + vector embeddings, and exact SQL schema / read-only sandboxing.
2. **Crawling strategy:** Pure HTTP/HTML + sitemap/`llms.txt` vs. optional headless-browser fallback for JS-rendered SPA doc sites.
3. **UI form factor:** Embedded local web UI (served on localhost) vs. native Terminal UI (TUI).
4. **Daemon architecture:** OS service integration (`launchd`/`systemd`) vs. self-managed background daemon, plus IPC between CLI, UI, daemon, and MCP server.
5. **Skill template, MCP config merge & multi-agent paths:** Exact `SKILL.md` prompt design, symlink vs. copy behavior, and per-agent local/global MCP config file locations and merge semantics for Claude Code, Antigravity (`agy`), and Codex.
6. **Release distribution & self-update mechanics:** Release channel (e.g., GitHub Releases), checksum/signature verification, version-check cache TTL and opt-out env var, and upgrade behavior in Apple `container` vs. native installs.
