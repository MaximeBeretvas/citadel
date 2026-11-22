"""Summarize a sample.sh CSV, optionally over a window.

usage: python3 -I summarize.py OUT.csv [START_EPOCH END_EPOCH]

- CPU-s   = last cputime - first cputime in the window.
- avg CPU = CPU-s / wall seconds (100 % = one core).
- peak CPU = largest cputime change over 3 sample gaps (~5 s) / elapsed seconds.
  Single gaps are unreliable: epochs are whole seconds but samples are ~1.5 s apart,
  so a 1-gap peak can overstate CPU by up to ~60 %.
- footprint = top MEM (what Activity Monitor shows). compressed = top CMPRS.
"""
import csv, statistics, sys


def mb(s):
    s = (s or "0").strip().rstrip("+-")
    if not s or s[-1].isdigit():
        return float(s or 0) / 1048576
    return float(s[:-1]) * {"B": 1 / 1048576, "K": 1 / 1024, "M": 1, "G": 1024}[s[-1]]


def secs(t):
    return sum(float(p) * 60 ** i for i, p in enumerate(reversed(t.split(":"))))


rows = list(csv.DictReader(open(sys.argv[1])))
if len(sys.argv) == 4:
    a, b = int(sys.argv[2]), int(sys.argv[3])
    rows = [r for r in rows if a <= int(r["epoch"]) <= b]
if len(rows) < 2:
    sys.exit("fewer than 2 samples in window")
ep = [int(r["epoch"]) for r in rows]
fp = [mb(r["footprint"]) for r in rows]
rss = [int(r["rss_kb"]) / 1024 for r in rows]
cmp_ = [mb(r.get("compressed")) for r in rows]
cpu = [secs(r["cputime"]) for r in rows]
wall = ep[-1] - ep[0] or 1
used = cpu[-1] - cpu[0]
peaks = [(cpu[i] - cpu[i - 3]) / max(1, ep[i] - ep[i - 3]) * 100 for i in range(3, len(rows))]
th = [int(r["threads"]) for r in rows]
print(f"samples={len(rows)} wall={wall}s")
print(f"footprint MB: start={fp[0]:.1f} median={statistics.median(fp):.1f} peak={max(fp):.1f} end={fp[-1]:.1f}")
print(f"rss MB:       start={rss[0]:.1f} median={statistics.median(rss):.1f} peak={max(rss):.1f} end={rss[-1]:.1f}")
print(f"compressed MB: end={cmp_[-1]:.1f} peak={max(cmp_):.1f}")
print(f"cpu: used={used:.2f}s avg={used / wall * 100:.2f}% peak={max(peaks) if peaks else 0:.0f}%")
print(f"threads: min={min(th)} max={max(th)}")
