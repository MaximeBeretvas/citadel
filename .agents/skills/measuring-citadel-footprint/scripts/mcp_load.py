"""Drive `citadel mcp` over stdio with the agent skill's 2-step FTS5 recipe and time each call.

usage: CITADEL_HOME=<vault dir> python3 -I mcp_load.py SAMPLE_SH OUT_CSV ROUNDS [TERM,TERM,...]
Each round = one search (LIMIT 15) + one body fetch (top 3 ids) per term.
Prints a JSON summary; sample.sh writes the CSV for the mcp process.
Use ROUNDS >= 20: a 100-call run ends in about a second, shorter than the sampling interval.
"""
import json, os, statistics, subprocess, sys, time
from typing import IO

sampler, out_csv, rounds = sys.argv[1], sys.argv[2], int(sys.argv[3])
TERMS = sys.argv[4].split(",") if len(sys.argv) > 4 else [
    "pipeline", "destination", "incremental", "schema", "install", "config",
    "credentials", "merge", "state", "resource", "source", "api", "filesystem",
    "database", "deploy", "error", "dataset", "secrets", "test", "cache"]

p = subprocess.Popen(["citadel", "mcp"], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                     stderr=subprocess.DEVNULL, text=True, bufsize=1, env=os.environ)


def _pipes(proc: "subprocess.Popen[str]") -> tuple[IO[str], IO[str]]:
    if proc.stdin is None or proc.stdout is None:
        sys.exit("failed to open pipes to citadel mcp")
    return proc.stdin, proc.stdout


stdin, stdout = _pipes(p)
time.sleep(0.5)
samp = subprocess.Popen(["zsh", sampler, str(p.pid), out_csv, "100000"])
_id = 0


def call(method, params):
    global _id
    _id += 1
    stdin.write(json.dumps({"jsonrpc": "2.0", "id": _id, "method": method, "params": params}) + "\n")
    stdin.flush()
    t = time.perf_counter()
    line = stdout.readline()
    return (time.perf_counter() - t) * 1000, json.loads(line)


time.sleep(2)  # let the sampler record the pre-initialize footprint
call("initialize", {"protocolVersion": "2025-03-26", "capabilities": {},
                    "clientInfo": {"name": "bench", "version": "0"}})
stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n")
stdin.flush()

search_ms, fetch_ms, errors, hits = [], [], 0, 0
for _ in range(rounds):
    for term in TERMS:
        q = ("SELECT s.id, src.name AS source, p.uri, s.heading_path, "
             "snippet(sections_fts, 1, '[', ']', '...', 16) AS excerpt, bm25(sections_fts) AS rank "
             "FROM sections_fts JOIN sections s ON s.id = sections_fts.rowid "
             "JOIN pages p ON p.id = s.page_id JOIN sources src ON src.id = s.source_id "
             f"WHERE sections_fts MATCH '{term}' ORDER BY rank LIMIT 15")
        ms, r = call("tools/call", {"name": "query", "arguments": {"sql": q}})
        search_ms.append(ms)
        res = r.get("result", {})
        if "error" in r or res.get("isError"):
            errors += 1
            continue
        payload = res.get("structuredContent") or json.loads(res["content"][0]["text"])
        rows = payload.get("rows", []) if isinstance(payload, dict) else []
        ids = [row["id"] if isinstance(row, dict) else row[0] for row in rows[:3]]
        if not ids:
            continue
        hits += 1
        q2 = f"SELECT s.id, s.content_md FROM sections s WHERE s.id IN ({','.join(map(str, ids))})"
        ms, r = call("tools/call", {"name": "query", "arguments": {"sql": q2}})
        fetch_ms.append(ms)
        if "error" in r or r.get("result", {}).get("isError"):
            errors += 1
    time.sleep(1)

time.sleep(3)
stdin.close()
p.wait(timeout=10)
samp.terminate()


def pct(xs, q):
    xs = sorted(xs)
    return round(xs[min(len(xs) - 1, int(q * len(xs)))], 1) if xs else None


print(json.dumps({
    "searches": len(search_ms), "fetches": len(fetch_ms), "errors": errors, "terms_with_hits": hits,
    "search_ms_median": round(statistics.median(search_ms), 1), "search_ms_p95": pct(search_ms, .95),
    "fetch_ms_median": round(statistics.median(fetch_ms), 1) if fetch_ms else None,
    "fetch_ms_p95": pct(fetch_ms, .95),
}))
