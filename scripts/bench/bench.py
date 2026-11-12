#!/usr/bin/env python3
"""Terminal throughput / startup / memory benchmark. Runs inside the
bench-ubuntu-26.04 container (see run.sh).

    bench.py --data DIR --out DIR [--reps N] [--terminals a,b] [--tests a,b]

For every run: a fresh $HOME with the terminal's config, a fresh D-Bus
session, then the terminal is started with inner.sh as its command.

  startup  launch -> window mapped and the command running inside it
  cat      wall time of `cat FILE` inside the terminal: the PTY only drains
           as fast as the terminal reads and parses it
  settle   after cat returned, how long the terminal kept using CPU
           (rendering what it had buffered)
  cpu      CPU time of the terminal's processes from cat start to settle
  xcpu     same for the X server (Xvfb only): xterm, Konsole and the GTK
           terminals partly draw through it
  rss      memory: resident set at idle, peak (VmHWM) after the run

Two more tests are not a `cat` of one file:

  windows100  the terminal is started 100 times in a row (BENCH_WINDOWS),
              each running a command that exits at once, so its window
              closes. Time from launch until the launcher process has
              exited, and the CPU time used for all of them.
  tabs16      one window with 16 tabs (xterm: 16 windows), all doing
              `cat ascii.txt` at the same time. Time from the first `cat`
              starting to the last one ending; memory with 16 tabs open.

Every terminal uses DejaVu Sans Mono 11 pt, 10 000 lines of scrollback and
roughly the same window size (fastcnsl's default, 960x600).
"""
import argparse
import json
import os
import resource
import select
import shlex
import shutil
import signal
import statistics
import subprocess
import sys
import tempfile
import threading
import time

from Xlib import X, display as xdisplay

HERE = os.path.dirname(os.path.abspath(__file__))
INNER = os.path.join(HERE, "inner.sh")
FASTCNSL = os.environ.get("FASTCNSL", "/work/target/bench-ubuntu-26.04/release/fastcnsl")
FONT = "DejaVu Sans Mono"
FONT_PT = 11
SCROLLBACK = 10000
# Character grid matching fastcnsl's default 960x600 window with this font.
COLS, ROWS = int(os.environ.get("BENCH_COLS", 100)), int(os.environ.get("BENCH_ROWS", 30))
TICK = os.sysconf("SC_CLK_TCK")
WINDOWS = int(os.environ.get("BENCH_WINDOWS", 100))
TABS = int(os.environ.get("BENCH_TABS", 16))
TABS_FILE = "ascii"


def home_fastcnsl(home):
    d = os.path.join(home, ".config/fastcnsl")
    os.makedirs(d)
    with open(os.path.join(d, "config.toml"), "w") as f:
        f.write(f'font_family = "{FONT}"\nfont_size = {FONT_PT}.0\n'
                f'scrollback = {SCROLLBACK}\ntheme = "dark"\nshell = "/bin/sh"\n'
                # Extra settings, e.g. to measure what a feature costs.
                + os.environ.get("FASTCNSL_CONFIG_EXTRA", "").replace("\\n", "\n") + "\n")


def home_konsole(home):
    d = os.path.join(home, ".local/share/konsole")
    os.makedirs(d)
    with open(os.path.join(d, "bench.profile"), "w") as f:
        f.write("[General]\nName=bench\nParent=FALLBACK/\n"
                f"TerminalColumns={COLS}\nTerminalRows={ROWS}\n"
                f"[Appearance]\nFont={FONT},{FONT_PT},-1,5,400,0,0,0,0,0,0,0,0,0,0,1\n"
                f"[Scrolling]\nHistoryMode=1\nHistorySize={SCROLLBACK}\n")
    os.makedirs(os.path.join(home, ".config"))
    with open(os.path.join(home, ".config/konsolerc"), "w") as f:
        f.write("[Desktop Entry]\nDefaultProfile=bench.profile\n")


def home_gsettings(home):
    # GLib's keyfile backend instead of dconf: plain file, no extra daemon.
    d = os.path.join(home, ".config/glib-2.0/settings")
    os.makedirs(d)
    with open(os.path.join(d, "keyfile"), "w") as f:
        f.write(f"[org/gnome/desktop/interface]\nmonospace-font-name='{FONT} {FONT_PT}'\n"
                f"[org/gnome/Ptyxis]\ndefault-columns=uint32 {COLS}\ndefault-rows=uint32 {ROWS}\n")


