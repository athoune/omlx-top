# omlx-top — Specification

A minimal-cost macOS TUI that reports oMLX resource usage: what the server says
about itself over HTTP, cross-referenced with what the operating system reports
about its processes, the GPU, and unified memory.

Status: **draft, v1 scope**. Implementation has not started.

Platform: macOS 13+ on Apple Silicon (reference: M1 Max, macOS 26.5). The binary
is read-only: it never loads, unloads, or restarts anything.

---

## 1. Problem

oMLX runs in two supported shapes: the macOS app bundle (which spawns a local
server on a loopback port) and a Homebrew-installed launchd service (`brew
services start omlx`). Either way, the server (`omlx-server`, a Python process)
exposes serving counters over HTTP but nothing about host resource consumption.
Understanding "what is oMLX doing to my machine" today requires a browser tab,
Activity Monitor, and `ioreg`, three tools that report three different things at
three different rates.

`omlx-top` answers one question, continuously and honestly: **what CPU, GPU and
unified memory is oMLX using right now, and what is the server doing with it?**

## 2. Ground truth (measured, not assumed)

Measured on 2026-11-09, Apple M1 Max, 32 GB unified memory, 10 cores, macOS
26.5 (Darwin 25), oMLX 0.7.3, server on `127.0.0.1:8008`.

### 2.1 Deployment modes

oMLX runs in three shapes on the same machine, and **all of them produce a
process whose `p_comm` is `omlx-server`**. Process name alone therefore cannot
identify the deployment; ancestry, executable path, and listening sockets are
required.

**A. App bundle** (observed running today):

```
/Applications/oMLX.app/Contents/MacOS/oMLX              comm "oMLX"
└─ omlx-server                                          comm "omlx-server"
   exe: /Applications/oMLX.app/Contents/Resources/Python/cpython-3.11/bin/python3.11
```

**B. Homebrew service under launchd** (installed here, currently stopped):

```
launchd (pid 1)
└─ omlx-server
   exe:  /opt/homebrew/Cellar/python@3.11/3.11.18/Frameworks/Python.framework/
         Versions/3.11/Resources/Python.app/Contents/MacOS/Python
   argv: /opt/homebrew/opt/omlx/bin/omlx serve       (rewritten, see below)
   env:  OMLX_SUPERVISED=launchd
   plist: /opt/homebrew/opt/omlx/sh.brew.omlx.plist
          Label=sh.brew.omlx, RunAtLoad=true, KeepAlive=true
          StandardOut/ErrPath=/opt/homebrew/var/log/omlx.log
          WorkingDirectory=/opt/homebrew/var
```

The Homebrew venv is a **framework** Python: the shebang in
`/opt/homebrew/opt/omlx/bin/omlx` points at
`.../Cellar/omlx/<version>/libexec/bin/python3.11`, but the running executable is
the framework's `Python.app` binary. The resulting `proc_pidpath` therefore
contains **no `omlx` segment at all** — only `Cellar/python@3.11/...`. The app
bundle uses a plain venv Python instead
(`/Applications/oMLX.app/Contents/Resources/Python/cpython-3.11/bin/python3.11`).
Both layouts must be recognised.

**C. Manual** — `omlx serve` from a shell: child of the user's shell, same
`omlx-server` title.

Facts that drive the design:

- The server rewrites its own **argv** (`omlx/process_title.py` → `setproctitle`),
  but **`p_comm` stays the interpreter name**, and that name differs per
  distribution: `python3.11` for the app bundle, `Python` (capital, framework
  build) for the Homebrew service. Measured: `proc_name`,
  `PROC_PIDTBSDINFO.pbi_comm` and `sysctl KERN_PROC_PID` all return the
  interpreter name; `KERN_PROCARGS2` returns `argv[0] == "omlx-server"` and
  zeroed `argv[1..]`. `ps -o comm` displays `omlx-server` because macOS `ps`
  derives that column from argv — it is not evidence about `p_comm`. Candidate
  detection must therefore not rely on `p_comm` alone (§6.3.1).
- The Homebrew Cellar path embeds the version (`.../omlx/0.7.3/...`) and moves
  on `brew upgrade`. **Never hardcode it.**
- The plist exists on disk even when the service is not loaded (`brew services
  info` reports `Loaded: ✘` while `File: ... ✔`). The file proves nothing about
  runtime state; only the process table does.
- `KeepAlive=true` restarts the process after a crash, so pids churn and any
  cached pid must be revalidated every tick.
- A `sudo brew services` install would run the server as root; a non-root
  monitor cannot read its `rusage` or file descriptors (EPERM, see §9).

### 2.2 The listening port is not in the config file

Port resolution has three layers, and they disagree by default:

| Layer | Value |
|---|---|
| `settings.py` default | `8000` |
| `~/.omlx/settings.json` → `server.port` | `8008` here (written by the app) |
| `OMLX_PORT` environment variable | overrides the file (`settings.py:_apply_env_overrides`) |

The Homebrew README documents the service as running "with zero-config defaults
… port 8000". Reading `settings.json` is therefore **not** a reliable way to find
the service's port, and the two deployments can legitimately listen on different
ports simultaneously. The authoritative source is the socket the process is
actually listening on (§6.3).

### 2.3 HTTP endpoints

| Endpoint | Size | Latency | Auth | Cost | Use |
|---|---|---|---|---|---|
| `GET /api/status` | 822 B | ~3 ms | none when bound to loopback | low | primary source, every tick |
| `GET /health` | 178 B | ~2.7 ms | none | low | liveness / "loading" discriminator |
| `GET /v1/models/status` | 15.3 KB | ~19 ms | none | medium | per-model table, every ~30 s |
| `GET /admin/api/stats` | ~2 KB | ~5 ms | admin session / Bearer key | low | *not used in v1*: its guard thresholds are redundant with the kernel pressure level and the model ceiling |
| `GET /admin/api/activity` | ~400 B | ~3 ms | admin session / Bearer key | low | *not used in v1* (same auth constraints as `/api/stats`) |
| `GET /admin/api/usage` | ~1 KB | ~3 ms | admin session / Bearer key | low | *not used in v1* (hourly history) |

