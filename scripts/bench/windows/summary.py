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
SPECIAL = ("startup", "windows100", "tabs16")
tests = list(dict.fromkeys(r["test"] for r in rows if r["test"] not in SPECIAL))


def med(rs, key):
    v = [r[key] for r in rs if r.get(key) is not None]
    return statistics.median(v) if v else None


def f(v, spec):
    return "—" if v is None else format(v, spec)


if tests or any(r["test"] == "startup" for r in ok):
    print("| Terminal | Startup, ms | Idle memory, MB |" + "".join(f" {t}: s (MB/s) |" for t in tests))
    print("|---|---:|---:|" + "---:|" * len(tests))
    for term in terms:
        tr = [r for r in ok if r["terminal"] == term]
        st = [r for r in tr if r["test"] == "startup"]
        cells = []
        for t in tests:
            x = [r for r in tr if r["test"] == t]
            c, m = med(x, "cat_s"), med(x, "mb_s")
            cells.append("—" if c is None else f"{c:.2f} ({m:.0f})")
        print(f"| {NAMES[term]} | {f(med(st, 'startup_ms'), '.0f')} | {f(med(st, 'idle_ws_mb'), '.0f')} |" + "".join(f" {c} |" for c in cells))

if tests:
    print()
    print("| Terminal | Test | s | settle, s | CPU, s | memory after, MB | grid |")
    print("|---|---|---:|---:|---:|---:|---|")
    for term in terms:
        for t in tests:
            x = [r for r in ok if r["terminal"] == term and r["test"] == t]
            if x:
                print(f"| {NAMES[term]} | {t} | {f(med(x, 'cat_s'), '.2f')} | {f(med(x, 'settle_s'), '.2f')} | "
                      f"{f(med(x, 'cpu_s'), '.2f')} | {f(med(x, 'ws_mb'), '.0f')} | {x[0].get('grid', '?')} |")

w = [r for r in ok if r["test"] == "windows100"]
if w:
    print()
    print(f"windows100: start the terminal {w[0].get('windows', 100)} times, each window closes at once")
    print()
    print("| Terminal | total, s | per window, ms | first, ms | slowest, ms |")
    print("|---|---:|---:|---:|---:|")
    for term in terms:
        x = [r for r in w if r["terminal"] == term]
        if x:
            print(f"| {NAMES[term]} | {f(med(x, 'total_s'), '.2f')} | {f(med(x, 'window_ms'), '.0f')} | "
                  f"{f(med(x, 'first_ms'), '.0f')} | {f(med(x, 'max_ms'), '.0f')} |")

t = [r for r in ok if r["test"] == "tabs16"]
if t:
    print()
    print(f"tabs16: {t[0].get('tabs', 16)} tabs, each `wsl.exe -e cat ascii.txt` at the same time (conhost: windows)")
    print()
    print("| Terminal | open tabs, s | all done, s (MB/s) | median tab, s | settle, s | CPU, s | idle memory, MB | memory after, MB | grid |")
    print("|---|---:|---:|---:|---:|---:|---:|---:|---|")
    for term in terms:
        x = [r for r in t if r["terminal"] == term]
        if x:
            print(f"| {NAMES[term]} | {f(med(x, 'open_s'), '.2f')} | {f(med(x, 'cat_s'), '.2f')} ({f(med(x, 'mb_s'), '.0f')}) | "
                  f"{f(med(x, 'tab_cat_s'), '.2f')} | {f(med(x, 'settle_s'), '.2f')} | {f(med(x, 'cpu_s'), '.2f')} | "
                  f"{f(med(x, 'idle_ws_mb'), '.0f')} | {f(med(x, 'ws_mb'), '.0f')} | {x[0].get('grid', '?')} |")

errors = [r for r in rows if r.get("error")]
if errors:
    print()
    print(f"{len(errors)} failed runs:")
    for r in errors:
        print(f"- {r['terminal']} {r['test']}: {r['error']}")