def fastcnsl_argv(*args):
    gpu = ["--gpu", os.environ["FASTCNSL_GPU"]] if os.environ.get("FASTCNSL_GPU") else []
    return [FASTCNSL, *gpu, *args]


def konsole_tabs(inners, tmp):
    path = os.path.join(tmp, "tabs.txt")
    with open(path, "w") as f:
        for i, inner in enumerate(inners):
            f.write(f"title: t{i};; profile: bench;; command: {shlex.join(inner)}\n")
    return [["konsole", "--nofork", "--profile", "bench", "--tabs-from-file", path]]


def gnome_tabs(inners, tmp):
    # One call: `--tab` in a later call opens in the active window, and
    # with no window manager there is none, so it would open a window.
    argv = ["gnome-terminal", f"--geometry={COLS}x{ROWS}"]
    for i, inner in enumerate(inners):
        argv += ["--window" if i == 0 else "--tab", "-e", shlex.join(inner)]
    return [argv]


# tabs: argument lists that open one tab per inner command; every call
# after the first goes to the window the first one opened.
TERMINALS = {
    "fastcnsl": dict(
        comms={"fastcnsl"},
        home=home_fastcnsl,
        cmd=lambda inner: fastcnsl_argv("new-tab", "bench", "--", *inner),
        tabs=lambda inners, tmp: [fastcnsl_argv("new-tab", f"t{i}", "--", *inner) for i, inner in enumerate(inners)],
        tab_call_exits=True,
    ),
    "konsole": dict(
        comms={"konsole"},
        home=home_konsole,
        cmd=lambda inner: ["konsole", "--nofork", "--profile", "bench", "-e", *inner],
        tabs=konsole_tabs,
    ),
    "xterm": dict(
        comms={"xterm"},
        home=None,
        cmd=lambda inner: ["xterm", "-fa", FONT, "-fs", str(FONT_PT), "-sl", str(SCROLLBACK),
                           "-geometry", f"{COLS}x{ROWS}", "-e", *inner],
        # No tabs: one window each.
        tabs=lambda inners, tmp: [TERMINALS["xterm"]["cmd"](inner) for inner in inners],
    ),
    "gnome-terminal": dict(
        comms={"gnome-terminal-"},  # gnome-terminal-server, comm is cut to 15 chars
        home=home_gsettings,
        cmd=lambda inner: ["gnome-terminal", "--wait", f"--geometry={COLS}x{ROWS}", "--", *inner],
        tabs=gnome_tabs,
    ),
    "ptyxis": dict(
        comms={"ptyxis", "ptyxis-agent"},
        home=home_gsettings,
        cmd=lambda inner: ["ptyxis", "--standalone", "--", *inner],
        # Not --standalone: later calls reach the first over D-Bus.
        tabs=lambda inners, tmp: [["ptyxis", "--new-window", "--", *inners[0]]] +
                                 [["ptyxis", "--tab", "-x", shlex.join(inner)] for inner in inners[1:]],
        tab_call_exits=True,
    ),
}


def procs():
    out = {}
    for p in os.listdir("/proc"):
        if p.isdigit():
            try:
                with open(f"/proc/{p}/comm") as f:
                    out[int(p)] = f.read().strip()
            except OSError:
                pass
    return out


def cpu_of(pids):
    total = 0
    for p in pids:
        try:
            with open(f"/proc/{p}/stat") as f:
                fields = f.read().rsplit(")", 1)[1].split()
            total += int(fields[11]) + int(fields[12])
        except OSError:
            pass
    return total / TICK


def mem_of(pids):
    rss = hwm = 0
    for p in pids:
        try:
            with open(f"/proc/{p}/status") as f:
                for line in f:
                    if line.startswith("VmRSS:"):
                        rss += int(line.split()[1])
                    elif line.startswith("VmHWM:"):
                        hwm += int(line.split()[1])
        except OSError:
            pass
    return rss / 1024, hwm / 1024


