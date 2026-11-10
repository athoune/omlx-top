# AGENTS.md

Project-specific instructions for AI agents working in this repository.
Commit authorship, push policy and general conduct live in the maintainer's
global `AGENTS.md`; this file only adds what is specific to `omlx-top`.

## What this is

`omlx-top` is a read-only macOS terminal monitor for oMLX. It reads the oMLX HTTP
API and macOS process/GPU/VM interfaces. It never writes to oMLX, never loads or
unloads a model, and never spawns a child process.

- `SPECS.md` is the contract. Read it before changing behaviour, and update it in
  the same commit when behaviour, a threshold or a JSON field changes.
- `README.md` is the user-facing documentation. Keep it truthful: no feature
  listed before it works, no number quoted that was not measured.

## Commands

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
./target/release/omlx-top --once
./target/release/omlx-top --json | python3 -m json.tool
./target/release/omlx-top --list
```

## Hard rules

- **macOS only.** Do not add Linux or Windows support. The `compile_error!` is
  deliberate; `cargo install` on another platform must fail loudly.
- **No async runtime.** The TUI uses one sampler thread, one blocking input
  thread, and a UI thread that only wakes on a message. Do not add `tokio` or a
  polling redraw loop.
- **The binary never spawns processes.** No `ioreg`, `ps`, `lsof`, `launchctl`,
  `top` or `powermetrics`. Use libproc, sysctl and IOKit.
- **Read-only.** No load/unload/restart/settings action, ever.
- **No `unwrap()`/`expect()`** outside `main()` and tests.
- Comments in English. No `#[allow(...)]` to silence a real warning.
- Every new dependency must be justified against `SPECS.md` §4.1. The current set
  is deliberately small: `clap`, `libc`, `ratatui`, `serde`, `serde_json`,
  `ureq` (no TLS, no gzip).
- Never tag, push, publish to crates.io or create a release without an explicit
  instruction from the maintainer.

## Architecture

| Path | Owns |
|---|---|
| `src/sys/discover.rs` | candidate scan, ancestry classification, instance selection |
| `src/sys/socket.rs` | listening-socket decoding (port source of truth) |
| `src/sys/proc.rs` | pid metadata, `rusage`, CPU deltas, `KERN_PROCARGS2` |
| `src/sys/vm.rs` | page accounting, swap, pressure, system CPU |
| `src/sys/gpu.rs` | IOKit `PerformanceStatistics` |
| `src/http/` | loopback client, typed payloads, status classification |
| `src/sampler/` | tick loop, snapshot assembly, models cache |
| `src/ui/` | ratatui layout, widgets, keys, lifecycle |
| `src/model.rs` | snapshot and JSON-facing types |
| `src/format.rs` | human formatting (bytes, durations, counts) |

The one-shot paths (`--once`, `--json`, `--list`) and the TUI share the same
discovery and sampling code. Do not fork them.

## Landmines

Each of these cost a debugging session. Do not "simplify" them away.

1. **`socket_fdinfo` is 792 bytes** in the SDK; the union is sized by
   `un_sockinfo` (528), not `tcp_sockinfo` (120). `proc_pidfdinfo` with a smaller
   buffer returns `ENOMEM`, the port list stays empty, and discovery silently
   falls back to `settings.json`. The code passes a 1024-byte scratch buffer and
   reads the typed prefix. Keep it that way.
2. **`p_comm` / `proc_name` is the interpreter**, `python3.11` for the app bundle
   and `Python` for the Homebrew framework build, never `omlx-server`. The server
   rewrites `argv`, not `p_comm`. Identify candidates by executable path and
   `argv[0]`.
3. **`mach_host_self()` returns a send right** and increments its reference
   count. Calling it per sample leaks a port right every tick; it is cached in a
   `OnceLock`.
4. **`ioreg` costs ~26 ms of CPU per spawn.** It was replaced by IOKit FFI for a
   reason.
5. **Unified memory double counts.** `ri_phys_footprint` and the GPU driver's
   `Alloc system memory` overlap. Two rows, never a sum.
6. **Built-in engines are not models.** `MarkItDown` (`builtin://markitdown`) is
   filtered from every count and from the table. The server's own counters do
   the same (19 vs 20).
