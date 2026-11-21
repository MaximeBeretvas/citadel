# web-vault

Local knowledge vault and MCP server for web documentation sites and local Markdown folders. `web-vault` crawls documentation sites, watches local Markdown directories, indexes content into SQLite + FTS5 with section splitting and unified diff history, and serves an offline embedded Web UI alongside a 2-tool MCP interface.

## Key Features

- **Unified ingestion:** Crawl `http(s)://` docs sites (`spider`, `sitemap.xml`, `llms.txt`, optional headless Chromium) and watch local Markdown folders with incremental SHA-256 diffing and `stale`/`error` tracking.
- **SQLite + FTS5 search:** Single-file WAL database with heading-level sections (`H1`–`H3`), breadcrumbs, and full-text search views (`v_sources`, `v_recent_changes`, `pages_fts`, `sections_fts`).
- **Hardened 2-tool MCP server:** Exposes `query` (strictly read-only SQLite authorizer, 5s timeout, row/byte caps) and `manage` (Private-origin-guarded URL and canonicalized path validation) over Streamable HTTP (`/mcp`) or `stdio`.
- **Self-contained runtime:** Single Rust binary with bundled SQLite and `rust-embed` offline Web UI (Sources, Knowledge Browser, Changes & Health, SQL Explorer).

## Quickstart (Apple `container` — Default)

Uses Apple's native macOS OCI runtime ([`apple/container`](https://github.com/apple/container))—no Docker Desktop required.

```bash
# Build the multi-stage non-root image (~25–35 MB)
make build

# Optional: build with headless Chromium support for JS-rendered sites
FEATURES=chrome make build

# Start the container (Web UI + REST API + MCP HTTP on loopback)
make run

# Mount a local Markdown folder read-only at /sources/books
BOOKS_DIR=/path/to/your/markdown make run
```

Open the embedded Web UI at **http://127.0.0.1:4317**.

Useful `Makefile` targets:

```bash
make logs                      # Inspect container logs
make exec CMD="list"           # Run CLI commands inside the container
make exec CMD="sync --all"     # Trigger a manual sync inside the container
make stop                      # Stop the running container
make clean                     # Stop and remove the container (preserves data volume)
```

## Native Installation (macOS / Linux)

```bash
# Install the binary onto $PATH
cargo install --path .

# Register and start the background scheduler & local folder watcher
# (launchd on macOS, systemd --user on Linux)
web-vault daemon install
```

## MCP Client Configuration

`web-vault` exposes two MCP tools:

- **`query`**: Execute a single read-only SQLite/FTS5 `SELECT` statement against the vault (`v_sources`, `pages`, `sections`, `pages_fts`, `sections_fts`, `v_recent_changes`) with `SQLITE_OPEN_READ_ONLY`, `PRAGMA query_only = ON`, authorizer allowlist, 5-second timeout, and row/byte caps.
- **`manage`**: Register, inspect, sync, or remove sources (`add`, `list`, `show`, `sync`, `rm`) with directory canonicalization on local paths. Every entry point (CLI, UI, MCP) refuses Private origins (loopback, private network, link-local, `localhost`); set `WEB_VAULT_ALLOW_PRIVATE_ORIGINS=1` to index intranet or `localhost` docs.

### Streamable HTTP (Recommended with `web-vault serve` or Apple `container`)

Point Claude, Antigravity, or any Streamable HTTP MCP client to:

```json
{
  "mcpServers": {
    "web-vault": {
      "url": "http://127.0.0.1:4317/mcp"
    }
  }
}
```

### Native `stdio` (`web-vault mcp`)

```json
{
  "mcpServers": {
    "web-vault": {
      "command": "web-vault",
      "args": ["mcp"]
    }
  }
}
```

### Apple `container` `stdio` (`container exec -i web-vault web-vault mcp`)

```json
{
  "mcpServers": {
    "web-vault": {
      "command": "container",
      "args": ["exec", "-i", "web-vault", "web-vault", "mcp"]
    }
  }
}
```

## Agent Skill Installation

Install the embedded `SKILL.md` router so coding agents know the vault schema, FTS5 recipes, and `query`/`manage` workflows:

```bash
# Install into the current repository (.agents/skills/web-vault/SKILL.md)
web-vault skill install

# Install globally for user agent skill directories (Claude, Antigravity, Codex, etc.)
web-vault skill install --global

# Install into a custom skills parent directory
web-vault skill install --path /path/to/skills
```

## Core CLI Commands

| Command | Description |
|---|---|
| `web-vault add <URL_OR_PATH> [--name SLUG]` | Register and sync a documentation URL or local Markdown directory. |
| `web-vault list [--json]` | List configured sources, page counts, and health status. |
| `web-vault show <NAME>` | Inspect source details, recent sync runs, and stale/error pages. |
| `web-vault sync [NAME \| --all] [--prune]` | Manually sync one or all sources (optionally pruning stale pages). |
| `web-vault rm <NAME> [--yes]` | Remove a source and cascade-delete its indexed pages. |
| `web-vault query "<SQL>" [--limit N] [--format table\|json\|csv]` | Run a read-only SQLite/FTS5 query against the vault. |
| `web-vault ui [--port 4317] [--no-open]` | Launch the embedded local Web UI server on `127.0.0.1`. |
| `web-vault serve [--bind 0.0.0.0:4317]` | Run the all-in-one server (daemon + Web UI + MCP Streamable HTTP on `/mcp`). |
| `web-vault daemon <start\|stop\|status\|install\|uninstall>` | Manage the background scheduler/watcher and OS service registration. |
| `web-vault skill install [--global \| --path DIR]` | Install the embedded agent router `SKILL.md`. |
