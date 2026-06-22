# DiskSpy

Lightweight Windows background service that watches which processes are
silently consuming your disk space, attributes every change to the
responsible application, and exposes the result through a local web
dashboard.

---

## Table of contents

1. The problem
2. What DiskSpy does
3. What DiskSpy does not do
4. How it works
5. Technology stack
6. Repository structure
7. Prerequisites
8. Building from source
9. Running
10. Configuration
11. Dashboard panels
12. REST API reference
13. Performance and resource use
14. Limitations and known gaps
15. License

---

## 1. The problem

On Windows, you can see that disk space disappeared, but you cannot easily
see which process ate it. Task Manager shows CPU and RAM per process. It
does not show disk growth. Disk usage graphs in Windows Resource Monitor
report throughput (bytes per second), not persistent size change. The
"Storage" settings page shows you which folder is big, but not which
application put it there.

DiskSpy fills that gap. It tells you that `docker.exe` wrote 4.2 GB to
`C:\ProgramData\Docker\volumes\` last Tuesday at 3:14 PM, that `cargo`
consumed 1.1 GB under `\.cargo\registry` on Sunday, and that `claude.exe`
expanded a cache file 37 times in the past hour.

---

## 2. What DiskSpy does

DiskSpy watches every file write on the system (with optional drive and
path filters), groups the writes into per-file change events, attributes
each event to the process that produced it, persists the events to a
local SQLite database, and serves a browser dashboard at
`http://localhost:7272`.

Concrete capabilities:

- Real-time attribution of disk growth to specific processes
- Per-file aggregation (10,000 small writes become one DB row)
- Configurable size threshold and time debounce
- Excludes by path prefix and by process name
- Friendly labels for known tools (Docker, npm, pip, cargo, Ollama, ...)
- Dashboard with: today's top consumers, 30-day timeline, live event feed,
  largest files changed
- REST API for external tooling
- Per-day aggregate table for fast dashboard queries
- Automatic retention enforcement

---

## 3. What DiskSpy does not do

DiskSpy does not monitor real-time I/O throughput. It does not tell you
"this process is reading 50 MB per second right now." It monitors
persistent size changes: when a file or directory ends up larger (or
smaller) than it was before.

Other things it does not do:

- It does not delete files for you. It observes.
- It does not require a kernel driver. It uses the same user-mode ETW
  consumer mechanism as Sysinternals ProcMon.
- It does not send telemetry anywhere. Everything stays on disk in
  `diskspy.db`.

---

## 4. How it works

DiskSpy sits on top of ETW (Event Tracing for Windows). The Windows
kernel fires structured events for every file I/O operation, and each
event includes the responsible Process ID. DiskSpy is a user-mode
consumer of those events, no driver required.

The data flow:

```
  Windows kernel (FileIo events)
        |
        v
  ferrisetw kernel consumer (raw events)
        |
        v
  nt_to_dos path translation    (raw events, DOS paths)
        |
        v
  Process cache                 (PID -> process name, TTL 30 s)
        |
        v
  Debouncer                     (per-file aggregation, threshold filter)
        |
        v
  SQLite writer                 (change_log + daily_summary tables)
        |
        v
  Axum HTTP server              (REST API + dashboard at /)
```

### Step by step

1. **ETW subscription.** At startup, DiskSpy opens a kernel trace session
   named `DiskSpy-KernelTrace` and subscribes to the `FileIo` and
   `FileIo_Init` kernel providers. Every file write, create, cleanup,
   close, rename, and delete on the watched drives produces an event.
   Each event carries a Process ID and the NT device path of the file.

2. **Path normalization.** The kernel reports paths as
   `\Device\HarddiskVolume3\Users\alice\...`. DiskSpy maintains a map of
   volume device names to drive letters (built once on startup by
   iterating `A:` through `Z:` and calling `QueryDosDeviceW`) and
   rewrites every event path to the conventional form (`C:\Users\alice\...`).

3. **Process resolution.** The Process ID is resolved to a process
   basename (`docker.exe`, `cargo.exe`, ...) using the Win32
   `CreateToolhelp32Snapshot` / `Process32FirstW` / `Process32NextW`
   APIs. Results are cached for 30 seconds because Windows reuses PIDs.

4. **Per-file debounce.** Without aggregation, a single 100 MB Docker
   layer write would produce thousands of events. The debouncer holds a
   `DashMap<file_path, FileState>` and accumulates bytes per file. Every
   500 ms a background task inspects entries that have been quiet for
   longer than `debounce_seconds` and emits a single
   `FileChangeRecord` per file. Records below `min_delta_bytes` are
   silently dropped.