7. **A model load blocks the server event loop.** `/api/status` can exceed the
   1 s timeout while the process is healthy. A timeout with a live pid is
   `unresponsive`, not `unreachable`.
8. **`model_memory_max` moves with system pressure.** Never cache it, never
   compare a gauge to an older ceiling.
9. **A root-run service is partly unobservable.** Without root, `rusage` and fds
   of another user's process are unreadable. Report `n/a`, never `0`.
10. **The two columns must not collide.** Gauges compute their width from the
    real label length; a longer byte string must shrink the bar, not overwrite
    the right column.

## Testing

- `tests/golden/` holds real captured payloads; redact `/Users/...` paths.
- UI tests use `ratatui::Terminal<TestBackend>`; they must not need a TTY.
- Pure functions for anything testable: formatting, decoding, classification,
  sorting, ring buffers. Every metric needs a test at its boundary.
- No test may require the network, a running oMLX, or a live GPU.
- `clippy -D warnings` is part of the gate. It is MSRV-aware: when you bump
  `rust-version`, it will start rejecting newer APIs (that is how `1.77` was
  caught using `is_none_or` and `is_multiple_of`).

## Performance budget

Under 1 % of one core and 15 MB RSS at the default 2 s interval, zero child
processes. The 2 s default exists because each `/api/status` call costs ~3 ms of
CPU inside the process being measured. Measure with the five-minute method in
`README.md` before claiming a change is free.

## Release process

1. **Write the release notes first**, in `docs/release-notes/v<version>.md`, and
   commit them. They become part of the tag. See the template below.
2. Run `scripts/release.sh <version>` (add `--push` to publish).
3. The script bumps `Cargo.toml`, refreshes `Cargo.lock`, runs `fmt`, `clippy`,
   the tests and `cargo publish --dry-run`, commits, and creates the annotated
   tag. It refuses a dirty tree, a non-`main` branch, or an existing tag. Passing
   the current version re-tags without an empty commit.
4. Pushing the tag starts `.github/workflows/rust.yml`:
   - `test`: fmt, clippy, tests, release build;
   - `release`: arm64 build, `lipo` architecture check, tag ↔ `Cargo.toml`
     check, packaging, **crates.io trusted publishing**, draft GitHub release,
     assets, publish;
   - `provenance`: SLSA v2.1.0, attaches `<archive>.intoto.jsonl`.
5. crates.io publishing uses **trusted publishing** (`rust-lang/crates-io-auth-action`),
   with no token in repository secrets. One-time setup: crates.io → the crate's
   Settings → Trusted Publishing → add a GitHub publisher for
   `athoune/omlx-top`, workflow `rust.yml`. Without that configuration the
   release job fails **before** creating the GitHub release, which is the
   intended fail-loud behaviour.
6. crates.io versions are immutable. Never reuse, delete or silently republish a
   version. The tag must equal `Cargo.toml`; the workflow enforces it.
7. Never put a checksum in the release notes. CI builds the archive, so a hash
   written by hand describes a different file. Point to `SHA256SUMS`.

### Release notes

Path: `docs/release-notes/v<version>.md`, exactly matching the tag.

- **Language:** English.
- **Audience:** someone deciding whether to upgrade, not a commit reader.
- **Tone:** factual. No marketing, no badges, no "we are excited", no roadmap.
- **Length:** under ~60 lines.

Template:

```md
# omlx-top vX.Y.Z

One paragraph: what this release changes and why it matters to a user.

## Highlights

- User-visible change, with a measured number when one is quoted.
- Another one.

## Upgrade notes

- Breaking CLI, JSON or configuration change, or "None."

## Install

```sh
cargo install omlx-top
```

Or the Apple Silicon archive attached to this release.

## Known limitations

- Only new or newly relevant limitations; the README holds the full list.

## Checksum and provenance

Verify with the `SHA256SUMS` asset:

```sh
shasum -a 256 -c SHA256SUMS
```

SLSA provenance is attached as an `.intoto.jsonl` asset.
```

Rules:

- User-visible changes only. Internal refactors are not highlights.
- Every number must be measured, with the machine or protocol context.
- Do not paste a checksum (see rule 7 above).
- If the file is missing, CI falls back to generated notes. Treat that as a
  defect for any release that is more than a test tag.
