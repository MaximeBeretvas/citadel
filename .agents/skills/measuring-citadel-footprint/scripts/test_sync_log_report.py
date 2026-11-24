"""Tests for sync_log_report.py. Run:
python3 -m unittest discover -s .agents/skills/measuring-citadel-footprint/scripts -p 'test_*.py'
"""

import contextlib
import io
import os
import sys
import tempfile
import unittest
from datetime import datetime, timedelta, timezone

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import sync_log_report as report  # noqa: E402

BASE = datetime(2026, 11, 22, 19, 0, 0, tzinfo=timezone.utc)


def ts(seconds):
    t = BASE + timedelta(seconds=seconds)
    return t.strftime("%Y-%m-%dT%H:%M:%S.%f") + "Z"


def start(sec, name, origin=None):
    tail = f" origin={origin}" if origin else ""
    return f"{ts(sec)}  INFO citadel::daemon: starting scheduled web sync source={name}{tail}"


def complete(sec, name):
    return (
        f"{ts(sec)}  INFO citadel::daemon: scheduled web sync complete source={name} "
        "added=1 updated=0 unchanged=0 stale=0 failed=0"
    )


def failed(sec, name):
    return (
        f"{ts(sec)}  WARN citadel::daemon: scheduled web sync failed source={name} "
        "error=no reachable pages"
    )


def fetch(sec, url):
    return f"{ts(sec)}  INFO spider::utils: fetch {url}"


def summary(sec, name, origin):
    return (
        f'{ts(sec)}  INFO citadel::ingest::web: web_sync_summary source="{name}" origin="{origin}" '
        "ok=true max_pages=500 cut_off=false delay_ms=100 pages_received=3 results_stored=2 "
        "results_failed=0 lost_by_listener=0 unparsable_url=0 already_collected=0 "
        "redirected_out_of_scope=1 url_not_storable=0 duplicate_result=0 binary_file=0 "
        "later_crawls_started=0 discovery_ms=12 first_crawl_ms=900 later_crawls_ms=0 "
        "conversion_ms=30 write_ms=-"
    )


class SyncLogReportTest(unittest.TestCase):
    def write_log(self, lines):
        fd, path = tempfile.mkstemp(suffix=".log")
        with os.fdopen(fd, "w") as fh:
            fh.write("\n".join(lines) + "\n")
        self.addCleanup(os.remove, path)
        return path

    def rows_by_name(self, result):
        return {row.name: row for row in result.rows}

    def test_overlapping_syncs_on_different_hosts_are_split(self):
        path = self.write_log(
            [
                start(0, "alpha", "https://alpha.example/docs/"),
                fetch(1, "https://alpha.example/docs/"),
                start(1.5, "beta", "https://beta.example/"),
                fetch(2, "https://beta.example/"),
                fetch(2.1, "https://alpha.example/docs/a"),
                fetch(2.2, "https://beta.example/b"),
                fetch(2.3, "https://beta.example/c"),
                summary(2.9, "beta", "https://beta.example/"),
                failed(3, "beta"),
                fetch(3.5, "https://alpha.example/docs/b"),
                complete(4, "alpha"),
            ]
        )
        result = report.analyze(path, origins={}, single=False)
        rows = self.rows_by_name(result)
        self.assertEqual(rows["alpha"].requests, 3)
        self.assertEqual(rows["beta"].requests, 3)
        self.assertEqual(rows["beta"].closed_by, "failed")
        self.assertEqual(rows["alpha"].closed_by, "complete")
        self.assertAlmostEqual(rows["alpha"].duration, 4.0, places=3)
        self.assertAlmostEqual(rows["alpha"].before_first, 1.0, places=3)
        self.assertAlmostEqual(rows["alpha"].after_last, 0.5, places=3)
        self.assertEqual(rows["beta"].summary["redirected_out_of_scope"], "1")
        self.assertEqual(rows["beta"].summary["write_ms"], "-")
        self.assertEqual(rows["beta"].summary["source"], "beta")
        self.assertFalse(rows["alpha"].not_separable)
        self.assertEqual(result.unattributed, 0)

    def test_overlapping_syncs_on_one_host_are_not_separable(self):
        path = self.write_log(
            [
                start(0, "one", "https://same.example/a/"),
                start(0.5, "two", "https://same.example/b/"),
                fetch(1, "https://same.example/a/"),
                fetch(1.1, "https://same.example/b/"),
                complete(2, "one"),
                complete(3, "two"),
            ]
        )
        rows = self.rows_by_name(report.analyze(path, origins={}, single=False))
        self.assertTrue(rows["one"].not_separable)
        self.assertTrue(rows["two"].not_separable)

    def test_old_log_with_origin_flag_reports_slow_last_part(self):
        lines = [start(0, "uv")]
        t = 1.0
        for i in range(40):  # fast part: 0.1 s apart
            lines.append(fetch(t, f"https://docs.astral.sh/uv/p{i}"))
            t += 0.1
        t += 0.3
        for i in range(25):  # slow part: 0.4 s apart
            lines.append(fetch(t, f"https://docs.astral.sh/uv/s{i}/index.md"))
            t += 0.4
        lines.append(complete(t + 1, "uv"))
        path = self.write_log(lines)
        rows = self.rows_by_name(
            report.analyze(path, origins={"uv": "https://docs.astral.sh/uv/"}, single=False)
        )
        row = rows["uv"]
        self.assertEqual(row.requests, 65)
        self.assertIsNotNone(row.pace_split)
        self.assertEqual(row.pace_split.first_requests, 40)
        self.assertEqual(row.pace_split.last_requests, 25)

    def test_steady_pace_is_not_split(self):
        lines = [start(0, "steady", "https://s.example/")]
        for i in range(60):
            lines.append(fetch(1 + i * 0.1 + (0.004 if i % 3 else 0), f"https://s.example/{i}"))
        lines.append(complete(10, "steady"))
        rows = self.rows_by_name(report.analyze(self.write_log(lines), origins={}, single=False))
        self.assertIsNone(rows["steady"].pace_split)

    def test_single_mode_gives_one_row(self):
        path = self.write_log(
            [
                f"{ts(0)}  INFO citadel::cli: syncing source",
                fetch(1, "https://x.example/"),
                fetch(1.2, "https://x.example/a"),
                fetch(1.4, "https://x.example/b"),
                f"{ts(2)}  INFO citadel::cli: done",
            ]
        )
        result = report.analyze(path, origins={}, single=True)
        self.assertEqual(len(result.rows), 1)
        self.assertEqual(result.rows[0].requests, 3)
        self.assertAlmostEqual(result.rows[0].duration, 2.0, places=3)

    def test_fetch_outside_any_sync_is_unattributed(self):
        path = self.write_log(
            [
                fetch(0, "https://stray.example/"),
                start(1, "a", "https://a.example/"),
                fetch(2, "https://a.example/"),
                complete(3, "a"),
                fetch(4, "https://a.example/late"),
            ]
        )
        result = report.analyze(path, origins={}, single=False)
        self.assertEqual(result.unattributed, 2)
        self.assertEqual(self.rows_by_name(result)["a"].requests, 1)

    def test_no_fetch_lines_found(self):
        path = self.write_log([start(0, "a", "https://a.example/"), complete(1, "a")])
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = report.main([path])
        self.assertIn("no fetch lines found", out.getvalue())
        self.assertNotEqual(code, 0)


if __name__ == "__main__":
    unittest.main()
