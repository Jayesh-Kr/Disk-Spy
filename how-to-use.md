# How to Use DiskSpy

This guide walks through every step of using DiskSpy on a fresh machine,
from the first launch to configuring filters and inspecting the database.

---

## Table of contents

1. First launch
2. What you should see
3. The dashboard
4. Configuring DiskSpy
5. Inspecting the database directly
6. Stopping DiskSpy
7. Troubleshooting
8. Frequently asked questions

---

## 1. First launch

You need Administrator privileges to use DiskSpy, because the Windows
kernel's ETW tracing API requires them. There are two ways to get them,
and **two modes** the binary can run in.

### Modes

- **Console mode** (default): a black console window stays open showing
  live log output. Use this for development, debugging, or the first
  launch when you want to see what is happening.
- **Background / tray mode** (`--background` or `--tray`): the console
  window hides itself, all logs go to a rolling file, and a blue tray
  icon appears in the notification area. Use this for day-to-day use.

### Option A: background / tray mode (recommended)

After a one-time elevated launch, this is the mode you will use every
day.

1. Open File Explorer.
2. Navigate to `D:\Disk Spy\target\release`.
3. Double-click `diskspy.exe`.
4. Windows shows a UAC dialog. Click "Yes".
5. The console window briefly flashes and then disappears.
6. A blue tray icon appears in the notification area (you may need to
   expand the hidden icons arrow).

Right-click the tray icon for:

- Open Dashboard (in your browser)
- Show Log File (in your default text handler)
- Open Data Folder (in Explorer)
- Quit DiskSpy

### Option B: console mode

1. Press the Windows key, type "PowerShell", right-click "Windows
   PowerShell", and choose "Run as administrator".
2. In the elevated window, run:
   ```powershell
   cd "D:\Disk Spy"
   .\target\release\diskspy.exe
   ```

A console window appears and stays open. Press Ctrl+C to stop.

### What gets created on first launch

DiskSpy creates three files inside `%LOCALAPPDATA%\DiskSpy\`:

- `config.toml` - your editable configuration.
- `diskspy.db` - the SQLite database.
- `diskspy.log.YYYY-MM-DD` - the rolling log file (one per day).

The exact paths are printed at startup and returned by `/api/status`.

### Where is `%LOCALAPPDATA%`?

On a default Windows install, this is `C:\Users\<your-username>\AppData\Local\DiskSpy\`.
The `AppData` folder is hidden by default. The fastest way to open it:

1. Press Win+R, type `%LOCALAPPDATA%\DiskSpy`, press Enter.

Or in PowerShell:

```powershell
explorer "$env:LOCALAPPDATA\DiskSpy"
```

Either way, the console window will print a few lines of startup
information and then stay open. Do not close it.

On first launch, DiskSpy creates `%LOCALAPPDATA%\DiskSpy\` and three files
inside it:

- `config.toml` — your editable configuration.
- `diskspy.db` — the SQLite database.
- `diskspy.log.YYYY-MM-DD` — the rolling log file (one per day).

No files are ever created in the directory you launched from.

---

## 2. What you should see

Within a few seconds of launching, the console will print lines like
these:

```
2026-06-22T15:00:00.000Z  INFO DiskSpy v0.1.0 starting...
2026-06-22T15:00:00.005Z  INFO Configuration loaded port=7272 min_delta=51200 ...
2026-06-22T15:00:00.020Z  INFO ETW kernel trace session started: DiskSpy-KernelTrace
2026-06-22T15:00:00.025Z  INFO Dashboard available at http://localhost:7272
2026-06-22T15:00:00.030Z  INFO Monitoring C:\ (excluding 9 path patterns, 9 processes)
```

The exact wording may differ slightly. The four important lines are:

- "Configuration loaded" — your `config.toml` was read.
- "ETW kernel trace session started" — DiskSpy is now receiving kernel
  events.
- "Dashboard available at..." — the HTTP server is up.
- "Monitoring C:\\" — what DiskSpy is watching.

If you see "ERROR: DiskSpy requires Administrator privileges" instead,
DiskSpy is not running as Administrator. Close the window, re-launch
using Option A or B, and try again.

---

## 3. The dashboard

Open `http://localhost:7272` in any browser (Chrome, Edge, Firefox,
Safari — any of them). The page auto-refreshes every 10 seconds; you
do not need to reload manually.

You will see four panels stacked vertically.

### 3.1 Today's Top Space Consumers

The top-left panel. A horizontal bar chart ranking processes by the
number of bytes they added today. Color-coded:

- Blue: Docker
- Green: Node, npm, Cargo, Git
- Yellow: Python, pip, app installers
- Purple: Ollama
- Pink: Claude Desktop, WSL2
- Gray: anything else

Beneath the chart, a table repeats the same data with two extra
columns: the number of events and the largest single file that the
process touched.

### 3.2 30-Day Growth Timeline

The top-right panel. A stacked bar chart, one bar per day for the
last 30 days, each color representing one process. Hovering over a
bar shows the per-process breakdown. If you have only just started
running DiskSpy, most of the chart will be empty — DiskSpy only
knows about changes that happened while it was running.

### 3.3 Recent Events

The full-width panel below the charts. The 50 most recent debounced
change records. Columns: time, process, file path, and size change
(additions are green, deletions are red). Hovering over a truncated
file path shows the full path as a tooltip.

### 3.4 Largest Files Changed (last 7 days)

The bottom panel. The 50 files with the largest cumulative growth
over the last 7 days. Useful for spotting the next big thing to
clean up.

---

## 4. Configuring DiskSpy

Stop DiskSpy (Ctrl+C), edit `config.toml`, and restart it. The new
settings take effect on the next launch.

### 4.1 Change the dashboard port

Find the `[general]` section and change `dashboard_port`:

```toml
[general]
dashboard_port = 8080
```

Restart. The dashboard is now at `http://localhost:8080`.

### 4.2 Make DiskSpy less chatty

If the database is growing faster than you want, raise the threshold
or the debounce window:

```toml
[general]
# Only log events that are 500 KB or more.
min_delta_bytes = 512000
# Wait 10 seconds after the last write before committing a record.
debounce_seconds = 10
```

With these settings, a Docker pull that does many small writes will
result in far fewer database rows, at the cost of less precise
timestamps.

### 4.3 Exclude a noisy application

Suppose `electron.exe` is constantly updating its own cache and you do
not care. Find `[watch]` and add it to `exclude_processes`:

```toml
[watch]
exclude_processes = [
    "MsMpEng.exe",
    "SearchIndexer.exe",
    "svchost.exe",
    "System",
    "Registry",
    "MemCompression",
    "WmiPrvSE.exe",
    "RuntimeBroker.exe",
    "taskhostw.exe",
    "electron.exe",   # <-- new entry
]
```

Restart. The new process is no longer recorded.

### 4.4 Exclude a noisy directory

Suppose you do not want to see anything under your IDE's cache:

```toml
[watch]
exclude_paths = [
    # ... the defaults ...
    "C:\\Users\\%USERNAME%\\.cache\\",   # <-- new entry
]
```

`%USERNAME%` is replaced at runtime with the current user's name. Paths
are matched as a case-insensitive prefix.

### 4.5 Add a friendly label

If the dashboard is showing a process you do not recognize, add a
label. Find `[labels]` (or create it) and add a mapping:

```toml
[labels]
"msedge.exe" = "Microsoft Edge"
"OneDrive.exe" = "OneDrive"
"Spotify.exe" = "Spotify"
```

The next time a record comes in from any of these processes, the
dashboard shows the friendly name.

### 4.6 Change how long history is kept

The default is 90 days. To keep only the last 30 days:

```toml
[general]
retention_days = 30
```

On the next startup, any row older than 30 days is deleted.

---

## 5. Inspecting the database directly

`diskspy.db` is a regular SQLite database. You can open it with the
`sqlite3` CLI, with DB Browser for SQLite, or with any other tool that
understands SQLite.

### From the command line

```powershell
sqlite3 diskspy.db ".schema"
```

This shows the table structure. The two interesting tables are
`change_log` (one row per debounced file change) and `daily_summary`
(one row per day per process, pre-aggregated for fast queries).

### Useful queries

The ten most recent changes:

```sql
SELECT changed_at, process_label, file_path, delta_bytes
FROM change_log
ORDER BY changed_at DESC
LIMIT 10;
```

The top growers today:

```sql
SELECT process_label, SUM(delta_bytes) AS bytes_added, COUNT(*) AS events
FROM change_log
WHERE changed_at > strftime('%s', 'now', 'start of day')
  AND delta_bytes > 0
GROUP BY process_label
ORDER BY bytes_added DESC;
```

The largest single files by growth in the last week:

```sql
SELECT file_path, process_label, SUM(delta_bytes) AS total
FROM change_log
WHERE changed_at > strftime('%s', 'now', '-7 days')
GROUP BY file_path
ORDER BY total DESC
LIMIT 20;
```

