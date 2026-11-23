<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="Docs/assets/logo/citadel-horizontal-reversed.svg" />
    <img src="Docs/assets/logo/citadel-horizontal.svg" alt="Citadel" width="360" />
  </picture>
</p>

Local knowledge vault and MCP server for web documentation sites and local Markdown folders. `citadel` crawls documentation sites, watches local Markdown directories, indexes content into SQLite + FTS5 with section splitting and unified diff history, and serves an offline embedded Web UI alongside a 2-tool MCP interface.

## Key Features

- **Unified ingestion:** Crawl `http(s)://` docs sites (`spider`, `sitemap.xml`, `llms.txt`, optional headless Chromium) and watch local Markdown folders with incremental SHA-256 diffing and `stale`/`error` tracking.
- **SQLite + FTS5 search:** Single-file WAL database with heading-level sections (`H1`–`H3`), breadcrumbs, and full-text search views (`v_sources`, `v_recent_changes`, `pages_fts`, `sections_fts`).
- **Hardened 2-tool MCP server:** Exposes `query` (strictly read-only SQLite authorizer, 5s timeout, row/byte caps) and `manage` (Private-origin-guarded URL and canonicalized path validation) over Streamable HTTP (`/mcp`) or `stdio`.
- **Self-contained runtime:** Single Rust binary with bundled SQLite and `rust-embed` offline Web UI (Sources, Knowledge Browser, Changes & Health, SQL Explorer).

## Resource Usage

Measured on an M1 Pro Mac with a real docs site (500 pages of dlthub docs):

| | Memory | CPU |
|---|---|---|
| Daemon, idle | ~5 MB (empty vault), ~25–35 MB after Syncs | ~0.1 % |
| Daemon, syncing 500 pages | ~45–60 MB median, up to ~120 MB peak, for about a minute | ~6–7 CPU-seconds per Sync |
| `citadel mcp`, agent queries | under 8 MB | ~3 ms per query |
| `citadel ui` | under 10 MB | negligible |

Method, every scenario, and caveats: [Docs/resource-usage.md](Docs/resource-usage.md).

## Install (Recommended)