Notes:

- The `/api/status` docstring states its purpose: *"Lightweight status endpoint
  for external tool polling (statuslines, scripts)"*. It is the right contract.
- `verify_api_key` accepts a missing key only when the bind address is
  loopback. Non-loopback binds require a Bearer token → see `--api-key`.
- `/admin/*` works locally here only because `skip_api_key_verification` is
  enabled **and** the bind is loopback. It must be treated as optional and
  degradable, never as required.
- `model_memory_max` (the MLX working-set ceiling) is **recomputed on every
  call** and moved between 10.4 GB and 11.4 GB within minutes on the same
  machine, tracking system pressure. It is not a constant.
- `GET /v1/models/status` returns **all** discovered models (20 here), not just
  loaded ones. One of them is not a model at all: `MarkItDown` is the built-in
  document converter (`model_path: "builtin://markitdown"`,
  `engine_type: "markitdown"`, size 0). The app's own counters exclude it —
  `/api/status.models_discovered` reports 19 while `model_count` reports 20.
  Treating it as a model would make "1 loaded" coexist with "0 B of model
  memory". Built-ins are therefore filtered out of every `models.*` counter and
  out of the TUI table (§8.1).

Full `/api/status` payload (as of 0.7.3):

```json
{
  "status": "ok",
  "version": "0.7.3",
  "uptime_seconds": 72645.9,
  "models_discovered": 19,
  "models_loaded": 0,
  "models_loading": 0,
  "default_model": "gemma-4-e4b-it-OptiQ-4bit",
  "loaded_models": [],
  "total_requests": 3,
  "active_requests": 0,
  "waiting_requests": 0,
  "total_prompt_tokens": 506739,
  "total_completion_tokens": 0,
  "total_cached_tokens": 0,
  "cache_efficiency": 0.0,
  "avg_prefill_tps": 15605.1,
  "avg_generation_tps": 0.0,
  "model_memory_used": 0,
  "model_memory_max": 11374576480,
  "model_memory_used_formatted": "0B",
  "model_memory_max_formatted": "10.59GB",
  "custom_kernels": { "attention": { "available": true, "import_error": null }, "...": {} },
  "ane_prefill": { "patch_available": true, "configured_models": 0, "models": [] }
}
```

`/health` additionally exposes `engine_pool.{model_count, loaded_count,
final_ceiling, current_model_memory}` and an `mcp` block. It answers **503**
with `{"status": "loading"}` while the startup preload runs: the port is bound
but the server is not ready.

Per-model entry in `/v1/models/status` (fields we consume marked ▸):

```
▸ id, ▸ loaded, ▸ is_loading, ▸ estimated_size, ▸ resident_estimated_size,
▸ actual_size, ▸ engine_type, ▸ model_type, ▸ model_context_length,
▸ max_context_window, ▸ pinned, ▸ is_helper, ▸ is_hidden, ▸ distributed,
▸ external_engine, ▸ last_access, ▸ model_path,
  model_format, source_type, source_repo_id, source_path, cluster,
  config_model_type, thinking_default, preserve_thinking_default,
  reasoning_efforts, max_tokens, is_favorite, realtime_stt, realtime_voice,
  loading_started_at
```

There is **no per-model memory figure**. Only a global `current_model_memory`.
The table therefore shows *estimated* sizes and marks which models are actually
resident; it must not invent a per-model byte count.

### 2.4 OS sources