5. **Filtering.** Before the debouncer sees an event, the process name
   and the file path are checked against the user's
   `exclude_processes` and `exclude_paths` lists. Default exclusions
   cover Windows Defender, Search Indexer, Service Host, the Recycle
   Bin, the user Temp folder, and other noise sources.

6. **Persistence.** Each `FileChangeRecord` is inserted into
   `change_log` and used to update a pre-aggregated
   `daily_summary` table so dashboard queries stay fast.

7. **HTTP server.** A Tokio task runs an Axum server bound to
   `dashboard_port`. The dashboard route (`/`) serves the bundled
   `assets/dashboard.html`. API routes return JSON.

8. **Graceful shutdown.** On `Ctrl+C`, the shutdown watch flips,
   the debouncer drains its pending entries, the DB writer task flushes,
   and the binary exits cleanly.

---

## 5. Technology stack

| Component | Crate | Version | Why this choice |
|---|---|---|---|
| ETW consumer | `ferrisetw` | 1.2 | The cleanest safe-Rust wrapper for ETW. Same library family used in production tooling. |
| Async runtime | `tokio` | 1.52 | Zero-cost async, timers for the debounce tick, channels for the ETW to DB pipeline. |
| Database | `rusqlite` | 0.31 (bundled) | SQLite compiled into the binary. No external SQLite library required. |
| HTTP server | `axum` | 0.7 | Minimal, fast, ergonomic. Pairs with `tower-http` for CORS. |
| HTTP middleware | `tower-http` | 0.5 | CORS layer for the dashboard. |
| Serialization | `serde`, `serde_json` | 1.0 | Records serialize to JSON for the API. |
| Time | `chrono` | 0.4 | With `serde` feature for the JSON wire format. |
| Concurrent maps | `dashmap` | 5.5 | Lock-free reads on the hot path. |
| Mutex | `parking_lot` | 0.12 | Faster than `std::sync::Mutex`. |
| Config | `toml` | 0.8 | Human-editable config file. |
| Errors | `anyhow`, `thiserror` | 1.0 | Error context for the entry point, derived error types in modules. |
| Win32 bindings | `windows` | 0.58 | ToolHelp32 for PID lookup, `QueryDosDeviceW` for path conversion, `GetTokenInformation` for the elevation check. |
| Logging | `tracing` | 0.1 | Structured logging with `EnvFilter`. |

---

## 6. Repository structure

```
Disk Spy/
|-- Cargo.toml                    Crate manifest
|-- Cargo.lock                    Pinned versions
|-- build.bat                     MSVC bootstrap wrapper for cargo
|-- embed_manifest.bat            Embeds the UAC manifest into release binary
|-- config.toml                   User-editable configuration (auto-created)
|-- README.md                     This file
|-- how-to-use.md                 End-user guide
|-- done.md                       Build log
|-- .gitignore
|
|-- assets/
|   |-- dashboard.html            Single-file dashboard (Chart.js via CDN)
|   |-- diskspy.exe.manifest      UAC elevation manifest
|
|-- src/
|   |-- main.rs                   Entry point, wiring, Ctrl+C handling
|   |-- config.rs                 TOML config, default generation, filters
|   |-- db.rs                     SQLite schema, insert, all dashboard queries
|   |-- debouncer.rs              Per-file aggregation, threshold filter
|   |-- etw.rs                    ETW consumer, path translation, elevation check
|   |-- process_cache.rs          PID -> process name with TTL
|   |-- server.rs                 Axum HTTP server (REST + dashboard)
|
|-- target/                       Build output (gitignored)
|   |-- debug/diskspy.exe
|   |-- release/diskspy.exe       ~5 MB optimized + LTO + stripped
|
|-- (no runtime files in the project tree; everything goes to %LOCALAPPDATA%\DiskSpy\)
```

Total source: roughly 1,700 lines of Rust.

---

## 7. Prerequisites

### Operating system

- Windows 10 or Windows 11, 64-bit
- Windows SDK 10.0.19041 or later (for the linker; already present on
  most development machines)

### Rust toolchain

- `rustc` 1.75 or later (1.91 confirmed working)
- `cargo` 1.75 or later
- The `x86_64-pc-windows-msvc` target installed (`rustup target add` it
  if missing)

### C build tools

- Microsoft Visual Studio 2019 Build Tools (or newer) with the "Desktop
  development with C++" workload. This provides `cl.exe`, `link.exe`,
  and the Windows SDK headers and libraries that `windows-sys` and
  `rusqlite` need.

If you have Visual Studio installed but `cl.exe` is not on PATH, run
`build.bat` — it calls `vcvarsall.bat x64` automatically before invoking
cargo.