class MapWatcher(threading.Thread):
    """Records when the first top-level window is mapped."""

    def __init__(self):
        super().__init__(daemon=True)
        self.d = xdisplay.Display()
        self.d.screen().root.change_attributes(event_mask=X.SubstructureNotifyMask)
        self.d.sync()
        self.mapped_ns = None
        self.stop = False
        self.start()

    def run(self):
        while not self.stop and self.mapped_ns is None:
            if not self.d.pending_events():
                select.select([self.d.fileno()], [], [], 0.05)
                if not self.d.pending_events():
                    continue
            ev = self.d.next_event()
            if ev.type == X.MapNotify and not ev.override:
                self.mapped_ns = time.monotonic_ns()
        self.d.close()


def wait_file(path, timeout, alive=None):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        if os.path.exists(path):
            with open(path) as f:
                return f.read().split()
        if alive is not None and alive.poll() is not None and alive.returncode != 0:
            raise RuntimeError(f"launcher exited with {alive.returncode}")
        time.sleep(0.005)
    raise TimeoutError(path)


def kill_new(baseline):
    for _ in range(3):
        left = [p for p in procs() if p not in baseline]
        if not left:
            return
        for p in left:
            try:
                os.kill(p, signal.SIGKILL)
            except OSError:
                pass
        time.sleep(0.2)


def wait_settled(tpids, t_done):
    """Waits until the terminal is idle: under 10 ms of CPU over the last
    300 ms. Returns (settle seconds, CPU seconds at the end)."""
    samples = [(t_done, cpu_of(tpids))]
    while time.monotonic() - t_done < 120:
        time.sleep(0.1)
        samples.append((time.monotonic(), cpu_of(tpids)))
        if len(samples) > 3 and samples[-1][1] - samples[-4][1] < 0.010:
            break
    settle = max(0.0, samples[-4][0] - t_done) if len(samples) > 3 else 0.0
    return settle, samples[-1][1]


def read_done(rundir):
    done = wait_file(os.path.join(rundir, "done"), 900)
    if len(done) > 1 and done[1] != "0":
        raise RuntimeError(f"cat exited with {done[1]}")
    return [int(x) for x in done]


def one_run(term, test, data, xserver_pids, baseline, log):
    spec = TERMINALS[term]
    tmp = tempfile.mkdtemp(prefix=f"run-{term}-")
    home, rundir, runtime = (os.path.join(tmp, x) for x in ("home", "run", "xdg"))
    for d in (home, rundir, runtime):
        os.makedirs(d, mode=0o700)
    if spec["home"]:
        spec["home"](home)
    env = {k: v for k, v in os.environ.items() if k not in ("WAYLAND_DISPLAY", "DBUS_SESSION_BUS_ADDRESS")}
    env.update(HOME=home, XDG_RUNTIME_DIR=runtime, LANG="C.UTF-8", LC_ALL="C.UTF-8",
               SHELL="/bin/sh", GSETTINGS_BACKEND="keyfile", NO_AT_BRIDGE="1",
               QT_QPA_PLATFORM="xcb", GDK_BACKEND="x11", TERM="xterm-256color",
               RUST_LOG="fastcnsl=info")
    bus = subprocess.Popen(["dbus-daemon", "--session", "--nofork", "--nopidfile", "--print-address=1",
                            f"--address=unix:path={runtime}/bus"],
                           stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, env=env, text=True)
    env["DBUS_SESSION_BUS_ADDRESS"] = bus.stdout.readline().strip()

    res = dict(terminal=term, test=test)
    run = dict(spec=spec, env=env, log=log, tmp=tmp, rundir=rundir, data=data,
               xserver_pids=xserver_pids, baseline=baseline)
    try:
        if test == "windows100":
            run_windows(res, **run)
        elif test == "tabs16":
            run_tabs(res, **run)
        else:
            run_cat(res, test, **run)
    except Exception as e:  # noqa: BLE001 - record and go on with the others
        res["error"] = f"{type(e).__name__}: {e}"
    finally:
        kill_new(baseline)
        shutil.rmtree(tmp, ignore_errors=True)
    return res


