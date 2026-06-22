# DiskSpy recovery steps

If the dashboard shows "No data yet" and the log contains
`failed to start ETW kernel trace: ... "Insufficient system resources exist to complete the requested service."`,
the Windows kernel ETW subsystem is in a degraded state.

## Why this happens

Windows allows a finite number of file-object registrations on the kernel
ETW logger. The first launches of DiskSpy on this machine ran with a debug
build that flooded the kernel with hundreds of thousands of `FileIo_Close`
events per minute from the `FILE_IO_INIT` provider. Each event consumed a
nonpaged-pool entry. After several such launches, the pool started returning
`ERROR_NO_SYSTEM_RESOURCES` (Win32 1450) on every subsequent `StartTraceW`
attempt, even with brand-new session names.

## The fix (in order)

### 1. Try a clean relaunch (may already be enough)

```powershell
# Elevated PowerShell
Get-Process diskspy -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Seconds 5
& "D:\Disk Spy\target\release\diskspy.exe" --background
```

Wait 10 seconds, then check the log:

```powershell
Get-Content "$env:LOCALAPPDATA\DiskSpy\diskspy.log.$(Get-Date -Format yyyy-MM-dd)" -Tail 5
```

You want to see:
```
INFO ETW kernel trace session started session=DiskSpy-KernelTrace-...
```

If you see `Insufficient system resources`, proceed to step 2.

### 2. Check for any other ETW consumers

Process Monitor, Windows Performance Recorder, and `xperf` all use the same
kernel ETW provider. Close them all.

```powershell
# Look for known ETW consumers
Get-Process procmon,procexp,xperf,wpr,UI0Detect -ErrorAction SilentlyContinue | Stop-Process -Force
```

### 3. Reboot (definitive fix)

A Windows reboot clears the kernel ETW registration table. After the reboot,
DiskSpy will start cleanly. To verify:

```powershell
# Right after login, in an elevated PowerShell:
& "D:\Disk Spy\target\release\diskspy.exe" --background
Get-Content "$env:LOCALAPPDATA\DiskSpy\diskspy.log.$(Get-Date -Format yyyy-MM-dd)" -Tail 5
```

You should see `INFO ETW kernel trace session started`. Then open
`http://localhost:7272` and write a test file:

```powershell
$bytes = New-Object byte[] 209715200
(New-Object Random).NextBytes($bytes)
[IO.File]::WriteAllBytes('C:\DiskSpy\test.bin', $bytes)
```

After 3 seconds (the `debounce_seconds` value), the dashboard's
"Recent Events" panel should show one row with `200 MB` for the file
`C:\DiskSpy\test.bin`.

### 4. After the system is healthy

The release binary at `D:\Disk Spy\target\release\diskspy.exe` is built
without the noisy `info!()` callback logs and without the `FILE_IO_INIT`
provider, so it consumes far fewer kernel resources. It will not put the
system back into the degraded state.

## What changed in the binary

| Change | Reason |
|---|---|
| Removed `FILE_IO_INIT` provider | It generated ~10x more events (open/cleanup/close), overwhelmed the nonpaged pool |
| Removed noisy `info!()` in callback | Blocking the ETW consumer thread caused buffer overruns and downstream `ERROR_NO_SYSTEM_RESOURCES` |
| Per-process unique session name | Avoids `ERROR_ALREADY_EXISTS` when a previous run was hard-killed |
| `stop_trace_by_name` retry only on `AlreadyExist` | Generic `IoError` is a real failure, retrying makes it worse |

## Diagnostic commands

```powershell
# Active ETW sessions (should be empty on a healthy system after diskspy is stopped)
logman query -ets

# Nonpaged pool usage (should be well below 1 GB on a healthy system)
(Get-Counter '\Memory\Pool Nonpaged Bytes').CounterSamples[0].CookedValue / 1MB
```

If the nonpaged pool is over 1 GB, the system is resource-starved. Reboot.
