#!/usr/bin/env python3
"""Markdown tables (medians) from the results.jsonl of windows/bench.ps1.

    summary.py results.jsonl
"""
import json
import statistics
import sys

NAMES = {"fastcnsl": "fastcnsl", "wt": "Windows Terminal", "conhost": "conhost (classic console)"}

rows = []
with open(sys.argv[1], encoding="utf-8-sig") as f:
    for line in f:
        line = line.strip().lstrip("﻿")
        if line:
            rows.append(json.loads(line))
ok = [r for r in rows if not r.get("error")]
terms = [t for t in NAMES if any(r["terminal"] == t for r in rows)]
tests = list(dict.fromkeys(r["test"] for r in rows if r["test"] != "startup"))


def med(rs, key):
    v = [r[key] for r in rs if r.get(key) is not None]
    return statistics.median(v) if v else None


def f(v, spec):
    return "—" if v is None else format(v, spec)


print("| Terminal | Startup, ms | Idle memory, MB | " + " | ".join(f"{t}: s (MB/s)" for t in tests) + " |")
print("|---|---:|---:|" + "---:|" * len(tests))
for term in terms:
    tr = [r for r in ok if r["terminal"] == term]
    st = [r for r in tr if r["test"] == "startup"]
    cells = []
    for t in tests:
        x = [r for r in tr if r["test"] == t]
        c, m = med(x, "cat_s"), med(x, "mb_s")
        cells.append("—" if c is None else f"{c:.2f} ({m:.0f})")
    print(f"| {NAMES[term]} | {f(med(st, 'startup_ms'), '.0f')} | {f(med(st, 'idle_ws_mb'), '.0f')} | " + " | ".join(cells) + " |")

print()
print("| Terminal | Test | s | settle, s | CPU, s | memory after, MB | grid |")
print("|---|---|---:|---:|---:|---:|---|")
for term in terms:
    for t in tests:
        x = [r for r in ok if r["terminal"] == term and r["test"] == t]
        if x:
            print(f"| {NAMES[term]} | {t} | {f(med(x, 'cat_s'), '.2f')} | {f(med(x, 'settle_s'), '.2f')} | "
                  f"{f(med(x, 'cpu_s'), '.2f')} | {f(med(x, 'ws_mb'), '.0f')} | {x[0].get('grid', '?')} |")

errors = [r for r in rows if r.get("error")]
if errors:
    print()
    print(f"{len(errors)} failed runs:")
    for r in errors:
        print(f"- {r['terminal']} {r['test']}: {r['error']}")
