#!/usr/bin/env python3
"""Split a Citadel daemon log into one row per web Sync.

Usage:
  python3 -I sync_log_report.py LOG [--origin name=url ...] [--single]

A Sync opens on the daemon's "starting scheduled web sync" line and closes on its
"scheduled web sync complete" or "scheduled web sync failed" line. Each spider
`fetch <url>` line goes to the only open Sync, or, when several are open, to the one
whose Origin has the same host. `--origin` gives the Origin for logs written before
the start line carried `origin=`. `--single` treats the whole file as one Sync (the
stderr of one `citadel sync` run).

Standard library only, Python 3.9 or later (the macOS system python3). The file is streamed once.
"""

from __future__ import annotations

import argparse
import re
import statistics
import sys
from dataclasses import dataclass, field
from datetime import datetime
from urllib.parse import urlsplit

TS_RE = re.compile(r"^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d+)?)Z\s")
START_RE = re.compile(r"starting scheduled web sync source=(\S+)(?: origin=(\S+))?")
COMPLETE_RE = re.compile(r"scheduled web sync complete source=(\S+)")
FAILED_RE = re.compile(r"scheduled web sync failed source=(\S+)")
FETCH_RE = re.compile(r"spider::utils: fetch (\S+)")
SUMMARY_RE = re.compile(r"web_sync_summary (.*)$")
FIELD_RE = re.compile(r'(\w+)=("(?:[^"\\]|\\.)*"|\S+)')

# A change of pace needs at least this many gaps on each side of the split.
MIN_GAPS_PER_PART = 10
# The last part is reported when its median gap is more than this many times the first's.
PACE_FACTOR = 2.0


@dataclass
class PaceSplit:
    first_requests: int
    first_seconds: float
    last_requests: int
    last_seconds: float


@dataclass
class SyncRow:
    name: str
    origin: str | None
    started: datetime
    ended: datetime | None = None
    closed_by: str | None = None
    fetches: list = field(default_factory=list)
    summary: dict | None = None
    not_separable: bool = False
    pace_split: PaceSplit | None = None

    @property
    def host(self):
        return host_of(self.origin) if self.origin else None

    @property
    def requests(self):
        return len(self.fetches)

    @property
    def duration(self):
        return seconds(self.started, self.ended)

    @property
    def before_first(self):
        return seconds(self.started, self.fetches[0]) if self.fetches else None

    @property
    def first_to_last(self):
        return seconds(self.fetches[0], self.fetches[-1]) if self.fetches else None

    @property
    def after_last(self):
        return seconds(self.fetches[-1], self.ended) if self.fetches else None

    @property
    def gaps(self):
        return [gap(a, b) for a, b in zip(self.fetches, self.fetches[1:])]

    @property
    def median_gap(self):
        gaps = self.gaps
        return statistics.median(gaps) if gaps else None

    @property
    def requests_per_second(self):
        return rate(self.requests, self.first_to_last)


@dataclass
class Report:
    rows: list
    unattributed: int
    fetch_lines: int


def seconds(a, b):
    if a is None or b is None:
        return None
    return gap(a, b)


def gap(a: datetime, b: datetime) -> float:
    return (b - a).total_seconds()


def host_of(url):
    try:
        return (urlsplit(url).hostname or "").lower() or None
    except ValueError:
        return None


def parse_ts(line):
    m = TS_RE.match(line)
    return datetime.fromisoformat(m.group(1)) if m else None


def parse_summary(rest):
    fields = {}
    for key, value in FIELD_RE.findall(rest):
        if value.startswith('"') and value.endswith('"'):
            value = value[1:-1].replace('\\"', '"').replace("\\\\", "\\")
        fields[key] = value
    return fields


def find_pace_split(times):
    """Split point of an L1 two-part fit of the gaps, reported only when the last part's
    median gap is more than PACE_FACTOR times the first part's."""
    gaps = [gap(a, b) for a, b in zip(times, times[1:])]
    n = len(gaps)
    if n < 2 * MIN_GAPS_PER_PART:
        return None
    best = None
    for k in range(MIN_GAPS_PER_PART, n - MIN_GAPS_PER_PART + 1):
        before, after = gaps[:k], gaps[k:]
        mb, ma = statistics.median(before), statistics.median(after)
        cost = sum(abs(g - mb) for g in before) + sum(abs(g - ma) for g in after)
        if best is None or cost < best[0]:
            best = (cost, k, mb, ma)
    if best is None:
        return None
    _, k, mb, ma = best
    if ma <= PACE_FACTOR * mb:
        return None
    # gaps[:k] lie between times[0..k]: the first part is k + 1 requests, the rest follow.
    first_requests = k + 1
    return PaceSplit(
        first_requests=first_requests,
        first_seconds=gap(times[0], times[k]),
        last_requests=len(times) - first_requests,
        last_seconds=gap(times[k], times[-1]),
    )


