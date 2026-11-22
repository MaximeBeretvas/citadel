# REVIEW.md — Grounded Verification & Feedback Loops

> **Purpose:** Objective, tool-agnostic feedback loops so every LLM grounds its output in real observed behavior—never guesses. Uses only standard cross-platform tools (`cargo`, `curl`, headless Chrome/Chromium, Apple `container`) with **zero admin rights** and **zero proprietary IDE dependencies**.

## Core Grounding Rule
Never claim a Rust invariant, MCP response, CLI flow, or UI layout works from reading code alone. Run the loop for every touched surface below, inspect the actual output (including PNG screenshots for UI changes), fix any defect, and cite the evidence in your `HumanAudit` handoff.

---

## Surface Matrix

| Touched Area | Verification Loop | Required Tooling |
|---|---|---|
| Any Rust code (`src/**`) | Cargo Gate (`fmt`, `clippy`, `test`) | `cargo` |
| SQLite / Read-Only (`src/db/**`) | Safety Suite + Live RO Probe | `cargo`, `citadel query` |
| MCP (`src/mcp/**`) | `stdio` JSON-RPC Purity + `/mcp` HTTP Probe | `python3` / `jq`, `curl` |
| Embedded Web UI (`src/ui/**`) | **Portable Headless Chrome Visual Loop** | Chrome / Chromium (`CHROME_BIN`), `curl` |
| CLI & Skill (`src/cli/**`, `src/skill/**`) | Temp-Vault CLI & Symlink Smoke | `cargo run` |
| Container (`Dockerfile`, `Makefile`) | Apple `container` Build + Health/MCP Smoke | `container` CLI |

---

## 1. Rust & SQLite Safety Loop (`src/**`, `src/db/**`)

