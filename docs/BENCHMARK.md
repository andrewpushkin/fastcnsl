# Benchmark: fastcnsl vs other terminals

- [Linux](#linux): Konsole, xterm, Ptyxis and GNOME Terminal on Ubuntu 26.04
- [Windows](#windows): Windows Terminal and the classic console (conhost)

## Linux

All five terminals run in one Ubuntu 26.04 container
([docker/bench-ubuntu-26.04.Dockerfile](../docker/bench-ubuntu-26.04.Dockerfile)).
Ptyxis is Ubuntu's default terminal since 25.10. GNOME Terminal was the
default before that.

```sh
make bench          # Xvfb, software rendering      -> dist/bench/
make bench-host     # WSLg display + GPU (d3d12)    -> dist/bench-host/
make bench BENCH_ARGS="--reps 5 --terminals fastcnsl,xterm --tests ascii"
```

Measured 2026-11-11 on an AMD Ryzen 9 7940HX (32 threads) under WSL2 (kernel
6.6.87). Versions: fastcnsl 0.1.85, Konsole 25.12.3, XTerm 407,
GNOME Terminal 3.58.0, Ptyxis 50.1 and Mesa 26.0.8. Each value is the median of 5 runs.
Each run starts a fresh terminal with a new `$HOME` and a new D-Bus session.

### Throughput: `cat` of a 32 MiB file

#### Xvfb, software rendering (fastcnsl: Vulkan on lavapipe)

| Terminal       | ascii: s, lower is better (MiB/s, higher is better) | color: s, lower is better (MiB/s, higher is better) | unicode: s, lower is better (MiB/s, higher is better) | lines: s, lower is better (MiB/s, higher is better) |
|----------------|----------------:|----------------:|----------------:|----------------:|
| **fastcnsl**   | **0.41** (77)   | **0.36** (90)   | **0.41** (79)   | **1.48** (22)   |
| GNOME Terminal | 0.53 (60)       | 0.61 (52)       | 0.87 (37)       | 2.07 (15)       |
| Ptyxis         | 0.72 (45)       | 0.75 (43)       | 1.19 (27)       | 3.08 (10)       |
| Konsole        | 1.04 (31)       | 0.79 (41)       | 0.97 (33)       | 1.90 (17)       |
| xterm          | 1.03 (31)       | 1.22 (26)       | 1.19 (27)       | 2.96 (11)       |

#### WSLg display, GPU (fastcnsl: OpenGL on D3D12, AMD Radeon 610M)

| Terminal       | ascii: s, lower is better (MiB/s, higher is better) | color: s, lower is better (MiB/s, higher is better) | unicode: s, lower is better (MiB/s, higher is better) | lines: s, lower is better (MiB/s, higher is better) |
|----------------|----------------:|----------------:|----------------:|----------------:|
| **fastcnsl**   | **0.37** (87)   | **0.33** (96)   | **0.37** (86)   | **1.48** (22)   |
| GNOME Terminal | 0.54 (59)       | 0.62 (52)       | 0.87 (37)       | 2.09 (15)       |
| Ptyxis         | 0.73 (44)       | 0.73 (44)       | 1.02 (31)       | 2.78 (12)       |
| Konsole        | 1.06 (30)       | 0.80 (40)       | 0.98 (33)       | 1.98 (16)       |
| xterm          | 1.03 (31)       | 1.32 (24)       | 1.25 (26)       | 3.13 (10)       |

The input files ([scripts/bench/gen.py](../scripts/bench/gen.py)):

- **ascii**: random printable text in 76-column lines.
- **color**: words in 256-colour SGR escapes.
- **unicode**: Cyrillic, Greek, box drawing and wide CJK characters.
- **lines**: `seq` output, 4.3 M short lines, which is almost pure scrolling.

### CPU and memory

Terminal CPU is the CPU time used by the terminal's processes while `cat`
ran, plus any time they kept working after it returned. Under Xvfb, X server CPU is the time Xvfb spent drawing
for the terminal. That time does not count as the terminal's own. Under WSLg the X server runs
outside the container, where it cannot be measured.

| Terminal       | Startup, ms (Xvfb / GPU), lower is better | Idle RSS, MB (Xvfb / GPU), lower is better | Peak RSS, MB (Xvfb / GPU), lower is better | CPU, s, ascii (Xvfb + X server / GPU), lower is better | CPU, s, lines (Xvfb + X server / GPU), lower is better |
|----------------|-------------:|-------------:|-------------:|-----------------:|-----------------:|
| fastcnsl       | 139¹ / 233¹  | 155 / 181    | 180 / 193    | 0.81 + 0.03 / 0.38 | 2.04 + 0.10 / 1.23 |
| GNOME Terminal | 160 / 145    | 40 / 40      | 42 / 42      | 0.34 + 0.03 / 0.37 | 1.10 + 0.06 / 1.21 |
| Ptyxis         | 315 / 404    | 216 / 315    | 219 / 327    | 1.01 + 0.03 / 0.46 | 3.35 + 0.10 / 1.48 |
| Konsole        | 183 / 196    | 74 / 74      | 87 / 88      | 1.03 + 0.01 / 1.05 | 1.92 + 0.01 / 1.97 |
| xterm          | 52 / 57      | 13 / 13      | 41 / 41      | 1.02 + 0.51 / 1.00 | 2.94 + 0.50 / 3.15 |

Startup is the time from launch until the window is mapped **and** the
command inside it is running. ¹ fastcnsl maps its window early and draws its
first frame later. Its log gives the first frame at a median of 186 ms (Xvfb) and 280 ms
(GPU). GPU setup takes about half of that.

### What it shows

- **fastcnsl has the highest throughput in every test.** On text it is
  1.3–2.3× faster than GNOME Terminal and 2.2–4× faster than
  Konsole and xterm. Its speed hardly changes between ASCII, colour and
  wide Unicode.
- **On a real GPU its CPU use halves.** It drops from 0.81 s to 0.38 s for 32 MiB. Under Xvfb,
  the CPU time includes llvmpipe rasterising the frames.
- **Many short lines (`lines`) are its weakest case.** It leads by only
  1.3× over Konsole and 1.4× over GNOME Terminal. It takes the same 1.48 s with or without a GPU, so
  the cost is per line on the CPU, not in drawing. Here GNOME Terminal uses
  about half the CPU (1.10 s against 2.04 s under Xvfb). That makes the per-line work the place to
  optimise: the gutter, code detection and scrollback rotation.
- **Memory is the cost.** fastcnsl uses 155–180 MB, against 40 MB for GNOME Terminal and 13–41 MB for xterm. Most of it
  is the graphics driver: Mesa's llvmpipe or d3d12, wgpu, plus the font and
  syntax tables. Ptyxis, the other GPU-drawn (GTK 4) terminal, uses even more.
- **xterm starts fastest (about 50 ms) and is the smallest.** It is also among the slowest
  at throughput, and half as much CPU again goes into the X server.
- **Unlimited scrollback costs memory, not speed.** Since 2026-11-11 a tab keeps
  all its output: its grid keeps the last 2 000 lines, and older lines go
  into a compact history on a thread of its own (compressed once it
  holds 32 MB), then to disk past `memory_per_tab_mb`. Measured right
  after the tables above, against the build before it: ascii 0.41 →
  0.41–0.43 s, unicode 0.41 → 0.41–0.43 s, color 0.39 → 0.37 s, lines
  1.52 → 1.49 s, within the run-to-run spread. Terminal CPU is 0.15–0.3 s
  higher for the thread that stores the history. Peak RSS for 32 MiB is
  about 20 MB higher (179 → 200 MB), because the output is kept. After
  smaller output a tab uses less than before (5 000 lines of a log:
  156 → 142 MB), since its grid is smaller.
- Every terminal uses DejaVu Sans Mono 11 pt, 10 000 lines of scrollback (fastcnsl: 2 000 in its grid, the rest in its unlimited history) and a 100×30 grid,
  which is fastcnsl's default 960×600 window. Under WSLg, fastcnsl's
  grid came out at 95×30. Everything else is left at each terminal's
  defaults, so fastcnsl's line-time gutter and code detection were on.
- The time is the wall time of `cat FILE` inside the terminal, measured with the
  monotonic clock ([scripts/bench/inner.sh](../scripts/bench/inner.sh)). The PTY
  buffer is small, so `cat` only finishes once the terminal has read and parsed
  almost everything. "Settle", the CPU work a terminal still had queued after
  `cat` returned, was at most 0.1 s for every terminal.
- Not measured: input latency, frame pacing, and how much of the output was
  actually drawn on screen. All of these terminals skip frames under load.
- WSL2 only. On native Linux with a real GPU driver (Vulkan), fastcnsl's
  startup and CPU figures should be lower. Run `make bench-host` there.

Raw per-run data: `dist/bench*/results.jsonl`.

## Windows

The Windows build of fastcnsl runs natively, without Docker, against
Windows Terminal and the classic console, conhost. Docker is only used to compile it.
[scripts/bench/windows.sh](../scripts/bench/windows.sh) is started from
WSL. It copies the build and the inputs to `%TEMP%\fastcnsl-bench` and runs
[scripts/bench/windows/bench.ps1](../scripts/bench/windows/bench.ps1) in
Windows PowerShell:

```sh
make bench-windows          # -> dist/bench-windows/
make bench-windows WIN_BENCH_ARGS="-Reps 5 -Terminals fastcnsl,wt -Tests startup,wsl-ascii"
```

Measured 2026-11-11 on Windows 11 Home 10.0.26300, with a Ryzen 9 7940HX, an RTX 4060 Laptop GPU and a
Radeon 610M. Versions: fastcnsl 0.1.86 (DirectX 12 on the Radeon 610M, with its bundled
ConPTY), Windows Terminal 1.24.12741 and PowerShell 5.1. Each value is the median of 5 runs.
All three terminals ran with their default settings, Cascadia Mono 12 in fastcnsl and Windows Terminal. Windows Terminal ran cold every time:
its own process, started for the run.

### Output from WSL: `cat` of a 32 MiB file inside `wsl.exe`

| Terminal         | ascii: s, lower is better (MiB/s, higher is better) | color: s, lower is better (MiB/s, higher is better) | unicode: s, lower is better (MiB/s, higher is better) | lines: s, lower is better (MiB/s, higher is better) |
|------------------|----------------:|----------------:|----------------:|----------------:|
| **fastcnsl**     | **0.32** (100)  | **0.26** (124)  | **0.27** (118)  | 1.53 (21)       |
| Windows Terminal | 0.37 (87)       | 0.90 (35)       | 0.71 (45)       | **1.50** (21)   |
| conhost          | 0.59 (55)       | 2.21 (14)       | 1.17 (27)       | 3.53 (9)        |

### Output from Windows programs: 4 MiB file

| Terminal         | cmd `type`, ascii: s, lower is better | cmd `type`, lines: s, lower is better | PowerShell `Get-Content`: s, lower is better |
|------------------|------------------:|------------------:|-------------------------:|
| fastcnsl         | 1.64              | 1.70              | 4.43                     |
| Windows Terminal | 1.55              | 1.60              | 4.14                     |
| conhost          | 1.52              | 1.72              | 4.23                     |

### Startup, CPU and memory

| Terminal         | Startup, ms, lower is better | Idle memory, MB, lower is better | CPU, s: WSL ascii / color / lines, lower is better | CPU, s: cmd `type` ascii, lower is better |
|------------------|------------:|----------------:|----------------------------------:|-------------------------:|
| fastcnsl         | 485¹        | 379             | 0.42 / 0.31 / 1.59                | 2.47                     |
| Windows Terminal | 278         | 138             | 0.44 / 1.34 / 2.25                | 1.03                     |
| conhost          | 85          | 16              | 0.42 / 2.02 / 3.31                | 1.00                     |

- **Startup** is the time from launch until a `cmd.exe` inside the window is running.
  ¹ fastcnsl's log puts its first frame at 471 ms, 354 ms of which is
  creating the DirectX 12 device.
- **Memory** is the working set of the terminal's processes, including the console host
  (OpenConsole.exe or conhost.exe). It also counts the shared DLL pages in
  each process, so it is higher than private memory.
- **CPU** is the CPU time of those processes while the command ran. WSL's own processes
  are not counted.

### What it shows

- **For WSL, fastcnsl is the fastest.** Plain ASCII is close (1.15× faster than
  Windows Terminal). With colours it is 3.5× faster, and with Unicode 2.6×. Its pipe relay skips the
  Windows console host, which Windows Terminal and conhost have to go
  through. It also uses the least CPU, 0.31 s against 1.34 s for Windows Terminal on the colour file.
  Many short lines are the exception: fastcnsl and Windows Terminal tie at 1.5 s there,
  the same per-line limit fastcnsl hits on Linux.
- **For cmd and PowerShell, all three take the same time.** It is about 2.5 MB/s with `type`
  and 1 MB/s with `Get-Content`. The console API (cmd and PowerShell
  writing to the console server) is the bottleneck, not the terminal. fastcnsl
  uses about 2.4× the CPU of the others for it (2.47 s against 1.0 s), counting its
  bundled OpenConsole.exe. That is worth a look: either the newer ConPTY sends more
  output, or fastcnsl spends more on parsing it.
- **Startup and memory cost the most.** fastcnsl's 485 ms is mostly DirectX 12
  setup. Its 379 MB working set, against 138 MB for Windows Terminal and 16 MB for conhost, comes
  mostly from the GPU driver. Since then fastcnsl asks wgpu for small
  allocation blocks (`MemoryHints::MemoryUsage`). That halved a window's private memory
  with DirectX 12, from 344 MB to 183 MB (195 MB working set), with the same output speed.

### Limits

- conhost uses its default 120×30 window. The others use 100×30.
- With Windows Terminal already open, a new window opens in that
  process. It then starts in about 140 ms and adds only about 35 MB. The harness never
  stops a Windows Terminal that was running before it started, and labels each run
  `wt: cold` or `wt: warm` in `results.jsonl`.
- Not measured: input latency, and the time the GPU and the desktop compositor (DWM) spend.