The pre-aggregated daily summary (what the timeline chart reads):

```sql
SELECT date_str, process_label, total_bytes, event_count
FROM daily_summary
WHERE date_str >= date('now', '-30 days')
ORDER BY date_str ASC, total_bytes DESC;
```

### Backing up the database

The simplest approach: stop DiskSpy, copy `diskspy.db` somewhere safe,
restart DiskSpy. SQLite is durable across copies; the database file
is always in a consistent state thanks to WAL-mode-style atomic
transactions.

---

## 6. Stopping DiskSpy

### From the tray menu (background mode)

Right-click the tray icon and choose **Quit DiskSpy**. DiskSpy will:

1. Stop accepting new events from the kernel.
2. Flush any pending debouncer aggregations to the database.
3. Close the database connection.
4. Stop the HTTP server.
5. Print `DiskSpy stopped.` to the log file and exit.

This usually takes well under a second.

### From the console (console mode)

Click in the console window and press `Ctrl+C`. Same flush sequence as
above.

### Force-stop

If DiskSpy stops responding, you can force-kill it from Task Manager.
Any unflushed debouncer entries (typically zero if you wait more than
`debounce_seconds`) are lost, but the database itself remains
consistent.

---

## 7. Troubleshooting

### "ERROR: DiskSpy requires Administrator privileges"

The process is not running elevated. Use Option A or Option B from
section 1. The UAC manifest is embedded in the release binary, so
double-clicking should work.

### The dashboard never loads

Check that:

1. DiskSpy is still running (the console window is open).
2. The port is correct. The console prints "Dashboard available at
   http://localhost:..." — use exactly that URL.
3. No other application is using the port. If port 7272 is taken,
   change `dashboard_port` in `config.toml` and restart.

### "Access is denied" when starting the ETW session

Even if the process is running as Administrator, some system policies
(for example, certain hardened corporate configurations) block
kernel tracing. DiskSpy will print the underlying Win32 error code.
You will need to contact your IT administrator.

### No events appear in the dashboard

Wait at least `debounce_seconds + 1` after the last file write.
DiskSpy only commits a record after the file has been quiet for that
long. The default is 2 seconds, so wait at least 3 seconds.

If the dashboard still shows "No data yet" after a minute of normal
PC use, check the console for warnings. Common causes:

- A path is in `exclude_paths` that you did not expect.
- A process is in `exclude_processes` that you did not expect.
- The file you wrote is smaller than `min_delta_bytes`.

### The database file is huge

Lower `retention_days` and delete the current database, or
periodically archive old rows:

```sql
DELETE FROM change_log WHERE changed_at < strftime('%s', 'now', '-30 days');
VACUUM;
```

Then restart DiskSpy.

### CPU usage is higher than expected

The most common cause is the debouncer being overwhelmed by
extremely chatty processes. The 500 ms flush tick is the floor on
how quickly DiskSpy can respond. If you need finer control, raise
`min_delta_bytes` to filter out the noise.

### Memory usage keeps growing

The PID cache and the debouncer map are both bounded. If memory
keeps growing, the most likely cause is SQLite growth in the
`change_log` table. Apply retention.

---

## 8. Frequently asked questions

### Does DiskSpy slow down my computer?

No. ETW is designed for production use. The kernel provider runs
whether or not DiskSpy is listening, and DiskSpy itself uses under
0.5% of one CPU core at idle.

### Is my data sent anywhere?

No. DiskSpy has no network code beyond the local HTTP server. It
cannot phone home. The HTTP server only listens on localhost.

### Can I run multiple instances?

No. Only one ETW kernel session named `DiskSpy-KernelTrace` can be
active at a time. The second instance will fail to start the trace
and exit.

### Can I monitor other drives?

Yes. Add them to `drives` in `config.toml`:

```toml
[watch]
drives = ["C:\\", "D:\\", "E:\\"]
```

### Can I use this on a server?

Yes, as long as you can launch the binary elevated. There is no
interactive UI requirement; the dashboard is browser-based.

### Can I run it as a Windows service?

Not out of the box. The simplest path is a scheduled task that runs
elevated at logon. A `--install` flag that registers the binary as
a service is on the wishlist.

### What about macOS or Linux?

DiskSpy is Windows-only because it depends on ETW. The architecture
is portable in principle (FSEvents on macOS, inotify on Linux), but
that is a separate project.

### Where do I report bugs?

Open an issue in the project repository with:

- The version (printed at startup)
- A snippet of the console log
- Steps to reproduce

---

*End of user guide.*