def run_cat(res, test, spec, env, log, tmp, rundir, data, xserver_pids, baseline):
    inner = ["/bin/sh", INNER, rundir, os.path.join(data, f"{test}.txt")]
    watcher = MapWatcher()
    t_launch = time.monotonic_ns()
    launcher = subprocess.Popen(spec["cmd"](inner), env=env, stdout=log, stderr=log,
                                stdin=subprocess.DEVNULL, start_new_session=True)
    try:
        begin = wait_file(os.path.join(rundir, "begin"), 60, launcher)
        t_begin = time.monotonic_ns()  # seen within 5 ms of being written
        time.sleep(0.05)  # let the map event arrive if it is just behind
    finally:
        watcher.stop = True
    res["grid"] = f"{begin[2]}x{begin[1]}" if len(begin) == 3 else "?"
    ready = max(t_begin, watcher.mapped_ns or t_begin)
    res["startup_ms"] = (ready - t_launch) / 1e6
    res["shell_ms"] = (t_begin - t_launch) / 1e6
    res["map_ms"] = (watcher.mapped_ns - t_launch) / 1e6 if watcher.mapped_ns else None

    time.sleep(1.0)  # idle a moment before sampling
    tpids = [p for p, c in procs().items() if c in spec["comms"] and p not in baseline]
    res["procs"] = len(tpids)
    res["idle_rss_mb"] = mem_of(tpids)[0]
    cpu0, x0 = cpu_of(tpids), cpu_of(xserver_pids)
    open(os.path.join(rundir, "go"), "w").close()

    done = read_done(rundir)
    t_done = time.monotonic()
    res["cat_s"] = done[0] / 1e9
    size = os.path.getsize(os.path.join(data, f"{test}.txt"))
    res["mb_s"] = size / 1048576 / res["cat_s"]
    res["settle_s"], cpu1 = wait_settled(tpids, t_done)
    res["cpu_s"] = cpu1 - cpu0
    res["xcpu_s"] = cpu_of(xserver_pids) - x0 if xserver_pids else None
    res["rss_mb"], res["peak_rss_mb"] = mem_of(tpids)


def run_windows(res, spec, env, log, tmp, rundir, data, xserver_pids, baseline):
    """Starts the terminal WINDOWS times in a row; each runs a command that
    exits at once, which closes the window and (except for GNOME
    Terminal's server) ends the terminal."""
    times = []
    x0 = cpu_of(xserver_pids)
    kids0 = resource.getrusage(resource.RUSAGE_CHILDREN)
    t_start = time.monotonic()
    for i in range(WINDOWS):
        marker = os.path.join(rundir, f"w{i}")
        t0 = time.monotonic_ns()
        p = subprocess.Popen(spec["cmd"](["/bin/sh", "-c", f": > {marker}"]), env=env, stdout=log, stderr=log,
                             stdin=subprocess.DEVNULL, start_new_session=True)
        # Not p.wait(timeout): it polls, in steps of up to 50 ms. A pidfd
        # becomes readable the moment the process exits.
        pidfd = os.pidfd_open(p.pid)
        try:
            if not select.select([pidfd], [], [], 60)[0]:
                raise TimeoutError(f"window {i} still open after 60 s")
        finally:
            os.close(pidfd)
        times.append((time.monotonic_ns() - t0) / 1e6)
        p.wait()
        if not os.path.exists(marker):
            raise RuntimeError(f"window {i} closed without running its command (exit {p.returncode})")
        if p.returncode != 0:  # e.g. a crash on close: -6 is SIGABRT
            raise RuntimeError(f"window {i}: the terminal exited with {p.returncode}")
    res["total_s"] = time.monotonic() - t_start
    kids1 = resource.getrusage(resource.RUSAGE_CHILDREN)
    # Launchers that were waited for, plus processes still running that the
    # test started (gnome-terminal-server).
    alive = [p for p, c in procs().items() if c in spec["comms"] and p not in baseline]
    res["cpu_s"] = (kids1.ru_utime - kids0.ru_utime + kids1.ru_stime - kids0.ru_stime) + cpu_of(alive)
    res["xcpu_s"] = cpu_of(xserver_pids) - x0 if xserver_pids else None
    res["windows"] = WINDOWS
    res["window_ms"] = statistics.median(times)
    res["first_ms"] = times[0]
    res["max_ms"] = max(times)


