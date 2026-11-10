# omlx-top

`omlx-top` is a read-only terminal monitor for oMLX on macOS. It puts what the
oMLX server reports about itself over HTTP next to what the operating system
reports about the same processes, the GPU and unified memory, on one screen that
refreshes every two seconds. It never loads, unloads, restarts or configures
anything.

A 100×30 terminal, with a model loaded:

```text
omlx-top  ·  service:8008  ·  healthy  v0.7.3  up 19h14m  12ms  every 2s
CPU       server 3.5%   app —                     GPU DEVICE [░░░░░░░░░░░░░░░░░░░░░░░░] 0%
RAM       footprint 7.3G (23%)                    GPU       renderer 0  tiler 0
MODEL     6.6G / 13.0G ceiling [████████░░░░░░░░] VRAM      in use 1.5G  alloc 13.1G
SYS       used 26.2G  wired 4.5G                            comp 12.7G  swap 8.8G/9.5G
          cached 4.5G  free 422.8M                SYS CPU   44%  PRESSURE warning (2)
history — last 120 samples
cpu ▁▁                                                                                      4.3% avg
gpu ▁▁▁                                                                                       2% avg
mem ███                                                                                    6.6G used
serving
5 requests   0 active   0 waiting   prefill 507 tok/s (session avg)   gen 20.5 tok/s
prompt 173,324   completion 654   cached 0   cache eff 0%
┌models — 1 loaded / 19 discovered — a for all─────────────────────────────────────────────────────┐
│  model                                                  type       context   size        state   │
│▸ gemma-4-12B-it-4bit                                    vlm        262k      6.9G        loaded  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
└──────────────────────────────────────────────────────────────────────────────────────────────────┘
q quit  space pause  +/- int  a all  i inst  m models  r rescan  ? help
```

`app` reads `—` because this instance is the Homebrew service; there is no GUI
process to measure.

## Requirements

- macOS 13 or later on Apple Silicon. The crate refuses to compile on any other
  target (`compile_error!` in `src/main.rs`); the code paths beyond libproc and
  IOKit are untested elsewhere.
- A stable Rust toolchain to build it. CI uses `dtolnay/rust-toolchain@stable`.
- oMLX 0.7.3, the version this monitor was written and measured against, running
  as the macOS app bundle, as the Homebrew service
  (`brew services start omlx`), or manually with `omlx serve`.