Install the `citadel` binary straight from GitHub with Cargo. Requires a Rust toolchain ([`rustup`](https://rustup.rs/)); SQLite is bundled, so no system `sqlite3` library is needed.

```bash
cargo install --git https://github.com/mberetvas/citadel --locked

# Lite build: no headless-browser support (smaller binary, no Chrome needed)
cargo install --git https://github.com/mberetvas/citadel --locked --no-default-features
```

The default build includes the `chrome` feature, so Sources added with `--render-js` can crawl sites whose navigation is built by JavaScript. Those crawls need Google Chrome or Chromium installed (or `CHROME_URL` pointing at a running instance); Citadel launches a private headless browser only while such a crawl runs, so other Sources and the idle daemon never start one.

This builds the `main` branch; add `--tag <tag>` or `--branch <name>` to pin another ref, and re-run with `--force` to upgrade. Ensure `~/.cargo/bin` is on your `$PATH`, then verify and optionally register the background service:

```bash
citadel --version

# Register and start the background scheduler & local folder watcher
# (launchd on macOS, systemd --user on Linux)
citadel daemon install
```

### Prebuilt Binaries (macOS & Linux)

Every [GitHub Release](https://github.com/mberetvas/citadel/releases) ships a `citadel` binary built with the default features (including `chrome`) for macOS (Apple Silicon `aarch64-apple-darwin`, Intel `x86_64-apple-darwin`) and Linux (`x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`; glibc 2.35+), plus a `checksums.sha256` manifest. No Rust toolchain needed:

```bash
V=v0.2.0                  # release tag
T=aarch64-apple-darwin    # your target, from the list above
curl -LO https://github.com/mberetvas/citadel/releases/download/$V/citadel-$V-$T.tar.gz
curl -LO https://github.com/mberetvas/citadel/releases/download/$V/checksums.sha256
shasum -a 256 -c --ignore-missing checksums.sha256   # Linux: sha256sum -c --ignore-missing checksums.sha256
mkdir -p ~/.local/bin
tar xzf citadel-$V-$T.tar.gz && install -m 755 citadel ~/.local/bin/
```

Make sure `~/.local/bin` is on your `$PATH` (on macOS it is not by default: add `export PATH="$HOME/.local/bin:$PATH"` to `~/.zshrc`). After that, `citadel update` keeps it current. On macOS, a binary downloaded through a **browser** is quarantined by Gatekeeper (the release is not notarized); clear it once with `xattr -d com.apple.quarantine citadel`. Downloads via `curl`, `cargo install`, and `citadel update` are not affected.

### Windows

Windows is **not yet tested** and has no prebuilt binary. Install with the `cargo install --git` command above, or build from a checkout (see below). `citadel update` and `citadel daemon install` are not supported on Windows.

## Alternative: Apple `container`

Uses Apple's native macOS OCI runtime ([`apple/container`](https://github.com/apple/container))—no Docker Desktop required.

```bash
# Build the multi-stage non-root image (includes Chromium for --render-js Sources)
make build

# Lite image without Chromium (~25–35 MB); --render-js Sources fall back to static HTTP
make build-lite

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

## Alternative: Install from a Local Checkout

For development, install from a cloned repository instead:

```bash
make install          # cargo install --path . --locked --force (default features, includes chrome)

make install-lite     # same, with --no-default-features (no headless-browser support)
```

## MCP Client Configuration

`citadel` exposes two MCP tools:

- **`query`**: Execute a single read-only SQLite/FTS5 `SELECT` statement against the vault (`v_sources`, `pages`, `sections`, `pages_fts`, `sections_fts`, `v_recent_changes`) with `SQLITE_OPEN_READ_ONLY`, `PRAGMA query_only = ON`, authorizer allowlist, 5-second timeout, and row/byte caps.
- **`manage`**: Register, inspect, sync, or remove sources (`add`, `list`, `show`, `sync`, `rm`) with directory canonicalization on local paths. Every entry point (CLI, UI, MCP) refuses Private origins (loopback, private network, link-local, `localhost`); set `CITADEL_ALLOW_PRIVATE_ORIGINS=1` to index intranet or `localhost` docs.

### Streamable HTTP (Recommended with `citadel serve` or Apple `container`)

Point Claude, Antigravity, or any Streamable HTTP MCP client to:

```json
{
  "mcpServers": {
    "citadel": {
      "url": "http://127.0.0.1:4317/mcp"
    }
  }
}
```

### Native `stdio` (`citadel mcp`)

```json
{
  "mcpServers": {
    "citadel": {
      "command": "citadel",
      "args": ["mcp"]
    }
  }
}
```

### Apple `container` `stdio` (`container exec -i citadel citadel mcp`)

```json
{
  "mcpServers": {
    "citadel": {
      "command": "container",
      "args": ["exec", "-i", "citadel", "citadel", "mcp"]
    }
  }
}
```

## Agent Skill Installation

Install the embedded `SKILL.md` router so coding agents know the vault schema, FTS5 recipes, and `query`/`manage` workflows:

```bash
# Install into the current repository (.agents/skills/citadel/SKILL.md)
citadel skill install

# Install globally for user agent skill directories (Claude, Antigravity, Codex, etc.)
citadel skill install --global

# Install into a custom skills parent directory
citadel skill install --path /path/to/skills
```

## Core CLI Commands

| Command | Description |
|---|---|
| `citadel add <URL_OR_PATH> [--name SLUG]` | Register and sync a documentation URL or local Markdown directory. |
| `citadel list [--json]` | List configured sources, page counts, and health status. |
| `citadel show <NAME>` | Inspect source details, recent sync runs, and stale/error pages. |
| `citadel sync [NAME \| --all] [--prune]` | Manually sync one or all sources (optionally pruning stale pages). |
| `citadel rm <NAME> [--yes]` | Remove a source and cascade-delete its indexed pages. |
| `citadel query "<SQL>" [--limit N] [--format table\|json\|csv]` | Run a read-only SQLite/FTS5 query against the vault. |
| `citadel ui [--port 4317] [--no-open]` | Launch the embedded local Web UI server on `127.0.0.1`. |
| `citadel serve [--bind 0.0.0.0:4317]` | Run the all-in-one server (daemon + Web UI + MCP Streamable HTTP on `/mcp`). |
| `citadel daemon <start\|stop\|status\|install\|uninstall>` | Manage the background scheduler/watcher and OS service registration. |
| `citadel skill install [--global \| --path DIR]` | Install the embedded agent router `SKILL.md`. |