def run_tabs(res, spec, env, log, tmp, rundir, data, xserver_pids, baseline):
    """One window with TABS tabs, each doing `cat TABS_FILE` at the same time."""
    file = os.path.join(data, f"{TABS_FILE}.txt")
    dirs = [os.path.join(rundir, f"t{i}") for i in range(TABS)]
    for d in dirs:
        os.makedirs(d)
    calls = spec["tabs"]([["/bin/sh", INNER, d, file] for d in dirs], tmp)
    launchers = []
    t_launch = time.monotonic()
    for i, argv in enumerate(calls):
        launchers.append(subprocess.Popen(argv, env=env, stdout=log, stderr=log,
                                          stdin=subprocess.DEVNULL, start_new_session=True))
        if i == 0 and len(calls) > 1:
            # The window that later calls open their tabs in.
            wait_file(os.path.join(dirs[0], "begin"), 60, launchers[0])
        elif spec.get("tab_call_exits"):
            launchers[-1].wait(timeout=30)  # it only asks the window for a tab
    begins = [wait_file(os.path.join(d, "begin"), 120, launchers[0]) for d in dirs]
    res["open_s"] = time.monotonic() - t_launch
    res["grid"] = f"{begins[0][2]}x{begins[0][1]}" if len(begins[0]) == 3 else "?"

    time.sleep(1.0)
    tpids = [p for p, c in procs().items() if c in spec["comms"] and p not in baseline]
    res["procs"] = len(tpids)
    res["idle_rss_mb"] = mem_of(tpids)[0]
    cpu0, x0 = cpu_of(tpids), cpu_of(xserver_pids)
    for d in dirs:
        open(os.path.join(d, "go"), "w").close()

    done = [read_done(d) for d in dirs]
    t_done = time.monotonic()
    res["cat_s"] = (max(d[3] for d in done) - min(d[2] for d in done)) / 1e9
    res["tab_cat_s"] = statistics.median(d[0] for d in done) / 1e9
    res["mb_s"] = TABS * os.path.getsize(file) / 1048576 / res["cat_s"]
    res["settle_s"], cpu1 = wait_settled(tpids, t_done)
    res["cpu_s"] = cpu1 - cpu0
    res["xcpu_s"] = cpu_of(xserver_pids) - x0 if xserver_pids else None
    res["rss_mb"], res["peak_rss_mb"] = mem_of(tpids)
    res["tabs"] = TABS


def med(rows, key):
    vals = [r[key] for r in rows if r.get(key) is not None]
    return statistics.median(vals) if vals else None


def fmt(v, spec):
    return "—" if v is None else format(v, spec)


SPECIAL = ("windows100", "tabs16")