- No root. A server started with `sudo brew services` is only partly observable
  (see [Limitations](#limitations)).

## Build and install

```sh
cargo build --release
./target/release/omlx-top
```

From crates.io, or from a checkout:

```sh
cargo install omlx-top
cargo install --path .
```

The binary is `target/release/omlx-top`, or `~/.cargo/bin/omlx-top` after
`cargo install`. There is no Homebrew formula yet: either run it from the build
directory or copy it somewhere on `PATH`
(`install -m 755 target/release/omlx-top ~/.local/bin/`).

## Usage

```
omlx-top [OPTIONS]

  --url <URL>          base URL; by default discovered from the running process
                       (listening socket), else http://127.0.0.1:8000
  --api-key <KEY>      Bearer token; env OMLX_API_KEY
  --interval <DUR>     sampling period, e.g. 2s, 500ms (default 2s; clamped to
                       500ms–60s)
  --warmup <DUR>       measurement window used by --once/--json to obtain a CPU
                       delta (default 250ms; 0 reports null CPU)
  --once               print one snapshot as text, exit 0/1
  --json               print one snapshot as JSON, exit 0/1 (implies --once)
  --list               print discovered instances (mode, pid, port) and exit
  --no-color           disable styling
  -h, --help
  -V, --version
```

Bare invocation starts the TUI. `--once` prints the same figures as text,
`--json` prints one JSON object (`--json | python3 -m json.tool` validates it),
and `--list` prints the discovered instances without starting the UI:

```text
MODE     PID    GUI    PORT   URL                    SELECTED
service  7212   -      8008   http://127.0.0.1:8008  *
```

### Keys

| Key | Action |
|---|---|
| `q`, `Esc` | quit |
| `space` | pause / resume sampling (the frozen snapshot stays on screen) |
| `+` / `-` | sampling interval ±1 s, clamped to 1–60 s |
| `a` | toggle the models table: loaded only ↔ all discovered |
| `i` | cycle the target instance when several are listening |
| `m` | force a `/v1/models/status` refresh |
| `↑` / `↓`, `PgUp` / `PgDn` | scroll the models table |
| `?` | help overlay |
| `r` | re-discover instances and refresh |

No key writes anything.

### Exit codes

Exit codes make the binary usable as a health probe:

| Code | Meaning |
|---|---|
| `0` | `--once`/`--json`: the server answered `200` with `status: "ok"`. `--list`: at least one instance was discovered and the `--url` port (if given) matched one. TUI: normal exit. |
| `1` | `--once`/`--json`: the server is unreachable or degraded. `--list`: nothing discovered, or `--url` names a port no instance listens on (rows are printed first, then `warning: no instance on port <port>` on stderr). |
| `2` | usage error: malformed `--interval`, `--warmup` or `--url`. |

Colour is dropped by `--no-color`, by `NO_COLOR` being present in the
environment (any value, including empty), or by `TERM=xterm-mono`. Nothing is
encoded by colour alone: every gauge shows its number and every state shows its
word, so a monochrome capture stays readable.

## Data sources

### HTTP endpoints used

The client (`ureq`, built without TLS, gzip or cookies) only speaks plain HTTP
to loopback. Requests time out after 1 s, with a 500 ms connect timeout. HTTP
status codes are not errors: a 503 and a 401 carry information.

| Endpoint | Cadence | Why |
|---|---|---|
| `GET /api/status` | every tick (2 s) | primary source: version, uptime, serving counters, model memory, model counts. 822 B, ~3 ms of server time |
| `GET /health` | only when `/api/status` fails or answers 503 | liveness, and it distinguishes "loading" (503 + `{"status":"loading"}`) from "down" |
| `GET /v1/models/status` | every ~30 s, on `m`, and on a loaded-count mismatch | per-model rows for the table. 15.3 KB, ~19 ms |

`/v1/models/status` returns every discovered engine, including `MarkItDown`, the
built-in document converter (`model_path: "builtin://markitdown"`). It is not a
model, so it is filtered out of every count and out of the table.

### Endpoints deliberately not used

The three `/admin/api/*` endpoints (`stats`, `activity`, `usage`) are **not
called at all**. They need an admin session or a Bearer key, so they degrade
where `/api/status` does not; their guard thresholds are redundant with the
kernel pressure level and the model ceiling; and requiring them would turn an
optional panel into a hard dependency.

### OS APIs

Everything is read through syscalls and framework calls. The binary never spawns
a child process (`ioreg` alone would cost ~26 ms of CPU per call).

| Metric | Source |
|---|---|
| Process identity and ancestry | `proc_listallpids`, `proc_name`, `proc_pidpath`, `proc_pidinfo(PROC_PIDTBSDINFO)`, `sysctl(KERN_PROCARGS2)` |
| Per-process CPU time and memory | `proc_pid_rusage(RUSAGE_INFO_V0)` → `ri_user_time`, `ri_system_time`, `ri_phys_footprint` |
| Listening port | `proc_pidinfo(PROC_PIDLISTFDS)` + `proc_pidfdinfo(PROC_PIDFDSOCKETINFO)` |
| GPU | IOKit `IOAccelerator` service property `PerformanceStatistics` (CoreFoundation FFI) |
| VM pages | `host_statistics64(HOST_VM_INFO64)` |
| System CPU | `host_statistics(HOST_CPU_LOAD_INFO)` |
| Swap | `sysctl(vm.swapusage)` |
| Memory pressure | `sysctl(kern.memorystatus_vm_pressure_level)` |
| Page size, total RAM, chip | `sysctl(hw.pagesize)`, `sysctl(hw.memsize)`, `sysctl(machdep.cpu.brand_string)`, read once |

## Metric definitions

"Memory used" is a matter of opinion unless the formula is written down, so here
it is.

### CPU

Per process (`omlx-server`, and the `oMLX` GUI in app mode), over the sampling
window:

```
Δticks = (ri_user_time + ri_system_time)[t] - [...][t-1]     // mach absolute time
Δns    = Δticks * timebase_numer / timebase_denom
cpu_%  = Δns / Δwall_ns * 100                                 // 100 % = one full core
```

The first tick has no baseline and prints `—`; a number is never invented from a
single reading. In the TUI the window is the sampling interval. `--once` and
`--json` take two readings separated by `--warmup` (250 ms by default), which is
noisier than a 2 s average — `--warmup 0` reports `null` CPU instead.

System CPU is `Δ(user + system + nice) / Δ(user + system + nice + idle) * 100`,
same first-tick rule.

### Process memory

`ri_phys_footprint`, the figure Activity Monitor calls "Memory". It includes
Metal buffers mapped by the process, so it is **never added** to the GPU memory
rows: on unified memory the same pages are counted by both.

### System memory (Activity Monitor model)

In pages × `hw.pagesize`:

```
app_memory   = (internal_page_count - purgeable_count) * page_size
wired        = wire_count * page_size
compressed   = compressor_page_count * page_size
used         = app_memory + wired + compressed
cached       = external_page_count * page_size       // file-backed
free         = (free_count + speculative_count) * page_size
uncompressed = total_uncompressed_pages_in_compressor * page_size
```

`uncompressed` is the logical size of what the compressor holds, not memory
consumed; it is shown as context and never added to `used`. The gauge is
`used / hw.memsize`.

### GPU

`Device Utilization %` is the primary gauge; `Renderer Utilization %` and
`Tiler Utilization %` are secondary. These are **whole-device** figures sampled
by the AGX driver: WindowServer and every other process are included, and an
idle 15–19 % is normal on a machine with a compositor. The labels therefore say
`GPU DEVICE` and `VRAM in use`, never "oMLX GPU". `In use system memory` and
`Alloc system memory` are the two VRAM rows.

### Model memory

`model_memory_used / model_memory_max` from `/api/status`, refreshed every tick.
The denominator is the server's working-set **ceiling**, not total RAM, and it
is not a constant: it moved between 10.4 GB and 11.4 GB within minutes on the
reference machine, tracking system pressure. Compare the gauge with the current
ceiling shown next to it, not with an older reading.

### Memory pressure

Two independent signals are displayed and never converted into each other:

1. the raw kernel level from `kern.memorystatus_vm_pressure_level` — `1 normal`,
   `2 warning`, `4 critical` — with the integer shown so an undocumented value
   is visible rather than silently reinterpreted;
2. `used / total`, with thresholds at 70 % and 85 %.

The alert state, used for colour and for the header banner:

| Condition | State |
|---|---|
| kernel level 4 | `critical` |
| kernel level 2 | `warning` |
| kernel level 1 | `normal` |
| kernel level unavailable, ratio ≥ 0.85 | `critical` |
| kernel level unavailable, ratio ≥ 0.70 | `warning` |
| otherwise | `unknown` |

The kernel level wins whenever it is present: if the kernel says `1` and the
ratio says 90 %, the state is `normal` and the ratio is shown as context. When
the kernel level is missing, the ratio drives the state and the row shows the
ratio instead of a level number.

`critical` colours the pressure row red and bold and adds
`⚠ PRESSURE CRITICAL` to the header. `warning` colours the row only: a banner
for a state that can last hours becomes wallpaper. Nothing blinks or animates;
the state is derived from the current snapshot, so there is no extra redraw.

## Deployment discovery

Discovery runs at startup, every 30 s, and immediately after a failure. It
answers three questions: which oMLX processes exist, how each was started, and
which one serves the URL being measured.

**Which processes.** A process is a candidate when any of these holds: its
`proc_name` is `omlx-server`; its executable path contains
`/oMLX.app/Contents/Resources/Python/`; it contains `/Cellar/omlx/` or
`/opt/omlx/`; or it looks like a Python interpreter and its `argv[0]` basename is
`omlx` or `omlx-server`. The last rule is what catches a Homebrew **framework**
Python build, whose `proc_pidpath` contains no `omlx` segment at all:

```text
/opt/homebrew/Cellar/python@3.11/3.11.18/Frameworks/Python.framework/
  Versions/3.11/Resources/Python.app/Contents/MacOS/Python
```

`p_comm` and `proc_name` return that interpreter name (`Python`, or
`python3.11` for the app bundle), never `omlx-server`, so process name alone
cannot identify a deployment. Environment variables are never used to accept a
candidate: they are inherited by unrelated children of a shell that exported
them.

**How it was started.** For each candidate the parent chain is walked (at most
4 hops): an ancestor ending in `/oMLX.app/Contents/MacOS/oMLX` means `app` (and
that ancestor is sampled too); `ppid == 1` means `service`; any other parent
means `manual`; unreadable ancestry means `unknown`.

**Which port.** The listening socket is authoritative, because the three layers
that claim to know the port disagree:

| Layer | Value |
|---|---|
| `settings.py` default | 8000 |
| `~/.omlx/settings.json` → `server.port` | what the app wrote (8008 on the reference machine) |
| `OMLX_PORT` environment variable | overrides the file |

The resolution order is: `--url`, then the candidate's own listening TCP socket
(decoded from `PROC_PIDLISTFDS` / `PROC_PIDFDSOCKETINFO`), then `OMLX_PORT` from
its environment, then `settings.json`, then 8000 and 8008 each probed once with
`/api/status`.

**Several instances.** Two deployments can run at once (app on 8008, service on
8000). They are never merged: one is targeted at a time, the header shows
`instance 1/2`, and `i` cycles the target and rebuilds the HTTP client against
the new port. Metrics are always scoped to the selected instance — its server
pid, plus its GUI parent in app mode — so the GUI of one instance is never
paired with the server of another.

`KeepAlive` restarts pids: every cached pid is revalidated on each tick, so a
restart is picked up within one tick and the uptime and counters visibly reset.

## Performance

The default interval is 2 s, not 1 s, because monitoring perturbs what it
measures: each `/api/status` call costs about 3 ms of CPU inside the Python
process being measured, roughly 0.15 % of a core at 2 s — the same order as the
idle server itself. The cost is documented, not subtracted; subtracting it would
be guessing.

Measured idle cost on the reference machine (M1 Max, 32 GB, macOS 26.5), two
independent five-minute runs at 100×40 with the default 2 s interval:
0.06–0.14 % of one core and 8.0–8.8 MB of RSS. The first run averaged 0.06 %
(`top -l 61 -s 5`; 0.14 s of CPU over a 120 s window) with `ps -o rss` between
8.24 and 8.29 MB; the second averaged 0.135 % with 8.6 MB. The budget is
< 1.0 % CPU and < 15 MB RSS, with zero child processes spawned. When the budget
is missed the fixes, in order, are: raise the default interval to 3 s, shrink
the history ring from 120 to 60 samples, then trim the remaining framework
calls.

## Limitations

- **GPU is device-wide.** WindowServer traffic is included; there is no
  per-process GPU figure in v1 (it would need `powermetrics` and root).
- **Polling inflates the measured CPU** by ~0.15 % of a core at 2 s.
- **No energy, power or frequency metrics.** IOReport and `powermetrics` are out
  of scope.
- **No persistence.** History is a 120-sample in-memory ring; there is no CSV,
  SQLite or history across restarts.
- **Loopback plain HTTP only.** `ureq` is built without TLS, so a remote HTTPS
  server is out of scope.
- **Read-only.** Nothing is loaded, unloaded or restarted, by design.
- **A root-run service is partly unobservable.** Without root, `rusage` and file
  descriptors of another user's process are unreadable; HTTP panels keep
  working and the process panels say so instead of showing zeros.
- **`PerformanceStatistics` and the pressure sysctl are undocumented.** Both have
  been stable for years, but a future macOS could change them. Failure is
  contained: the GPU panel shows `n/a`, and the pressure row shows the raw
  integer next to its label.

## Development

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

Tests use captured payloads in `tests/golden/` and synthetic process tables;
none needs a network or a running oMLX. `cargo build --release` must stay
warning-free as well.

## Releasing

```sh
scripts/release.sh 0.2.0 --push
```

The script bumps `Cargo.toml`, refreshes `Cargo.lock`, runs `fmt`, `clippy`, the
test suite and `cargo publish --dry-run`, commits, and creates the annotated
`v0.2.0` tag. Pushing the tag makes the CI build the Apple Silicon archive,
publish the crate to crates.io, create the GitHub release, upload `SHA256SUMS`,
and attach SLSA provenance. Release notes are taken from
`docs/release-notes/v<version>.md` when that file exists; otherwise GitHub
generates them from the commit history.

Crates.io publication uses [trusted publishing](https://docs.crates.io/crates-io/trusted-publishing):
no token is stored in repository secrets. It requires a one-time configuration
on crates.io — crate Settings → Trusted Publishing → GitHub publisher for
`athoune/omlx-top`, workflow `rust.yml`. Without it the release job fails before
creating the GitHub release, so nothing is published halfway.

## License

BSD 3-Clause. See [LICENSE](LICENSE).