### Runtime

- Administrator privileges at launch (the manifest prompts UAC
  automatically on Windows 10 and later)
- Approximately 10 MB of disk space for the binary
- A few hundred MB for `diskspy.db` after extended use

### Optional

- `sqlite3.exe` on PATH if you want to inspect the database from the
  command line. Not required at runtime.
- Any modern web browser for the dashboard.

---

## 8. Building from source

Open a regular PowerShell or Git Bash in the project directory and run:

```powershell
# Debug build (faster compile, larger binary, used during development)
./build.bat build

# Release build (slower compile, smaller and faster binary, used in production)
./build.bat build --release
```

`build.bat` will:

1. Call `vcvarsall.bat x64` so `cl.exe` and `link.exe` are on PATH.
2. Run `cargo build` (or `cargo build --release`).
3. If the build was `--release`, invoke `mt.exe` to embed the UAC
   manifest so launching the binary auto-prompts UAC.

The first build downloads and compiles all dependencies and takes
several minutes. Subsequent incremental builds are fast.

To run the test suite:

```powershell
./build.bat test
```

The current test suite contains 18 tests covering the database,
debouncer, process cache, configuration, and a full end-to-end
integration test that verifies the entire pipeline.

---

## 9. Running

There are two launch modes. **Background / tray mode is the recommended
one for normal use** - the console window hides itself and a tray icon
gives you access to the dashboard, log file, data folder, and a Quit
option.

### Background / tray mode (recommended)

From an elevated PowerShell:

```powershell
cd "D:\Disk Spy"
.\target\release\diskspy.exe --background
```

Or, since the UAC manifest is embedded, just double-click
`target\release\diskspy.exe` in Explorer. Windows shows a UAC dialog;
accept it, and DiskSpy starts in the background. A blue tray icon
appears in the notification area.

The tray menu:

- **Open Dashboard** - opens `http://localhost:7272` in your default
  browser.
- **Show Log File** - opens the current log file in your default text
  handler.
- **Open Data Folder** - opens `%LOCALAPPDATA%\DiskSpy\` in Explorer.
- **Quit DiskSpy** - signals a clean shutdown and exits.

### Foreground / console mode (for development and debugging)

From an elevated PowerShell:

```powershell
cd "D:\Disk Spy"
.\target\release\diskspy.exe
```

A console window stays open showing live log output. Press `Ctrl+C` to
stop.

### What happens on first launch (both modes)

1. Print `DiskSpy v0.1.0 starting...` to console and to the log file.
2. Detect it is running as Administrator (exit 1 with a clear error if
   not).
3. Create `%LOCALAPPDATA%\DiskSpy\` if it does not exist.
4. Read or create `config.toml` inside that directory.
5. Open or create `diskspy.db` inside that directory.
6. Apply the retention policy.
7. Scan `A:` through `Z:` and build the volume device map.
8. Start the ETW kernel trace session named `DiskSpy-KernelTrace`.
9. Begin listening for HTTP on `dashboard_port` (default `7272`).
10. In background mode, hide the console and install the tray icon.
11. In console mode, print the dashboard URL and wait for `Ctrl+C`.

### Where is my data stored?

**Always under `%LOCALAPPDATA%\DiskSpy\`.** This is
`C:\Users\<you>\AppData\Local\DiskSpy\` on a default Windows install.
Inside that directory you will find:

- `config.toml` - your editable configuration.
- `diskspy.db` - the SQLite database.
- `diskspy.log.YYYY-MM-DD` - the rolling log file (one per day).

The exact paths are printed at startup and returned by `/api/status`.

### Stopping

Press `Ctrl+C` in the console window. DiskSpy will:

1. Signal all background tasks to stop.
2. Flush any pending debouncer aggregations into the database.
3. Close the database connection cleanly.
4. Print `DiskSpy stopped.` and exit.

---

## 10. Configuration

`config.toml` is created automatically on first run with these
defaults. Edit it and restart DiskSpy to apply changes.

### General settings

| Key | Default | Description |
|---|---|---|
| `dashboard_port` | `7272` | TCP port for the HTTP dashboard and API. |
| `min_delta_bytes` | `51200` | Records smaller than this are silently dropped. Default 50 KB. |
| `debounce_seconds` | `2` | Wait this long after the last write before committing a record. |
| `retention_days` | `90` | Delete rows older than this on startup. |

### Watch settings

| Key | Default | Description |
|---|---|---|
| `drives` | `["C:\\"]` | Drives to monitor. |
| `exclude_paths` | (system paths) | Path prefixes to skip. `%USERNAME%` is expanded. |
| `exclude_processes` | (system processes) | Process basenames to skip. Case-insensitive. |

### Labels

A map from process basename to the friendly name shown in the
dashboard. Out of the box, `docker.exe` is shown as "Docker Desktop",
`cargo.exe` as "Cargo (Rust)", `pip.exe` as "pip", and so on. Add your
own tools here.

See `config.toml` in the project root for the full list.

---

## 11. Dashboard panels

The bundled `assets/dashboard.html` is a single-file SPA that polls the
REST API every 10 seconds. Four panels.

### Today's Top Space Consumers

A horizontal bar chart with process labels on the y-axis and bytes
added on the x-axis. Color-coded by category. Beneath it, a table with
the same data plus event counts.

### 30-Day Growth Timeline

A stacked bar chart. Each bar is one day; each color is one
process/app. Hovering shows the breakdown.

### Recent Events (live)

A table of the 50 most recent debounced records, auto-refreshing
every 10 seconds. File paths are truncated; click a row to see the
full path. Additions are colored green; deletions are red.

### Largest Files Changed (last 7 days)

A table of the files with the largest cumulative growth over the past
week, sortable by clicking the column headers.

---

## 12. REST API reference

All endpoints respond with JSON. Errors return HTTP 500 with
`{"error": "..."}`.

### GET /api/status

```json
{
  "status": "running",
  "uptime_seconds": 3600,
  "events_today": 142,
  "db_size_mb": 2.4
}
```

### GET /api/changes?limit=100

The most recent change records, newest first.

```
[ { "id": 123, "changed_at": "2026-06-22T15:42:11Z",
    "process_name": "docker.exe", "process_label": "Docker Desktop",
    "file_path": "C:\\ProgramData\\Docker\\volumes\\...",
    "delta_bytes": 4194304, "category": "Docker" }, ... ]