| Metric | Source | Cost |
|---|---|---|
| Per-process CPU time | `proc_pid_rusage(pid, RUSAGE_INFO_V0)` → `ri_user_time`, `ri_system_time` (mach absolute time) | µs |
| Per-process memory | same call → `ri_phys_footprint` (Activity Monitor's "Memory") | µs |
| Process identity | `proc_listallpids`, `proc_pidpath`, `proc_name` | µs per pid |
| GPU | IOKit service `IOAccelerator` → property `PerformanceStatistics` (CFDictionary): `Device Utilization %`, `Renderer Utilization %`, `Tiler Utilization %`, `In use system memory`, `Alloc system memory` | µs |
| VM state | `host_statistics64(HOST_VM_INFO64)` → page counts | µs |
| System CPU | `host_statistics(HOST_CPU_LOAD_INFO)` → `cpu_ticks[CPU_STATE_{USER,SYSTEM,IDLE,NICE}]` | µs |
| Swap | `sysctlbyname("vm.swapusage")` → `xsw_usage` | µs |
| Memory pressure | `sysctlbyname("kern.memorystatus_vm_pressure_level")` → 1 normal, 2 warning, 4 critical | µs |
| Page size | `sysctlbyname("hw.pagesize")` → 16384 on Apple Silicon | once at startup |
| Total RAM | `sysctlbyname("hw.memsize")` | once |
| Chip | `sysctlbyname("machdep.cpu.brand_string")` | once |
| Thermal state | *not used in v1* (`NSProcessInfo.thermalState` would need objc2) | — |

**Process identity caveat.** `proc_name` returns the *kernel* `p_comm`, which is
the interpreter name for a Python server (`python3.11`), not the rewritten
`argv[0]` (`omlx-server`). See §2.1 and §6.3.1 before building any signature on
it.

**Mach port-right caveat.** `mach_host_self()` returns a send right and
increments its user reference count on every call. Calling it once per sample
leaks a port right every tick. Resolve it once into a `OnceLock<u32>` and reuse
it; same for `mach_task_self()` if it is ever needed.

**Do not shell out.** `ioreg -c IOAccelerator` costs ~26 ms of CPU per
invocation (measured over 5 runs: 0.13 s wall, 0.03 s user+sys). At 1 Hz that is
2.6 % of a core, which violates the whole point of this tool. Direct IOKit FFI
is microseconds and avoids spawning a child process.

## 3. Goals and non-goals

### Goals

1. One screen that shows CPU, GPU, memory and serving counters for oMLX.
2. **Zero-configuration deployment discovery**: app bundle, Homebrew/launchd
   service, and manual `omlx serve` are all detected, classified, and reported
   without the user telling us which one is running or on which port.
3. Honest attribution: every number is either "oMLX" or explicitly labelled
   "device"/"system".
4. Steady-state cost under **1 % CPU** and **15 MB RSS** at the default
   interval.
5. Degrade gracefully: if the server is down, the TUI still shows OS metrics and
   says so; if an OS source disappears, only that panel degrades.
6. Scriptable: `--once` and `--json` for statuslines, tmux, and CI smoke tests.

### Non-goals (v1)

- **Per-process GPU utilisation.** Requires `powermetrics` + root. The displayed
  GPU figure is device-wide and will include WindowServer.
- Model control (load/unload/restart). Read-only by design.
- Persistence: no CSV, no SQLite, no history across restarts. Ring buffer only.
- Power / energy / frequency metrics (`powermetrics`, IOReport): root or private
  frameworks, out of scope.
- Linux, Windows, Intel GPUs. Target is `aarch64-apple-darwin`; the code paths
  used are architecturally neutral but untested elsewhere.
- Remote HTTPS servers. `ureq` is built without TLS; loopback plain HTTP only.
- Config file. CLI flags plus environment variables only.

## 4. Architecture

Single binary, single-threaded UI, one sampler thread, **no async runtime**.

```
startup: discover() → Instance { mode, server_pid, gui_pid?, port, url }

            ┌───────────────────────────┐
            │ sampler thread (2 s tick) │
            │  http::poll  ──┐          │
            │  proc::sample ─┤→ Snapshot│
            │  vm::sample   ─┤    │     │
            │  gpu::sample  ─┘    │     │
            └─────────────────────┼─────┘
                                  │ mpsc::Sender<Snapshot>
            ┌─────────────────────▼─────┐
            │ UI thread (main)          │
            │  crossterm event::poll    │
            │  App { ring buffers }     │
            │  ratatui draw (diff only) │
            └───────────────────────────┘
```

Why no Tokio: a single 2 s timer plus one blocking HTTP GET and a handful of
syscalls does not need a work-stealing runtime. Skipping it saves roughly 5 MB
RSS and a background thread pool.

### 4.1 Crates

| Crate | Purpose | Notes |
|---|---|---|
| `ratatui` | TUI | buffer diffing, sparkline/gauge/table widgets |
| `crossterm` | terminal + events | `event::poll` with timeout, no busy loop |
| `ureq` (`default-features = false`) | blocking HTTP | no rustls, no gzip, no cookies |
| `serde`, `serde_json` | HTTP payloads, `--json` | unknown fields ignored |
| `libc` | syscalls, mach FFI | plus hand-written externs for IOKit/CoreFoundation |
| `core-foundation` | `CFDictionary` access for `PerformanceStatistics` | optional; can be replaced by raw FFI |
| `clap` (derive) | CLI | or `lexopt` if binary size matters |

No `tokio`, no `sysinfo` (it re-implements `libproc` less precisely and pulls a
dependency tree), no `objc2`.

### 4.2 File layout

```
Cargo.toml
SPECS.md
README.md
src/
  main.rs            CLI dispatch: tui | --once | --json
  cli.rs             argument parsing, env fallbacks
  config.rs          URL/port resolution, intervals, thresholds
  model.rs           Snapshot, HostMetrics, ProcessMetrics, GpuMetrics, serving structs
  sampler/
    mod.rs           per-tick snapshot assembly, CPU warm-up for one-shot mode
  http/
    mod.rs           client construction, retry/backoff, error classification
    endpoints.rs     typed GETs + exact serde structs for the four endpoints
  sys/
    mod.rs           platform gate (cfg target_os = "macos")
    proc.rs          proc_pid_rusage, CPU delta computation, pid metadata
    discover.rs      candidate scan, mode classification, instance selection
    socket.rs        PROC_PIDLISTFDS / PROC_PIDFDSOCKETINFO decoding (port)
    vm.rs            host_statistics64, swap, pressure, page accounting
    gpu.rs           IOKit FFI, PerformanceStatistics parsing
    info.rs          one-shot machine facts (chip, RAM, page size)
  ui/
    mod.rs           App state, event loop, key handling
    draw.rs          layout composition
    widgets.rs       gauge, sparkline row, models table, status badge
  format.rs          bytes, durations, percentages, throughput
tests/
  golden/            captured JSON payloads (paths redacted)
  parse.rs           deserialisation + formatting unit tests
```

## 5. Metrics: definitions and formulas

Ambiguity here is where monitoring tools lie. Each metric below has exactly one
definition.

### 5.1 Per-process CPU

Flavor `RUSAGE_INFO_V0`: it has existed since macOS 10.9 and carries the three
fields we need (`ri_user_time`, `ri_system_time`, `ri_phys_footprint`). Later
flavors add fields we would not display, at the cost of a 30-field hand-written
struct whose layout could silently drift.

In the TUI, the window is the sampling interval (2 s default). In one-shot mode
(`--once`, `--json`) there is no previous tick, so the sampler takes two readings
separated by `--warmup` (default 250 ms) and reports the CPU over that window.
A 250 ms window is noisier than a 2 s average and the README says so; `--warmup 0`
skips it and reports `null` CPU rather than inventing a number.

```
Δticks = (ri_user_time + ri_system_time)[t] - [...][t-1]     // mach absolute time
Δns    = Δticks * timebase_numer / timebase_denom
cpu_%  = Δns / Δwall_ns * 100                                 // 100 % = one full core
```

First tick has no delta → report `None`, render `—`. Never report a number
derived from a first sample.

Reported per process (`oMLX` app and `omlx-server` separately) and as a sum.

### 5.2 System CPU

```
Δbusy = Δ(user + system + nice)
Δall  = Δ(user + system + nice + idle)
cpu_% = Δbusy / Δall * 100
```

Same first-tick rule: `None` until two samples exist. Context only — the
per-process figures are the subject.

### 5.3 Process memory

`ri_phys_footprint`, displayed as bytes. This is the same figure Activity
Monitor shows under "Memory" and it includes Metal buffers mapped by the
process.

**Never add footprint to GPU memory.** On unified memory the same pages are
counted by both the process and the GPU driver's `Alloc system memory`. There
are two rows, never one sum.

### 5.4 System memory

From `host_statistics64` (`vm_statistics64_data_t`), in pages × `hw.pagesize`:

```
app_memory   = (internal_page_count - purgeable_count) * page_size
wired        = wire_count * page_size
compressed   = compressor_page_count * page_size          // memory held by the compressor
used         = app_memory + wired + compressed            // Activity Monitor model
cached       = external_page_count * page_size            // file-backed
free         = (free_count + speculative_count) * page_size
uncompressed = total_uncompressed_pages_in_compressor * page_size
```

`uncompressed` is the *logical* size of the data the compressor holds, not
memory consumed; it is shown as secondary context (`2.9G → 734M`), never added
to `used`. Gauge uses `used / hw.memsize`. The formula is documented in the
README because "memory used" is otherwise a matter of opinion.

### 5.5 GPU

| Field | Display |
|---|---|
| `Device Utilization %` | primary gauge, labelled **device** |
| `Renderer Utilization %`, `Tiler Utilization %` | secondary, small text |
| `In use system memory` | VRAM in use row |
| `Alloc system memory` | VRAM allocated row, small text |

These are whole-device percentages sampled by the AGX driver, updated about once
per second. Idle values of 15–19 % are normal on this machine (compositor). The
UI must say `GPU device` / `GPU in use`, never `oMLX GPU`.

### 5.6 Model memory

`model_memory_used` vs `model_memory_max` from `/api/status`, plus the
`current_model_memory` / `final_ceiling` pair from `/v1/models/status` and
`/health` when available. The gauge denominator is the **ceiling**, not total
RAM. The ceiling moves with system pressure, so it is re-read every tick rather
than cached.

### 5.7 Pressure

Two independent signals, both displayed:

1. Raw kernel value from `kern.memorystatus_vm_pressure_level`: `1 normal`,
   `2 warning`, `4 critical`.
2. Derived `used / total` percentage, with thresholds at 70 % and 85 %.

They can disagree (the kernel level is sticky). When they do, show the kernel
level as the authoritative badge and the ratio as context. Do not silently
convert one into the other.

## 6. Sampling

### 6.1 Cadence

| Work | Default | Configurable | Rationale |
|---|---|---|---|
| discovery (§6.3) | startup, then every 30 s, and on failure | — | process table + socket fds, µs |
| `/api/status` | every tick (2 s) | `--interval` | primary; 3 ms server cost |
| `/health` fallback | only when `/api/status` fails or returns 503 | — | discriminates loading vs down |
| OS counters (proc/vm/gpu) | every tick | same tick | µs each |
| `/v1/models/status` | every 15th tick (~30 s), plus on `m`, plus on startup | internal | 19 ms server cost |

`--interval` accepts 500 ms–60 s, clamped with a warning.

The models cache is invalidated immediately, without waiting for its TTL, when
`/api/status.models_loaded` disagrees with the filtered count of the cached list
— a model finished loading or unloading. This keeps §8.1's consistency rule from
turning a 30 s cache into a visible contradiction.

**Monitoring perturbs the measurement.** Each `/api/status` call costs ~3 ms of
CPU inside the Python process being measured. At 2 s that is ~0.15 % of a core —
the same order as the idle server. The default interval is 2 s rather than 1 s
for this reason, and the README states it plainly. We do not attempt to subtract
it; that would be guessing.

### 6.2 Tick algorithm

```
loop {
    wait_until(next_deadline)                 // no drift accumulation
    let t0 = Instant::now()
    let http = api.status()                   // Ok/503/Err + latency
    let procs = proc::sample(known_pids)      // resolves pids if unknown/empty
    let vm = vm::sample()
    let gpu = gpu::sample()
    let gpu2 = same-tick second read only if the first read failed
    tx.send(Snapshot { ... })
    next_deadline = t0 + interval
}
```

- `Instant`-based deadlines; if a tick overruns, skip rather than queue.
- HTTP timeout 1 s per request, connect timeout 500 ms.
- On connection failure: back off to 5 s, keep sampling OS counters, mark
  `server.reachable = false`, retain the last successful serving block with an
  age stamp (`last seen 12 s ago`). If the server pid is still alive, the badge
  is `unresponsive` rather than `unreachable` (§7.2): a model load blocks the
  Python event loop, and calling a live process "down" is wrong.
- PID cache and instance selection: see §6.3.5.
- GPU read failure (property missing) → `gpu: None`, panel shows `n/a`, no error
  spam.
- Admin endpoints are not called at all in v1 (§2.3), so there is no admin panel
  to degrade.

**Thread model.** Two producers, one consumer, no polling:

- `sampler` owns the HTTP client and the OS probes. It waits on
  `recv_timeout(until_deadline)` from a command channel, so pause, interval
  changes and refreshes take effect immediately while the thread sleeps
  otherwise. It publishes `Msg::Snapshot`.
- `input` blocks on `crossterm::event::read()` and forwards `Msg::Input(event)`
  into the same channel. Resize events arrive this way; there is no `poll()`
  loop and no periodic wake-up.
- the UI thread blocks on `recv()` of the merged channel and redraws only when a
  message arrives. Idle cost is therefore zero wake-ups between ticks.

Commands (UI → sampler): `Pause`, `Resume`, `SetInterval(Duration)`,
`RefreshNow`, `RefreshModels`, `Retarget(Instance)`, `Stop`.

**Terminal hygiene.** Raw mode and the alternate screen are released by an RAII
guard on normal exit, on `q`/`Esc`, and in a panic hook installed before
entering the TUI — `panic = "abort"` still runs the hook, and a monitor that
leaves the shell in raw mode after a crash is worse than the crash.

### 6.3 Instance discovery

Discovery answers three questions in order: **which oMLX processes exist**, **how
each was started**, and **which one serves the URL we measure**. It runs at
startup, then every 30 s, and immediately when a cached pid or socket fails.

#### 6.3.1 Candidates

Scan `proc_listallpids` (one syscall for the pid list, one `proc_pidpath` per pid)
and accept a process when **any** of these holds, cheapest first:

1. `proc_name == "omlx-server"` — free, and correct if a future build names the
   binary rather than only rewriting argv;
2. `proc_pidpath` contains `/oMLX.app/Contents/Resources/Python/` — the
   app-bundle server;
3. `proc_pidpath` contains `/Cellar/omlx/` or `/opt/omlx/` — a Homebrew install
   under any prefix, when the venv Python is *not* a framework build;
4. **the executable looks like a Python interpreter** and `KERN_PROCARGS2`
   `argv[0]` has basename `omlx` or `omlx-server`. "Looks like a Python
   interpreter" means, case-insensitively, that the basename starts with
   `python` **or** the path contains `/Python.framework/`. This is the only rule
   that reads the argument block, and it only runs for interpreter paths.

Rule 4 is what actually catches a Homebrew **framework** install, whose
`proc_pidpath` is
`.../Cellar/python@3.11/<version>/Frameworks/Python.framework/.../Python.app/Contents/MacOS/Python`
with basename `Python` — no `omlx` segment anywhere, and a name that a
lowercase-only `python3` check would miss.

Never accept on an environment token alone: `OMLX_*` variables are inherited by
unrelated children of a shell that exported them. The environment is used for
`OMLX_PORT` / `OMLX_SUPERVISED` only, after a process has been accepted.

Measured on the reference machine:

| Mode | `proc_pidpath` | basename | `p_comm` | accepted by |
|---|---|---|---|---|
| app | `/Applications/oMLX.app/Contents/Resources/Python/cpython-3.11/bin/python3.11` | `python3.11` | `python3.11` | rule 2 |
| service | `.../Cellar/python@3.11/3.11.18/Frameworks/Python.framework/Versions/3.11/Resources/Python.app/Contents/MacOS/Python` | `Python` | `Python` | rule 4 |

Candidates not owned by the current user are recorded and reported, never
silently dropped (§9).

#### 6.3.2 Mode classification

For each candidate, walk the parent chain (max 4 hops):

| Condition | Mode |
|---|---|
| an ancestor path ends with `/oMLX.app/Contents/MacOS/oMLX` | `app` — that ancestor is the GUI process to sample as well |
| `ppid == 1` | `service` — confirmed by `OMLX_SUPERVISED` in env or by `proc_pidpath` under `/Cellar/omlx/` |
| any other parent | `manual` — the parent's `comm` is shown in the header |
| ancestry unreadable (EPERM) | `unknown` |

`service` is the displayed spelling everywhere (UI, `--list`, JSON); `launchd` is
the mechanism, not a mode name.

`OMLX_SUPERVISED` is read with `sysctl KERN_PROCARGS2` (permitted for same-uid
processes, no root). It is confirmation only: ancestry decides, so a manual
`OMLX_SUPERVISED=launchd` cannot hide the real parent.

#### 6.3.3 Which port, hence which instance

URL resolution order:

1. `--url` — explicit, never overridden.
2. **The candidate's own listening TCP socket** — authoritative:
   - `proc_pidinfo(pid, PROC_PIDLISTFDS)` → array of `proc_fdinfo`
   - keep `proc_fdtype == PROX_FDTYPE_SOCKET` (2)
   - `proc_pidfdinfo(pid, fd, PROC_PIDFDSOCKETINFO)` → `socket_fdinfo`
   - accept when `psi.soi_kind == SOCKINFO_TCP` (2),
     `psi.soi_proto.pri_tcp.tcpsi_state == TSI_S_LISTEN` (1), and the address is
     loopback or wildcard
   - port = `ntohs((uint16_t)psi.soi_proto.pri_tcp.tcpsi_ini.insi_lport)`
     (`insi_lport` is stored in network byte order)
3. `OMLX_PORT` from the candidate's environment, when readable.
4. `~/.omlx/settings.json` → `server.port`.
5. `8000`, then `8008`, each probed with one `/api/status` call.

Verified on pid 93916 (app-bundle server): 21 fds, the listening socket is
`tcpsi_state == 1` with `insi_lport == 0x0000481f`, `ntohs` → **8008**. A full
list + decode loop measures **13.8 µs** on this machine (10 000 iterations), so
running it on every tick would still be cheaper than this paragraph. The
monitor keeps it at startup / every 30 s / on failure anyway, because fds are
more expensive than a cached pid and the value only changes when oMLX restarts.

Step 2 costs one `PROC_PIDLISTFDS` call (a few dozen fds) plus one
`PROC_PIDFDSOCKETINFO` per socket fd. It is the only method that survives
`OMLX_PORT`, a customized plist, or two instances on different ports.

#### 6.3.4 Several instances

Two deployments can run at once (app on 8008, service on 8000). The monitor does
**not** merge them.

- With `--url`: the instance owning that port wins; the others are named in the
  header as inactive.
- Without `--url`: prefer the instance whose port matches `settings.json`; if
  still ambiguous, target the lowest port and show `instance 1/2`. `i` cycles the
  target and rebuilds the HTTP client against the new port.
- Metrics are always scoped to the selected instance: its server pid, plus its
  GUI parent in `app` mode. The GUI app of one instance is never paired with the
  server of another.

#### 6.3.5 Caching and refresh

Resolve at startup; revalidate the selected pids with a cheap `proc_pidinfo` on
every tick; full re-scan every 30 s, or immediately when the HTTP call fails with
a connection error, when `proc_pid_rusage` returns ESRCH/EPERM, or when the
listening socket disappears. A `KeepAlive` restart is thus picked up within one
tick.

## 7. User interface

### 7.1 Layout (80×24 minimum, reflows above)

```
┌ omlx-top ── service:8000 ── healthy ── v0.7.3 ── up 20h11m ── 4ms ─────────┐
│ CPU   server   0.9%   app   5.3%   │ GPU DEVICE  [█████░░░░░░░░░]  17%     │
│ RAM   footprint 1.2G (3.7%)        │             renderer 17  tiler 18     │
│ MODEL 288M / 10.6G ceiling         │ VRAM        in use 1.17G  alloc 7.0G  │
│ SYS   used 12.3G  wired 4.1G       │             comp 2.0G  swap 2.9/4.0G  │
│       cached 5.4G  free 1.1G       │             cpu 12%  PRESSURE warn    │
├ history — last 120 samples ────────────────────────────────────────────────┤
│ cpu ▁▂▁▁▃▂▁▂▁▁                                                    0.9% avg │
│ gpu ▁▁▂▁▂▃▁▂▁▂                                                     17% avg │
│ mem ▂▂▂▂▃▃▃▃▃▃                                                     1.2G fp │
├ serving ───────────────────────────────────────────────────────────────────┤
│ 3 requests   0 active   0 waiting   prefill 15.6k tok/s   gen 0 tok/s      │
│ prompt 506,739   completion 0   cached 0   cache eff 0%                    │
├ models — 2 loaded / 20 discovered (a for all) ─────────────────────────────┤
│ ▸ gemma-4-12B-it-4bit        llm     ctx 262k   8.1G est   loaded          │
│   nomicai-modernbert-…-8bit  embed   ctx 8k     137M est   loaded          │
└ q quit   space pause   +/- int   i instance   a all   m refresh   ? help   ┘
```

The header carries the deployment mode and port (`app:8008`, `service:8000`,
`manual:8008`) so a glance distinguishes which instance is being measured. In
`app` mode the CPU/RAM rows show both processes; in `service`/`manual` mode the
`app` column reads `—`.

At narrower widths the two columns stack; below 60 columns the history panel is
dropped. At less than 20 rows the models table is dropped.

### 7.2 Header

`mode:port` · status badge · server version · uptime · last poll latency.

The mode segment is one of `app`, `service`, `manual`, `unknown`, suffixed by
`1/2` when several instances are listening and `i` can cycle. A tooltip line is
not available in a TUI, so the mode word must be self-explanatory: `service`
always means launchd, and `app` always means the GUI bundle.

Status badge states:

| Badge | Condition |
|---|---|
| `healthy` (green) | last 200, `status == "ok"` |
| `loading` (yellow) | 503 with `status == "loading"` |
| `unresponsive` (yellow) | transport timeout **and** the server pid still exists — a model load or heavy prefill holds the GIL and blocks the event loop for seconds |
| `unreachable` (red) | transport error / timeout with no live server pid, with `last seen Ns ago` |
| `auth` (red) | 401/403, hint `--api-key` |
| `degraded` (yellow) | 200 but payload missing expected fields |

### 7.3 Colour and legibility

- Green/yellow/red thresholds: CPU > 50/80 %, GPU > 50/80 %, model memory >
  60/85 % of ceiling, system used > 70/85 %.
- Never encode meaning by colour alone: every gauge shows its number, every
  badge shows its word.
- Respect `NO_COLOR` (disable styling, keep layout) and `TERM=xterm-mono`.

### 7.4 Keys

| Key | Action |
|---|---|
| `q`, `Esc` | quit |
| `space` | pause/resume sampling (UI keeps redrawing the frozen snapshot) |
| `+` / `-` | interval ±1 s, clamped 1–60 s |
| `a` | toggle models table: loaded only ↔ all discovered |
| `i` | cycle target instance when several are listening (§6.3.4) |
| `m` | force `/v1/models/status` refresh |
| `↑` / `↓`, `PgUp` / `PgDn` | scroll models table |
| `?` | help overlay |
| `r` | force full refresh |

No key writes anything. The tool is read-only; there is no `u`/`load` binding by
design.

### 7.5 Models table

One row per non-built-in model (§2.3). Columns: `▸` when loaded, id truncated to
the available width, `engine_type`/`model_type`, context window, estimated size
(`actual_size` when the server reports one), and state (`loaded`, `loading`,
`—`). Sort: loaded first, then loading, then alphabetical by id.

Default view is loaded + loading only; `a` switches to every discovered model.
The header of the panel states the counts and the current mode
(`2 loaded / 19 discovered — a for all`). Scrolling with `↑`/`↓`/`PgUp`/`PgDn`
applies to whichever view is active and is clamped to the row count; the
selected row is highlighted and its full id is shown in the footer line when it
is truncated.

The rows come from the same `/v1/models/status` response as the counters, so a
row marked `loaded` and the `loaded_count` cannot disagree.

## 8. CLI and environment

```
omlx-top [OPTIONS]

  --url <URL>          base URL; by default discovered from the running process
                       (listening socket, §6.3.3), else http://127.0.0.1:8000
  --api-key <KEY>      Bearer token; env OMLX_API_KEY
  --interval <DUR>     sampling period, e.g. 2s, 500ms (default 2s)
  --warmup <DUR>       measurement window used by --once/--json to obtain a
                       CPU delta (default 250ms; 0 reports null CPU)
  --once               print one snapshot as text, exit 0/1
  --json               print one snapshot as JSON, exit 0/1 (implies --once)
  --list               print discovered instances (mode, pid, port) and exit
  --no-color           disable styling
  -h, --help
  -V, --version
```

Exit codes for one-shot modes: `0` server reachable and healthy, `1` server
unreachable or degraded, `2` usage error. This makes the binary usable as a
health probe. `--list` follows the same rule: exit `0` when at least one
instance is discovered and the requested `--url` (if any) matched one; exit `1`
when nothing is discovered, or when `--url` names a port no discovered instance
listens on (print the rows, then `warning: no instance on port <port>` on
stderr).

### 8.1 JSON schema (`--json`)

```json
{
  "timestamp_unix_ms": 1794226879000,
  "server": {
    "mode": "app",
    "pid": 93916,
    "gui_pid": 38225,
    "url": "http://127.0.0.1:8008",
    "reachable": true,
    "http_status": 200,
    "status": "ok",
    "version": "0.7.3",
    "uptime_seconds": 72645.9,
    "last_latency_ms": 3
  },
  "instances": [
    { "mode": "app",     "pid": 93916, "gui_pid": 38225, "port": 8008, "selected": true },
    { "mode": "service", "pid": 41200, "gui_pid": null,  "port": 8000, "selected": false }
  ],
  "serving": {
    "total_requests": 3,
    "active_requests": 0,
    "waiting_requests": 0,
    "total_prompt_tokens": 506739,
    "total_completion_tokens": 0,
    "total_cached_tokens": 0,
    "cache_efficiency": 0.0,
    "avg_prefill_tps": 15605.1,
    "avg_generation_tps": 0.0
  },
  "models": {
    "discovered": 19,
    "loaded_count": 1,
    "loading_count": 0,
    "memory_used_bytes": 288687808,
    "memory_max_bytes": 11374576480,
    "loaded": ["gemma-4-12B-it-4bit"]
  },
  "processes": {
    "app":    { "pid": 38225, "cpu_percent": 5.3, "footprint_bytes": 33520 },
    "server": { "pid": 93916, "cpu_percent": 0.9, "footprint_bytes": 120000000 }
  },
  "system": {
    "cpu_percent": 12.4,
    "memory": { "total_bytes": 34359738368, "used_bytes": 13200000000,
                "wired_bytes": 4400000000, "compressed_bytes": 12000000000,
                "cached_bytes": 5800000000, "free_bytes": 1180000000 },
    "swap": { "total_bytes": 4294967296, "used_bytes": 3078000000 },
    "pressure": { "kernel_level": 2, "kernel_label": "warning", "used_ratio": 0.78 }
  },
  "gpu": {
    "device_util_percent": 17,
    "renderer_util_percent": 17,
    "tiler_util_percent": 18,
    "in_use_bytes": 1176158208,
    "alloc_bytes": 7001751552
  }
}
```

Absent or unavailable values are `null`, never `0`. A consumer must be able to
distinguish "idle" from "unknown". In `service` and `manual` mode,
`processes.app` is `null` (there is no GUI process to measure); in `app` mode it
is an object.

`models.discovered`, `models.loaded_count`, `models.loading_count` and
`models.loaded` are derived from **one** source so they never contradict each
other: `/v1/models/status` when available (filtered), else `/api/status`.
Consequently `loaded_count == loaded.len()` whenever that per-model source is
used. Built-in engines — `model_path` beginning with `builtin://`, or
`engine_type`/`model_type` equal to `markitdown` — are excluded from all four.

The `/health` last resort carries counters but no per-model ids. It reports
`loaded_count` from `engine_pool.loaded_count` and `loaded: null` (unknown),
never `[]`: an empty array must keep meaning "fetched, nothing loaded". When no
source answers, both are `null`.

## 9. Failure and degradation matrix

| Failure | Behaviour |
|---|---|
| Server not running | red `unreachable` badge; OS panels keep updating; serving/model panels show last values greyed with age |
| Server alive but HTTP times out (model load, GIL held) | yellow `unresponsive` badge; process panels keep updating; serving block goes stale with its age; retry after the normal back-off |
| 503 `loading` | yellow `loading` badge; model gauge shows `loading`; keep polling |
| 401/403 | red `auth` badge; hide the serving/model panels; suggest `--api-key` |
| Malformed / short JSON | `degraded` badge; keep last good values; log once per field, not per tick |
| `/v1/models/status` fails | models table keeps previous rows, marked stale; serving block unaffected |
| `IOAccelerator` missing/changed | GPU panel `n/a`; no error banner; rest of the app unaffected |
| `proc_pid_rusage` fails on a cached pid | re-resolve pids; if still failing, show `—` |
| Plist installed, service not running | red `unreachable` + hint `brew services start omlx`; one `stat` on the plist, no `launchctl` spawn |
| Service running as another user (sudo) | OS panels show `n/a — permission denied`; HTTP panels and serving counters keep working |
| Two instances listening | target one, header shows `instance 1/2`; `i` cycles; never merged |
| `KeepAlive` restart between ticks | new pid resolved within one tick; on-screen uptime and totals reset — expected, not an error |
| Discovery finds nothing | `unreachable` + one-line diagnosis (`no omlx-server process`, plist present or not) |
| Terminal resize below 60×20 | fluid layout; drop history, then table |
| Interval overrun (> 3× interval) | skip ticks, never queue; show `slow tick` in header |

No panics on any of the above. `unwrap()` is forbidden outside `main()` and
tests; errors are typed (`HttpError`, `SysError`) and surfaced as `Option`/`Result`
into the snapshot.

## 10. Performance budget

Acceptance, measured with `top -pid $(pgrep omlx-top) -l 120` at the default 2 s
interval, five-minute idle run, 100×40 terminal:

| Metric | Budget |
|---|---|
| Average CPU | < 1.0 % of one core |
| Peak CPU (single tick) | < 10 % |
| RSS | < 15 MB |
| Startup to first frame | < 150 ms |
| Child processes spawned | 0 |
| Samples dropped | 0 |

`[profile.release]`: `lto = "fat"`, `codegen-units = 1`, `panic = "abort"`,
`strip = true`.

If the budget is missed, the order of fixes is: raise the default interval to
3 s, reduce ring buffer to 60, drop the `core-foundation` crate for raw FFI.

## 11. Testing

- **Unit:** deserialise the captured payloads in `tests/golden/` (real responses
  from 0.7.3, paths redacted); assert unknown fields are ignored and missing
  fields become `None` rather than errors.
- **Unit:** formatting — bytes (`999 B`, `1.0K`, `10.6G`), durations
  (`20h11m`), percentages, throughput (`15.6k tok/s`).
- **Unit:** CPU delta maths with synthetic `rusage` pairs, including first-tick
  `None` and counter-reset (pid reuse) detection.
- **Unit:** VM page accounting against a hand-built `vm_statistics64` struct.
- **Unit:** pressure mapping (1/2/4 and unknown values).
- **Unit:** ring buffer wraparound and average computation.
- **Unit:** `socket_fdinfo` decoding — port extraction from network byte order,
  `TSI_S_LISTEN` filter, ignoring UDP/unix sockets — over synthetic byte
  buffers, so no live process is needed.
- **Unit:** mode classification over synthetic parent chains (`ppid == 1`,
  app-bundle ancestor, shell ancestor, EPERM gap).
- **Unit:** candidate signature matrix — app-bundle path, `/Cellar/omlx/` path,
  framework `Python.app` path (capital `Python`), plain `python3.11` path with
  `argv[0] == "omlx-server"`, interpreter path with an unrelated argv, and a
  process carrying `OMLX_PORT` in its environment but not accepted on that basis
  alone.
- **Unit:** URL precedence (explicit `--url` > socket > `OMLX_PORT` >
  `settings.json` > 8000 > 8008).
- **Integration (manual, documented in README):**
  - app mode: GUI running, server reachable;
  - service mode: `brew services start omlx`, monitor started with no arguments;
  - both at once on 8000 and 8008: `--list` shows two rows, `i` switches, the
    per-process figures follow the switch;
  - server stopped, service plist still installed: hint shown;
  - during a model load; resize to 60×20 and 200×60; `--json | jq .` validates.
- **Smoke:** `cargo test`, `cargo clippy -- -D warnings`, `cargo fmt --check`.

No network in tests. No test may require a running oMLX.

## 12. Risks and accepted limitations

1. **GPU is device-wide.** WindowServer traffic is included. Documented in the
   UI label and README. This is a platform limitation, not an implementation
   shortcut.
2. **`IOAccelerator.PerformanceStatistics` is undocumented.** It has been stable
   for years and is what Activity Monitor and third-party monitors use, but a
   future macOS could change it. Failure is contained to the GPU panel.
3. **`kern.memorystatus_vm_pressure_level` is undocumented.** Values 1/2/4 are
   observed behaviour; the UI shows the raw integer alongside the label so a
   change is visible rather than wrong.
4. **Polling inflates the measured CPU** by ~0.15 % of a core at 2 s. Documented,
   not compensated.
5. **`model_memory_max` is dynamic.** Comparisons across time are not apples to
   apples; the gauge always shows the current ceiling.
6. **Multiple server instances** are a normal configuration, not an error
   (app + service). One is targeted at a time, the header says which, and `i`
   switches. They are never merged.
7. **Footprint vs VRAM double counting** on unified memory. Two rows, never
   summed.
8. **Homebrew paths are versioned** (`Cellar/omlx/0.7.3/...`). Discovery must
   never match on a literal version or on `Cellar` depth.
9. **Three port layers disagree by default** (8000 default, 8008 in the app's
   settings, `OMLX_PORT` override). Only socket introspection is authoritative;
   config files are a last resort.
10. **`KeepAlive` restarts reset counters.** `uptime_seconds`, totals and
    per-model stats restart from zero after a crash loop. The UI shows them as
    given; it does not pretend they are continuous.
11. **A root-run service is partly unobservable.** Without root we cannot read
    `rusage` or fds of another user's process. HTTP still works. We say so
    instead of showing zeros.
12. **Metrics are only as good as the server's own counters.** `avg_prefill_tps`
    is a session average, not instantaneous; the UI labels it `session avg`.

## 13. Definition of done (v1)

- [ ] `SPECS.md` reviewed and agreed.
- [ ] `cargo build --release` clean, `clippy -D warnings` clean, `fmt` clean.
- [ ] TUI renders all panels at 100×40 with no flicker (buffer diffing verified
      by eye at 2 s cadence).
- [ ] `--json` output validates against §8.1 and is stable in shape.
- [ ] Delegation modes verified end to end: `app` (GUI running), `service`
      (`brew services start omlx`, monitor launched with no arguments), `manual`
      (`omlx serve` in a shell), and two instances at once (`--list` + `i`).
- [ ] Behaviour confirmed against the matrix in §9: server stopped, service
      stopped with plist present, server loading, 401, GPU property removed,
      service running as another user.
- [ ] Performance budget in §10 met on the reference machine.
- [ ] README documents sources, formulas, limitations, deployment discovery, and
      the 2 s interval rationale.
