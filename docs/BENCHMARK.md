# Benchmark: fastcnsl vs other terminals

- [Linux](#linux): Konsole, xterm, Ptyxis and GNOME Terminal on Ubuntu 26.04
- [Windows](#windows): Windows Terminal and the classic console (conhost)

Besides `cat` throughput, startup, CPU and memory, there are two tests of
everyday use:

- **100 windows**: the terminal is started 100 times in a row. Each window runs a
  command that exits at once, so the window closes again.
- **16 tabs**: one window with 16 tabs, all printing a 32 MiB file at the same
  time. xterm and conhost have no tabs, so they open 16 windows instead.

## Linux

All five terminals run in one Ubuntu 26.04 container
([docker/bench-ubuntu-26.04.Dockerfile](../docker/bench-ubuntu-26.04.Dockerfile)).
Ptyxis is Ubuntu's default terminal since 25.10. GNOME Terminal was the
default before that.

```sh
make bench          # Xvfb, software rendering      -> dist/bench/
make bench-host     # WSLg display + GPU (d3d12)    -> dist/bench-host/
make bench BENCH_ARGS="--reps 5 --terminals fastcnsl,xterm --tests ascii,windows100,tabs16"
```

Measured 2026-11-11 on an AMD Ryzen 9 7940HX (32 threads) under WSL2 (kernel
6.6.87). Versions: fastcnsl 0.1.122, Konsole 25.12.3, XTerm 407,
GNOME Terminal 3.58.0, Ptyxis 50.1 and Mesa 26.0.8. Each value is the median of 5 runs.
Each run starts a fresh terminal with a new `$HOME` and a new D-Bus session.

### Throughput: `cat` of a 32 MiB file

#### Xvfb, software rendering (fastcnsl: Vulkan on lavapipe)

| Terminal       | ascii: s, lower is better (MiB/s, higher is better) | color: s, lower is better (MiB/s, higher is better) | unicode: s, lower is better (MiB/s, higher is better) | lines: s, lower is better (MiB/s, higher is better) |
|----------------|----------------:|----------------:|----------------:|----------------:|
| **fastcnsl**   | **0.36** (88)   | **0.35** (91)   | **0.39** (81)   | **1.46** (22)   |
| GNOME Terminal | 0.52 (61)       | 0.63 (51)       | 0.85 (38)       | 2.06 (16)       |
| Ptyxis         | 0.70 (46)       | 0.75 (43)       | 1.22 (26)       | 3.22 (10)       |
| Konsole        | 1.03 (31)       | 0.79 (40)       | 0.94 (34)       | 1.91 (17)       |
| xterm          | 1.06 (30)       | 1.23 (26)       | 1.19 (27)       | 2.98 (11)       |

#### WSLg display, GPU (fastcnsl: OpenGL on D3D12, AMD Radeon 610M)

| Terminal       | ascii: s, lower is better (MiB/s, higher is better) | color: s, lower is better (MiB/s, higher is better) | unicode: s, lower is better (MiB/s, higher is better) | lines: s, lower is better (MiB/s, higher is better) |
|----------------|----------------:|----------------:|----------------:|----------------:|
| **fastcnsl**   | **0.36** (88)   | **0.33** (97)   | **0.38** (85)   | **1.46** (22)   |
| GNOME Terminal | 0.53 (60)       | 0.63 (51)       | 0.86 (37)       | 2.07 (15)       |
| Ptyxis         | 0.68 (47)       | 0.73 (44)       | 0.97 (33)       | 2.61 (12)       |
| Konsole        | 1.05 (31)       | 0.79 (40)       | 0.96 (33)       | 1.93 (17)       |
| xterm          | 1.01 (32)       | 1.29 (25)       | 1.22 (26)       | 3.18 (10)       |

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
| fastcnsl       | 137¹ / 231¹  | 134 / 180    | 182 / 228    | 0.85 + 0.02 / 0.49 | 2.39 + 0.11 / 1.57 |
| GNOME Terminal | 159 / 144    | 40 / 40      | 42 / 42      | 0.35 + 0.03 / 0.37 | 1.12 + 0.06 / 1.19 |
| Ptyxis         | 310 / 400    | 215 / 314    | 217 / 325    | 0.99 + 0.04 / 0.45 | 3.65 + 0.11 / 1.47 |
| Konsole        | 181 / 191    | 74 / 74      | 86 / 88      | 1.02 + 0.01 / 1.05 | 1.93 + 0.01 / 1.96 |
| xterm          | 57 / 57      | 13 / 13      | 40 / 41      | 1.05 + 0.55 / 1.00 | 2.97 + 0.49 / 3.16 |

Startup is the time from launch until the window is mapped **and** the
command inside it is running. ¹ fastcnsl draws its first frame before it
shows the window. Its log gives the first frame at a median of 105 ms (Xvfb) and 202 ms
(GPU) after launch. Setting up the GPU takes 64 ms and 147 ms of that.

### Opening and closing 100 windows

The terminal is started 100 times, one after another. Each window runs
`sh -c ': > FILE'`, which exits at once, so the window closes. The time per
window runs from launch until the terminal's process has exited. GNOME Terminal
is the exception: its launcher (`gnome-terminal --wait`) returns when the
command has exited, and the window lives on in `gnome-terminal-server`, which
keeps running between windows.

| Terminal       | Total, s (Xvfb / GPU), lower is better | Per window, ms (Xvfb / GPU), lower is better | First window, ms (Xvfb / GPU) | CPU, s (Xvfb + X server / GPU), lower is better |
|----------------|-------------:|-------------:|-------------:|-----------------:|
| fastcnsl       | 14.6 / 28.4  | 145 / 264    | 201 / 322    | 15.7 + 1.8 / 16.0 |
| GNOME Terminal | **3.5 / 4.1** | **34 / 39** | 161 / 145    | 3.6 + 0.5 / 3.8  |
| Ptyxis         | 44.1 / 91.0  | 434 / 894    | 803 / 1344   | 48.8 + 1.7 / 75.7 |
| Konsole        | 21.6 / 26.0  | 216 / 259    | 227 / 260    | 19.4 + 1.7 / 23.2 |
| xterm          | 6.0 / 4.7    | 60 / 47      | **44 / 48**  | **3.4 + 2.3 / 3.4** |

### Output in 16 tabs at once

One window with 16 tabs, and each tab runs `cat` of the 32 MiB ascii file,
512 MiB in all. The `cat`s start together. The time runs from the first `cat` starting to the last
one ending. "Open" is the time until the command in all 16 tabs is running. xterm has no tabs,
so it opens 16 windows. Konsole and GNOME Terminal open all their tabs in one call
(`--tabs-from-file`; `--window … --tab …`). fastcnsl and Ptyxis open the first
tab, then add one tab per call (`fastcnsl new-tab`, `ptyxis --tab`).

#### Xvfb, software rendering

| Terminal       | Open, s, lower is better | All done, s, lower is better (MiB/s) | Terminal CPU, s, lower is better | RSS with 16 tabs, MB: idle / peak, lower is better |
|----------------|------:|-------------------:|-----------------:|---------------:|
| **fastcnsl**   | 0.47  | **1.82** (281)     | 25.5             | 151 / 956      |
| GNOME Terminal | 0.34  | 7.88 (65)          | **5.5**          | **42 / 52**    |
| Ptyxis         | 2.35  | 8.75 (58)          | 7.9              | 221 / 233      |
| Konsole        | 0.39  | 15.80 (32)         | 15.6             | 85 / 289       |
| xterm (16 windows) | **0.19** | 5.29 (97)   | 18.4 + 5.2 X server | 208 / 644   |

#### WSLg display, GPU

| Terminal       | Open, s, lower is better | All done, s, lower is better (MiB/s) | Terminal CPU, s, lower is better | RSS with 16 tabs, MB: idle / peak, lower is better |
|----------------|------:|-------------------:|-----------------:|---------------:|
| **fastcnsl**   | 0.57  | **1.82** (281)     | 25.0             | 197 / 998      |
| GNOME Terminal | **0.35** | 8.01 (64)       | **5.6**          | **42 / 53**    |
| Ptyxis         | 4.79  | 8.69 (59)          | 5.9              | 313 / 325      |
| Konsole        | 0.43  | 15.51 (33)         | 15.5             | 86 / 289       |
| xterm (16 windows) | 0.92 | 10.49 (49)      | 18.1             | 209 / 644      |

### What it shows

- **fastcnsl has the highest throughput in every `cat` test.** On text it is
  1.4–2.3× faster than GNOME Terminal and 2.3–3.9× faster than
  Konsole and xterm. Its speed hardly changes between ASCII, colour and
  wide Unicode.
- **With 16 tabs it is 4× faster than the next terminal with tabs.** All 16 tabs finish in 1.8 s
  (281 MiB/s), against 7.9 s for GNOME Terminal and 15.8 s for Konsole. Each tab
  parses on its own thread, so the tabs run in parallel. The cost is CPU and memory. 25 s of CPU
  splits into 13 s in the 16 PTY reader threads (parsing) and 10 s in the 16 threads that store the
  history. That is 1.6 s per tab against 0.5–0.85 s for one tab alone: 32 busy threads
  on 16 cores share them through SMT. Each tab keeps all its output,
  so the window ends up holding about 1 GB for 512 MiB of text. GNOME Terminal
  needs a fifth of the CPU (5.5 s) and stays at 52 MB.
- **Opening and closing a window takes 145 ms (Xvfb) and 264 ms (GPU).**
  That is 3× faster than Ptyxis, 1.5× faster than Konsole under Xvfb and the same on the GPU.
  It is 2.4–7× slower than xterm (47–60 ms) and GNOME Terminal, whose server stays running
  between windows (34–39 ms after a first window of 145–161 ms). On the GPU, creating the
  D3D12 device takes 147 ms of fastcnsl's 264. This test found a crash: until this
  measurement, fastcnsl panicked whenever the program in its last tab exited, and the
  abort and core dump made each such close take about 2.3 s.
- **On a real GPU its CPU use falls by 40%,** from 0.85 s to 0.49 s for 32 MiB. Under Xvfb,
  the CPU time includes llvmpipe rasterising the frames.
- **Many short lines (`lines`) are its weakest case.** It leads by only
  1.3× over Konsole and 1.4× over GNOME Terminal. It takes the same 1.46 s with or without a GPU, so
  the cost is per line on the CPU, not in drawing. Here GNOME Terminal uses
  about half the CPU (1.12 s against 2.39 s under Xvfb). That makes the per-line work the place to
  optimise: the gutter, code detection and scrollback rotation.
- **Memory is the cost.** fastcnsl uses 134–180 MB idle, against 40 MB for GNOME Terminal and 13 MB for xterm. Most of it
  is the graphics driver: Mesa's llvmpipe or d3d12, wgpu, plus the font and
  syntax tables. Ptyxis, the other GPU-drawn (GTK 4) terminal, uses even more.
- **xterm starts fastest (about 57 ms) and is the smallest.** It is also among the slowest
  at throughput, and half as much CPU again goes into the X server.
- **Unlimited scrollback costs memory, not speed.** Since 2026-11-11 a tab keeps
  all its output: its grid keeps the last 2 000 lines, and older lines go
  into a compact history on a thread of its own (compressed once it
  holds 32 MB), then to disk past `memory_per_tab_mb`. Measured
  against the build before it: ascii 0.41 →
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
  With tabs, only the visible tab is drawn; xterm's 16 windows are all drawn.
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
make bench-windows WIN_BENCH_ARGS="-Reps 5 -Terminals fastcnsl,wt -Tests startup,wsl-ascii,windows100,tabs16"
```

Measured 2026-11-11 on Windows 11 Home 10.0.26300, with a Ryzen 9 7940HX, an RTX 4060 Laptop GPU and a
Radeon 610M. Versions: fastcnsl 0.1.124 (DirectX 12 on the Radeon 610M, with its bundled
ConPTY), Windows Terminal 1.24.12741 and PowerShell 5.1. Each value is the median of 5 runs.
All three terminals ran with their default settings, Cascadia Mono 12 in fastcnsl and Windows Terminal.
A Windows Terminal was already open this time, so every Windows Terminal window
opened in that running process ("warm"). Its startup, memory and CPU are what the window added to it.

### Output from WSL: `cat` of a 32 MiB file inside `wsl.exe`

| Terminal         | ascii: s, lower is better (MiB/s, higher is better) | color: s, lower is better (MiB/s, higher is better) | unicode: s, lower is better (MiB/s, higher is better) | lines: s, lower is better (MiB/s, higher is better) |
|------------------|----------------:|----------------:|----------------:|----------------:|
| **fastcnsl**     | **0.31** (105)  | **0.27** (119)  | **0.28** (115)  | 1.53 (21)       |
| Windows Terminal | 0.35 (91)       | 0.88 (36)       | 0.71 (45)       | **1.49** (21)   |
| conhost          | 0.56 (57)       | 2.16 (15)       | 1.23 (26)       | 3.48 (9)        |

### Output from Windows programs: 4 MiB file

| Terminal         | cmd `type`, ascii: s, lower is better | cmd `type`, lines: s, lower is better | PowerShell `Get-Content`: s, lower is better |
|------------------|------------------:|------------------:|-------------------------:|
| fastcnsl         | 1.67              | 1.73              | 4.54                     |
| Windows Terminal | 1.55              | 1.63              | 4.04                     |
| conhost          | 1.53              | 1.74              | 4.19                     |

### Startup, CPU and memory

| Terminal         | Startup, ms, lower is better | Idle memory, MB, lower is better | CPU, s: WSL ascii / color / lines, lower is better | CPU, s: cmd `type` ascii, lower is better |
|------------------|------------:|----------------:|----------------------------------:|-------------------------:|
| fastcnsl         | 454         | 107             | 0.47 / 0.41 / 1.84                | 2.39                     |
| Windows Terminal | 167 (warm)  | 36 (warm)       | 0.42 / 1.38 / 2.25                | 1.08                     |
| conhost          | 79          | 15              | 0.39 / 2.09 / 3.17                | 1.11                     |

- **Startup** is the time from launch until a `cmd.exe` inside the window is running.
  An earlier run put most of fastcnsl's startup in creating the DirectX 12 device.
- **Memory** is the working set of the terminal's processes, including the console host
  (OpenConsole.exe or conhost.exe). It also counts the shared DLL pages in
  each process, so it is higher than private memory.
- **CPU** is the CPU time of those processes while the command ran. WSL's own processes
  are not counted.

### Opening and closing 100 windows

The terminal is started 100 times in a row. Each window runs `cmd.exe /d /c type nul > FILE`,
which exits at once. The time per window runs from launch until the window is gone: the
fastcnsl or conhost process has exited, or, for Windows Terminal, its window has closed
(`wt.exe` only hands the window to the running `WindowsTerminal.exe`).

| Terminal         | Total, s, lower is better | Per window, ms, lower is better | First window, ms |
|------------------|---------:|-----------:|---------:|
| fastcnsl         | 53.3     | 526        | 529      |
| Windows Terminal (warm) | 16.8 | 160     | 149      |
| conhost          | **8.9**  | **79**     | 67       |

### Output in 16 tabs at once: `cat` inside `wsl.exe`

16 tabs (conhost: 16 windows), each running `wsl.exe -e cat` of the 32 MiB ascii file, 512 MiB in all.
The time runs from the first `cat` starting to the last one ending. Windows Terminal opens all 16
tabs in one call (`wt … ; new-tab …`). fastcnsl opens the first tab, then adds one tab per
`fastcnsl new-tab` call.

| Terminal         | Open, s, lower is better | All done, s, lower is better (MiB/s) | CPU, s, lower is better | Memory with 16 tabs, MB: idle / after, lower is better |
|------------------|------:|-------------------:|--------:|---------------:|
| **fastcnsl**     | 1.93  | **2.35** (218)     | 23.6    | **117** / 920  |
| Windows Terminal (warm) | 2.04 | 5.62 (91)   | 46.3    | 558 / 651      |
| conhost (16 windows) | **0.60** | 3.49 (147) | **18.7** | 216 / **302** |

These two tests vary more from run to run than the others. In an earlier full run
the same day, Windows Terminal took 4.27 s and conhost 2.66 s for 16 tabs, and fastcnsl 2.25 s.

### What it shows

- **For WSL, fastcnsl is the fastest.** Plain ASCII is close (1.13× faster than
  Windows Terminal). With colours it is 3.3× faster, and with Unicode 2.5×. Its pipe relay skips the
  Windows console host, which Windows Terminal and conhost have to go
  through. It also uses the least CPU, 0.41 s against 1.38 s for Windows Terminal on the colour file.
  Many short lines are the exception: fastcnsl and Windows Terminal tie at 1.5 s there,
  the same per-line limit fastcnsl hits on Linux.
- **With 16 WSL tabs fastcnsl is 2.4× faster than Windows Terminal** (2.35 s against 5.62 s)
  and uses half its CPU. Like on Linux, it keeps all 512 MiB of output, so the window
  grows to about 920 MB.
- **For cmd and PowerShell, all three take the same time.** It is about 2.5 MB/s with `type`
  and 1 MB/s with `Get-Content`. The console API (cmd and PowerShell
  writing to the console server) is the bottleneck, not the terminal. fastcnsl
  uses about 2.2× the CPU of the others for it (2.39 s against 1.1 s), counting its
  bundled OpenConsole.exe. That is worth a look: either the newer ConPTY sends more
  output, or fastcnsl spends more on parsing it.
- **Opening a window is the slowest part.** fastcnsl needs 454 ms to start and 526 ms per window to open and close,
  against 79 ms for conhost and 160 ms for a Windows Terminal that is already running.
  100 windows take 53 s against 9 s and 17 s. DirectX 12 setup is most of it.
- **Memory is now close to Windows Terminal's.** A fastcnsl window uses 107 MB (379 MB in the
  previous measurement), since it asks wgpu for small allocation blocks
  (`MemoryHints::MemoryUsage`). A warm Windows Terminal window adds 36 MB; a cold
  one used 138 MB in the previous measurement.

### Limits

- conhost uses its default 120×30 window. The others use 100×30.
- With Windows Terminal already open, a new window opens in that
  process. It then starts in about 150 ms and adds only about 35 MB. The harness never
  stops a Windows Terminal that was running before it started, and labels each run
  `wt: cold` or `wt: warm` in `results.jsonl`.
- The first launch of a freshly copied `fastcnsl.exe` can take several seconds (presumably a
  virus scan). The `startup` test runs first and absorbs it.
- Not measured: input latency, and the time the GPU and the desktop compositor (DWM) spend.