Run the full check gate from [AGENTS.md](./AGENTS.md):
```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

When `src/db/` changes, also run the targeted read-only safety suite explicitly to confirm write/DDL/PRAGMA/multi-statement/timeout/cap guards stay green:
```bash
cargo test db::readonly
```

---

## 2. MCP `stdio` & Streamable HTTP Loop (`src/mcp/**`)

### `stdio` Purity & Contract Probe
Verify `stdout` emits **strictly valid JSON-RPC frames** even with verbose `RUST_LOG=debug` logging (all logs must land on `stderr`):
```bash
CITADEL_HOME=/tmp/wv-mcp-review RUST_LOG=debug cargo run --quiet -- mcp <<'EOF' \
  1>/tmp/wv-mcp-out.jsonl 2>/tmp/wv-mcp-err.log
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"review","version":"0.1"}}}
{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"query","arguments":{"sql":"SELECT name FROM v_sources"}}}
EOF

# Every stdout line MUST parse as JSON and tools/list MUST expose exactly ["query", "manage"]
python3 -c '
import json
lines = [json.loads(l) for l in open("/tmp/wv-mcp-out.jsonl") if l.strip()]
assert len(lines) == 3, f"expected 3 frames, got {len(lines)}"
tools = [t["name"] for t in lines[1]["result"]["tools"]]
assert tools == ["query", "manage"], f"unexpected tools: {tools}"
print("MCP stdio OK:", tools)
'
rm -rf /tmp/wv-mcp-review /tmp/wv-mcp-out.jsonl /tmp/wv-mcp-err.log
```

### Streamable HTTP Probe (`citadel serve`)
```bash
curl -sf -X POST http://127.0.0.1:4317/mcp \
  -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}'
```

---

## 3. Portable UI Visual Feedback Loop (`src/ui/**`)

Uses standard **Google Chrome or Chromium in headless mode** (user-space, zero admin rights, works identically on macOS and Linux for any contributor).

### Step 1 — Resolve Chrome/Chromium Binary
```bash
CHROME="${CHROME_BIN:-$(command -v google-chrome || command -v chromium || command -v chromium-browser || echo '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome')}"
test -x "$CHROME" && echo "Chrome ready: $CHROME" || echo "CHROME_MISSING"
```

### Step 2 — Seed an Isolated Temp Vault & Launch UI
Populate a throwaway vault (`/tmp/wv-ui-review`) with realistic data (at least 1 local source, pages, sections, and a diff/stale row when available), then start the server in the background:
```bash
rm -rf /tmp/wv-ui-review && mkdir -p /tmp/wv-ui-review/sample-docs
printf '# Sample Doc\n\n## Intro\nHello citadel.\n\n## Details\nGrounded UI test.\n' > /tmp/wv-ui-review/sample-docs/guide.md

CITADEL_HOME=/tmp/wv-ui-review cargo run --quiet -- add /tmp/wv-ui-review/sample-docs --name sample-docs
CITADEL_HOME=/tmp/wv-ui-review cargo run --quiet -- ui --port 4317 --no-open >/tmp/wv-ui.log 2>&1 &
UI_PID=$!
sleep 1
curl -sf http://127.0.0.1:4317/api/health
```

### Step 3 — DOM & Runtime Check
Dump rendered DOM after JS hydration to verify data renders and no error banners appear:
```bash
"$CHROME" --headless=new --disable-gpu --virtual-time-budget=2000 \
  --dump-dom "http://127.0.0.1:4317/#sources" | grep -q "sample-docs"
```

### Step 4 — Capture Screenshots (All 4 Views + Mobile + Dark Mode)
```bash
for view in sources browser changes sql; do
  "$CHROME" --headless=new --disable-gpu --window-size=1440,900 \
    --virtual-time-budget=2000 \
    --screenshot="/tmp/wv-${view}.png" \
    "http://127.0.0.1:4317/#${view}"
done

# Narrow viewport & dark mode spot checks
"$CHROME" --headless=new --disable-gpu --window-size=480,900 \
  --virtual-time-budget=2000 \
  --screenshot="/tmp/wv-mobile.png" \
  "http://127.0.0.1:4317/#sources"

"$CHROME" --headless=new --disable-gpu --force-dark-mode --window-size=1440,900 \
  --virtual-time-budget=2000 \
  --screenshot="/tmp/wv-dark.png" \
  "http://127.0.0.1:4317/#changes"
```

### Step 5 — Inspect Screenshots with Vision & Iterate
Read every captured PNG (`/tmp/wv-sources.png`, `/tmp/wv-browser.png`, `/tmp/wv-changes.png`, `/tmp/wv-sql.png`, `/tmp/wv-mobile.png`, `/tmp/wv-dark.png`) using your image-reading tool (`read_file`). Check for:
- Broken layout, overlapping elements, or clipped tables/code blocks.
- Unreadable contrast in both light and dark modes.
- Missing seeded data, raw unrendered template strings, or visible error states.

Fix any defect in `src/ui/assets/`, re-run Steps 3–5, and only stop when every screenshot is clean.

### Step 6 — Cleanup
```bash
kill "$UI_PID" 2>/dev/null || true
rm -rf /tmp/wv-ui-review /tmp/wv-ui.log /tmp/wv-*.png
```

### Fallback (No Chrome/Chromium Installed)
If Step 1 prints `CHROME_MISSING`, run `cargo test ui::` and `curl` checks against `/`, `/api/health`, `/api/sources`, and `/api/query`, then explicitly state `Visual check: skipped (Chrome/Chromium unavailable)` in your `HumanAudit` handoff so the human reviewer visually inspects `http://127.0.0.1:4317`.

---

## 4. CLI & Skill Installer Smoke Loop (`src/cli/**`, `src/skill/**`)

```bash
CITADEL_HOME=/tmp/wv-cli-review cargo run --quiet -- skill install --path /tmp/wv-cli-review/skills
test -f /tmp/wv-cli-review/skills/citadel/SKILL.md
rm -rf /tmp/wv-cli-review
```

---

## 5. Apple `container` Loop (`Dockerfile`, `Makefile`)

```bash
container build -t citadel:check .
```
When testing runtime behavior, start a short-lived container on loopback (`127.0.0.1:4317:4317`), verify `curl -sf http://127.0.0.1:4317/api/health` and `POST /mcp`, then stop and remove the test container. If running on Linux/CI without Apple's `container` CLI, note `Container build: skipped (Apple container CLI unavailable)` in `HumanAudit`.

---

## HumanAudit Evidence Format

Include this 4-line block in every `HumanAudit` handoff:
```markdown
- **Checks:** `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` (exact pass/fail counts)
- **Live / Visual Evidence:** commands run & observed output (e.g. MCP JSON-RPC frames, `curl /api/health`)
- **Screenshots Inspected:** list of views verified via headless Chrome (or `N/A` / fallback note)
- **Open Risks / Assumptions:** anything untested or deferred
```