def analyze(path, origins, single):
    rows = []
    open_syncs = {}
    unattributed = 0
    fetch_lines = 0
    first_ts = last_ts = None
    single_row = None

    with open(path, encoding="utf-8", errors="replace") as fh:
        for raw in fh:
            line = raw.rstrip("\n")
            t = parse_ts(line)
            if t is None:
                continue
            first_ts = first_ts or t
            last_ts = t

            if single:
                if single_row is None:
                    single_row = SyncRow(name="(single)", origin=None, started=t)
                    rows.append(single_row)
                if m := FETCH_RE.search(line):
                    fetch_lines += 1
                    single_row.fetches.append(t)
                elif m := SUMMARY_RE.search(line):
                    single_row.summary = parse_summary(m.group(1))
                    single_row.name = single_row.summary.get("source", single_row.name)
                    single_row.origin = single_row.summary.get("origin")
                continue

            if m := FETCH_RE.search(line):
                fetch_lines += 1
                target = attribute(open_syncs, host_of(m.group(1)))
                if target is None:
                    unattributed += 1
                else:
                    for row in target:
                        row.fetches.append(t)
            elif m := START_RE.search(line):
                name = m.group(1)
                row = SyncRow(name=name, origin=m.group(2) or origins.get(name), started=t)
                rows.append(row)
                open_syncs[name] = row
            elif m := COMPLETE_RE.search(line):
                close(open_syncs, m.group(1), t, "complete")
            elif m := FAILED_RE.search(line):
                close(open_syncs, m.group(1), t, "failed")
            elif m := SUMMARY_RE.search(line):
                fields = parse_summary(m.group(1))
                row = open_syncs.get(fields.get("source"))
                if row is not None:
                    row.summary = fields
                    row.origin = row.origin or fields.get("origin")

    if single_row is not None:
        single_row.ended = last_ts
    for row in open_syncs.values():
        row.ended = last_ts
        row.closed_by = "end of log"
    for row in rows:
        if row.summary is None and not row.not_separable:
            row.pace_split = find_pace_split(row.fetches)
    return Report(rows=rows, unattributed=unattributed, fetch_lines=fetch_lines)


def attribute(open_syncs, host):
    """The rows a fetch line belongs to, or None. Two open Syncs on one host share the
    line and are both marked not separable."""
    candidates = list(open_syncs.values())
    if not candidates:
        return None
    if len(candidates) == 1:
        return candidates
    matching = [row for row in candidates if row.host and row.host == host]
    if not matching:
        return None
    if len(matching) > 1:
        for row in matching:
            row.not_separable = True
    return matching


def close(open_syncs, name, t, how):
    row = open_syncs.pop(name, None)
    if row is not None:
        row.ended = t
        row.closed_by = how


def fmt(value, unit="", digits=1):
    if value is None:
        return "-"
    return f"{value:.{digits}f}{unit}"


# Summary fields already shown in the row itself; every other field is printed in log order.
ROW_FIELDS = ("source", "origin")


def print_report(result, out):
    header = (
        "Source",
        "Started (UTC)",
        "Duration",
        "Requests",
        "Before first",
        "First to last",
        "After last",
        "Median gap",
        "Req/s",
        "Note",
    )
    table: list[tuple[str, ...]] = [header]
    for row in result.rows:
        notes = []
        if row.not_separable:
            notes.append("not separable (shares a host with an overlapping Sync)")
        if row.closed_by and row.closed_by != "complete":
            notes.append(f"closed by {row.closed_by}")
        if row.origin is None and not row.summary:
            notes.append("origin unknown")
        table.append(
            (
                row.name,
                row.started.strftime("%Y-%m-%d %H:%M:%S"),
                fmt(row.duration, " s", 0),
                str(row.requests),
                fmt(row.before_first, " s"),
                fmt(row.first_to_last, " s"),
                fmt(row.after_last, " s"),
                fmt(row.median_gap, " s", 3),
                fmt(row.requests_per_second),
                "; ".join(notes),
            )
        )
    widths = [max(len(r[i]) for r in table) for i in range(len(header))]

    def print_cells(cells):
        print("  ".join(c.ljust(w) for c, w in zip(cells, widths)).rstrip(), file=out)

    print_cells(header)
    for row, cells in zip(result.rows, table[1:]):
        print_cells(cells)
        if row.pace_split:
            p = row.pace_split
            print(
                f"    change of pace: first {p.first_requests} requests in "
                f"{p.first_seconds:.1f} s ({fmt(rate(p.first_requests, p.first_seconds))}/s), "
                f"last {p.last_requests} in {p.last_seconds:.1f} s "
                f"({fmt(rate(p.last_requests, p.last_seconds))}/s)",
                file=out,
            )
        if row.summary:
            pairs = " ".join(f"{k}={v}" for k, v in row.summary.items() if k not in ROW_FIELDS)
            print(f"    web_sync_summary: {pairs}", file=out)
    print(f"unattributed fetch lines: {result.unattributed}", file=out)


def rate(n, secs):
    return n / secs if secs else None


def parse_origins(values):
    origins = {}
    for value in values or []:
        name, sep, url = value.partition("=")
        if not sep or not name or not url:
            raise SystemExit(f"--origin expects name=url, got {value!r}")
        origins[name] = url
    return origins


def main(argv=None):
    parser = argparse.ArgumentParser(description="Split a Citadel daemon log per web Sync.")
    parser.add_argument("log")
    parser.add_argument(
        "--origin",
        action="append",
        metavar="NAME=URL",
        help="Origin of a Source, for logs whose start lines lack origin= (repeatable)",
    )
    parser.add_argument(
        "--single", action="store_true", help="treat the whole file as one Sync"
    )
    args = parser.parse_args(argv)
    result = analyze(args.log, parse_origins(args.origin), args.single)
    if result.fetch_lines == 0:
        print("no fetch lines found (has spider's log format changed?)")
        return 1
    print_report(result, sys.stdout)
    return 0


if __name__ == "__main__":
    sys.exit(main())