def summary(results, terms, all_tests):
    tests = [t for t in all_tests if t not in SPECIAL]
    out = []
    if tests:
        out.append("| Terminal | Grid | Startup, ms | Idle RSS, MB | " +
                   " | ".join(f"{t}: cat s (MB/s)" for t in tests) + " |")
        out.append("|---|---|---:|---:|" + "---:|" * len(tests))
        for term in terms:
            rows = [r for r in results if r["terminal"] == term and r["test"] in tests and "error" not in r]
            if not rows:
                out.append(f"| {term} | failed |" + " |" * (2 + len(tests)))
                continue
            cells = []
            for t in tests:
                tr = [r for r in rows if r["test"] == t]
                c, m = med(tr, "cat_s"), med(tr, "mb_s")
                cells.append("—" if c is None else f"{c:.2f} ({m:.0f})")
            out.append(f"| {term} | {rows[0]['grid']} | {fmt(med(rows, 'startup_ms'), '.0f')} | "
                       f"{fmt(med(rows, 'idle_rss_mb'), '.0f')} | " + " | ".join(cells) + " |")
        out.append("")
        out.append("| Terminal | Test | cat, s | settle, s | terminal CPU, s | X server CPU, s | peak RSS, MB |")
        out.append("|---|---|---:|---:|---:|---:|---:|")
        for term in terms:
            for t in tests:
                tr = [r for r in results if r["terminal"] == term and r["test"] == t and "error" not in r]
                if not tr:
                    continue
                out.append(f"| {term} | {t} | {fmt(med(tr, 'cat_s'), '.2f')} | {fmt(med(tr, 'settle_s'), '.2f')} | "
                           f"{fmt(med(tr, 'cpu_s'), '.2f')} | {fmt(med(tr, 'xcpu_s'), '.2f')} | "
                           f"{fmt(med(tr, 'peak_rss_mb'), '.0f')} |")
    ok = lambda term, t: [r for r in results if r["terminal"] == term and r["test"] == t and "error" not in r]
    if "windows100" in all_tests:
        out.append("")
        out.append(f"windows100: start the terminal {WINDOWS} times, each window closes at once")
        out.append("")
        out.append("| Terminal | total, s | per window, ms | first, ms | slowest, ms | terminal CPU, s | X server CPU, s |")
        out.append("|---|---:|---:|---:|---:|---:|---:|")
        for term in terms:
            tr = ok(term, "windows100")
            if tr:
                out.append(f"| {term} | {fmt(med(tr, 'total_s'), '.2f')} | {fmt(med(tr, 'window_ms'), '.0f')} | "
                           f"{fmt(med(tr, 'first_ms'), '.0f')} | {fmt(med(tr, 'max_ms'), '.0f')} | "
                           f"{fmt(med(tr, 'cpu_s'), '.2f')} | {fmt(med(tr, 'xcpu_s'), '.2f')} |")
    if "tabs16" in all_tests:
        out.append("")
        out.append(f"tabs16: {TABS} tabs, each `cat {TABS_FILE}.txt` at the same time")
        out.append("")
        out.append("| Terminal | Grid | open tabs, s | all done, s (MB/s) | median tab, s | settle, s | "
                   "terminal CPU, s | X server CPU, s | idle RSS, MB | peak RSS, MB |")
        out.append("|---|---|---:|---:|---:|---:|---:|---:|---:|---:|")
        for term in terms:
            tr = ok(term, "tabs16")
            if tr:
                out.append(f"| {term} | {tr[0]['grid']} | {fmt(med(tr, 'open_s'), '.2f')} | "
                           f"{fmt(med(tr, 'cat_s'), '.2f')} ({fmt(med(tr, 'mb_s'), '.0f')}) | "
                           f"{fmt(med(tr, 'tab_cat_s'), '.2f')} | {fmt(med(tr, 'settle_s'), '.2f')} | "
                           f"{fmt(med(tr, 'cpu_s'), '.2f')} | {fmt(med(tr, 'xcpu_s'), '.2f')} | "
                           f"{fmt(med(tr, 'idle_rss_mb'), '.0f')} | {fmt(med(tr, 'peak_rss_mb'), '.0f')} |")
    return "\n".join(out)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--data", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--reps", type=int, default=3)
    ap.add_argument("--terminals", default=",".join(TERMINALS))
    ap.add_argument("--tests", default="ascii,color,unicode,lines,windows100,tabs16")
    a = ap.parse_args()
    terms, tests = a.terminals.split(","), a.tests.split(",")
    a.data = os.path.abspath(a.data)
    os.makedirs(a.out, exist_ok=True)
    xserver = [p for p, c in procs().items() if c in ("Xvfb", "Xorg", "Xwayland")]
    baseline = set(procs())
    jsonl = open(os.path.join(a.out, "results.jsonl"), "a")
    log = open(os.path.join(a.out, "terminals.log"), "a")
    results = []
    # Interleaved, so slow drift on the host spreads over all terminals.
    for rep in range(a.reps):
        for test in tests:
            for term in terms:
                r = one_run(term, test, a.data, xserver, baseline, log)
                r["rep"] = rep
                results.append(r)
                jsonl.write(json.dumps(r) + "\n")
                jsonl.flush()
                if r.get("error"):
                    status = r["error"]
                elif test == "windows100":
                    status = (f"total {r['total_s']:.2f}s per window {r['window_ms']:.0f}ms "
                              f"(first {r['first_ms']:.0f}, max {r['max_ms']:.0f}) cpu {r['cpu_s']:.2f}s")
                elif test == "tabs16":
                    status = (f"open {r['open_s']:.2f}s all done {r['cat_s']:.2f}s settle {r['settle_s']:.2f}s "
                              f"cpu {r['cpu_s']:.2f}s idle {r['idle_rss_mb']:.0f}MB peak {r['peak_rss_mb']:.0f}MB "
                              f"grid {r['grid']}")
                else:
                    status = (f"cat {r['cat_s']:.2f}s settle {r['settle_s']:.2f}s "
                              f"cpu {r['cpu_s']:.2f}s startup {r['startup_ms']:.0f}ms "
                              f"peak {r['peak_rss_mb']:.0f}MB grid {r['grid']}")
                print(f"[{rep + 1}/{a.reps}] {test:8} {term:15} {status}", flush=True)
    table = summary(results, terms, tests)
    with open(os.path.join(a.out, "summary.md"), "w") as f:
        f.write(table + "\n")
    print()
    print(table)


if __name__ == "__main__":
    sys.exit(main())