```

Query parameter `limit` (default 100, max 1000).

### GET /api/top-growers?days=1

The processes with the most bytes added in the last `days` days
(default 1, max 90).

```
[ { "process_label": "Docker Desktop", "total_bytes": 2147483648,
    "event_count": 47 }, ... ]
```

### GET /api/daily?days=30

Per-day per-process growth for the last `days` days. Used by the
timeline chart.

```
[ { "date": "2026-06-21", "process_label": "Docker Desktop",
    "total_bytes": 500000000, "event_count": 12 }, ... ]
```

### GET /api/largest-files?days=7

The files with the largest cumulative growth in the last `days` days.

```
[ { "file_path": "C:\\...\\layer.tar",
    "process_label": "Docker Desktop",
    "total_bytes": 1073741824 }, ... ]
```

### GET /api/config

The current configuration as JSON. Useful for verifying edits.

### POST /api/config/exclude-process

Adds a process basename to the in-memory exclude list. Body:

```json
{ "process_name": "myapp.exe" }
```

Note: this updates only the running process's view. To make it
persistent, also edit `config.toml`.

### GET /

Returns the dashboard HTML. No special URL parameters.

---

## 13. Performance and resource use

DiskSpy is designed to be cheap at idle and bounded under load.

- Idle CPU: under 0.5% on Windows 11 with no active writes.
- Idle memory: roughly 10-15 MB working set.
- Memory under load: stays below 50 MB even with thousands of writes
  per second, because the debouncer caps the in-memory map.
- Disk I/O: only one SQLite transaction per debounced record. With
  the default `debounce_seconds = 2`, expect tens of rows per minute
  on a typical workstation.
- ETW overhead: the kernel provider runs whether or not DiskSpy is
  consuming, but at a cost measured in single-digit percent of one
  CPU core.

---

## 14. Limitations and known gaps

These are inherent to the design and not on the roadmap for the
initial release.

- **Administrator required.** Solved for the launch (UAC manifest), not
  for the kernel consumer's intrinsic need for elevation.
- **WSL2 internal writes are invisible.** Files written inside the
  Linux VM at `\\wsl$\Ubuntu\...` are not seen by the Windows kernel
  tracer. Use `inotifywait` inside WSL and pipe events out over TCP if
  this matters.
- **Write bytes are not the same as net growth.** ETW reports bytes
  written per event, not the final file size delta. A "write 100 MB
  then immediately delete" sequence will be logged as a +100 MB event
  even though the disk usage did not change. A periodic
  `fs::metadata` reconciliation would address this.
- **No baseline scan.** DiskSpy only knows about files that change
  after it starts. A `--baseline` flag that walks the drive once on
  startup would address this.
- **No auto-start on boot.** A `--install` flag that writes a registry
  `Run` key would address this.


---

See also:

- `how-to-use.md` — end-user guide
- `done.md` — build log and dependencies