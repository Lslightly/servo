import unittest

import analyze_pool_stats as a


def record(**over):
    r = {
        "pid": 1, "runtime": 1, "thread": "Script#1",
        "pushes": {"result": 10, "borrowed": 0, "root": 0, "deferred": 0},
        "apis": [{"api": "X", "kind": "result", "total": 10,
                  "fresh": 10, "rooted": 10, "kept": 0, "dropped": 0}],
        "drains": [
            {"ctx": "native_call", "calls": 100, "empty": 0, "total_ns": 150_000_000,
             "max_ns": 20_000_000, "values": 1000,
             "hist": [{"lo_ns": 1_048_576, "count": 1},
                      {"lo_ns": 1_024, "count": 99}]},
        ],
    }
    r.update(over)
    return r


def rows_of(*records):
    return a.drain_rows(a.aggregate(list(records), "ctx"))


class ConclusionsTest(unittest.TestCase):
    def test_conclusion_with_elapsed(self):
        text = a.conclusions([record(elapsed_ms=200)], rows_of(record(elapsed_ms=200)), [])
        self.assertIn("占运行时间 75.0%", text)

    def test_conclusion_without_elapsed(self):
        text = a.conclusions([record()], rows_of(record()), [])
        self.assertIn("缺 `elapsed_ms`", text)

    def test_conclusion_slow_tail(self):
        text = a.conclusions([record()], rows_of(record()), [])
        self.assertIn("至少 1 次 drain ≥ 1 ms", text)
        self.assertIn("20.0 ms", text)

    def test_conclusion_short_correlation(self):
        text = a.conclusions([record()], rows_of(record()), [])
        self.assertIn("相关性不可用", text)

    def test_conclusion_dropped(self):
        rec = record()
        rec["apis"][0]["dropped"] = 3
        text = a.conclusions([rec], rows_of(rec), [])
        self.assertIn("dropped = 3", text)


class SourceSectionTest(unittest.TestCase):
    def test_source_commands_are_fenced(self):
        text = a.source_section(["stats.jsonl"], ["QJS_POOL_STATS_SAMPLE=0 ./servoshell page.html"],
                                [record()])
        self.assertIn("```sh", text)
        self.assertIn("QJS_POOL_STATS_SAMPLE=0 ./servoshell page.html", text)
        self.assertIn("analyze_pool_stats.py", text)

    def test_source_quotes_paths_and_report(self):
        text = a.source_section(["dir/a b.jsonl"], [], [record()],
                                report="out/report p.md")
        self.assertIn("--report 'out/report p.md'", text)
        self.assertIn("'dir/a b.jsonl'", text)
