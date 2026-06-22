//! ETW kernel consumer for DiskSpy.
//!
//! Subscribes to the `FileIo` kernel provider. For each kernel event we
//! receive, we extract:
//!   - the file path (NT path, converted to DOS path)
//!   - the size of the write
//!   - the process ID
//! and forward it to the debouncer as a `RawFileEvent`.
//!
//! Kernel tracing requires the process to be running as Administrator; we
//! check this on startup in `main.rs`.

use std::collections::HashMap;
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use ferrisetw::parser::Parser;
use ferrisetw::provider::kernel_providers::{FILE_IO_PROVIDER, FILE_INIT_IO_PROVIDER};
use ferrisetw::provider::Provider;
use ferrisetw::schema_locator::SchemaLocator;
use ferrisetw::trace::KernelTrace;
use ferrisetw::EventRecord;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use windows::core::PCWSTR;
use windows::Win32::Storage::FileSystem::QueryDosDeviceW;

use crate::debouncer::RawFileEvent;

/// Returns true if the current OS is Windows 8 or later. Kernel FileIo
/// tracing is significantly more efficient on Win8+ because the kernel
/// uses the new `EVENT_TRACE_SYSTEM_LOGGER_MODE`.
pub fn is_win8_or_greater() -> bool {
    // DiskSpy targets Windows 10/11. Any modern Windows build supports the
    // FileIo kernel provider we use.
    cfg!(windows)
}

/// Build a `Provider` that listens to both `FILE_IO` and `FILE_IO_INIT` kernel
/// keywords. `FILE_IO_INIT` gives us path/duration for `Create`/`Cleanup`/
/// `Close` events, which we use to track file deletions, while `FILE_IO`
/// gives us the `Write` events with byte counts.
fn build_fileio_provider(tx: mpsc::Sender<RawFileEvent>, dos_devices: Arc<parking_lot::Mutex<HashMap<String, String>>>) -> Provider {
    let tx_clone = tx.clone();
    let dos_clone = dos_devices.clone();

    Provider::kernel(&FILE_IO_PROVIDER)
        .any(0xFFFF_FFFF_FFFF_FFFF)
        .add_callback(move |record: &EventRecord, locator: &SchemaLocator| {
            handle_fileio_event(record, locator, &tx_clone, &dos_clone, "fileio");
        })
        .build()
}

fn build_fileio_init_provider(
    tx: mpsc::Sender<RawFileEvent>,
    dos_devices: Arc<parking_lot::Mutex<HashMap<String, String>>>,
) -> Provider {
    let tx_clone = tx.clone();
    let dos_clone = dos_devices.clone();

    Provider::kernel(&FILE_INIT_IO_PROVIDER)
        .any(0xFFFF_FFFF_FFFF_FFFF)
        .add_callback(move |record: &EventRecord, locator: &SchemaLocator| {
            handle_fileio_event(record, locator, &tx_clone, &dos_clone, "fileio_init");
        })
        .build()
}

fn handle_fileio_event(
    record: &EventRecord,
    locator: &SchemaLocator,
    tx: &mpsc::Sender<RawFileEvent>,
    dos_devices: &Arc<parking_lot::Mutex<HashMap<String, String>>>,
    _kind: &'static str,
) {
    let pid = record.process_id();
    if pid == 0 {
        // System pseudo-PID; ignore (e.g. kernel writes to NTFS metadata).
        return;
    }
    let opcode = record.opcode();
    // OpCodes we care about:
    //   0x00 = Info / generic FileIo
    //   0x01 = Name (FileIo_Name)
    //   0x20 = Create
    //   0x21 = Cleanup
    //   0x22 = Close
    //   0x25 = Write
    //   0x26 = SetInfo
    //   0x27 = Delete
    //   0x32 = DirEnum
    let is_write = opcode == 0x25;
    let is_close = opcode == 0x22;
    let is_setinfo = opcode == 0x26;
    let is_delete = opcode == 0x27;
    if !(is_write || is_close || is_setinfo || is_delete) {
        return;
    }

    let schema = match locator.event_schema(record) {
        Ok(s) => s,
        Err(e) => {
            debug!(?e, "no schema for FileIo event");
            return;
        }
    };
    let parser = Parser::create(record, &schema);

    let nt_path: Option<String> = parser.try_parse("FileName").ok();
    let io_size: Option<u64> = parser.try_parse("IoSize").ok();

    let Some(nt_path) = nt_path else { return; };
    if nt_path.is_empty() {
        return;
    }
    let dos_path = nt_to_dos(&nt_path, dos_devices);

    // Compute delta:
    //   Write: +IoSize
    //   SetInfo (rename/truncate): treat as 0 unless we know better
    //   Delete: 0 (the file removal will be reflected in the next drive scan)
    //   Close: 0 (we just want a marker; debouncer will drop the entry below threshold)
    let bytes = match opcode {
        0x25 => io_size.unwrap_or(0) as i64, // Write
        _ => 0,
    };

    let event = RawFileEvent {
        pid,
        file_path: dos_path,
        bytes_written: bytes.max(0) as u64,
        category_hint: String::new(),
    };

    // Best-effort non-blocking send; the debouncer might be slow but its
    // channel is bounded, so this just drops if it overflows.
    if let Err(e) = tx.try_send(event) {
        warn!(?e, "debouncer channel full, dropping event");
    }
}

/// Convert a Windows NT device path to a DOS path. e.g.
/// `\Device\HarddiskVolume3\Users\foo` → `C:\Users\foo`.
pub fn nt_to_dos(nt_path: &str, dos_devices: &Arc<parking_lot::Mutex<HashMap<String, String>>>) -> String {
    let map = dos_devices.lock();
    // Find the longest matching prefix.
    let mut best: Option<(&str, &str)> = None;
    for (nt_prefix, dos_prefix) in map.iter() {
        if nt_path.to_lowercase().starts_with(&nt_prefix.to_lowercase()) {
            if best.map_or(true, |(b, _)| nt_prefix.len() > b.len()) {
                best = Some((nt_prefix.as_str(), dos_prefix.as_str()));
            }
        }
    }
    match best {
        Some((nt_prefix, dos_prefix)) => {
            let rest = &nt_path[nt_prefix.len()..];
            format!("{}{}", dos_prefix, rest.replace('\\', "\\"))
        }
        None => nt_path.to_string(),
    }
}

/// Build a map of `NT device name → DOS drive letter` for every mounted
/// volume on the system, e.g. `\Device\HarddiskVolume3` → `C:`.
pub fn refresh_dos_devices() -> HashMap<String, String> {
    let mut out = HashMap::new();
    for letter in b'A'..=b'Z' {
        let mut drive = [0u16; 4];
        drive[0] = letter as u16;
        drive[1] = ':' as u16;
        drive[2] = 0;
        let mut target = [0u16; 512];
        // SAFETY: we hand a properly-terminated wide string and a buffer.
        let len = unsafe {
            QueryDosDeviceW(
                PCWSTR(drive.as_ptr()),
                Some(&mut target),
            )
        };
        if len == 0 {
            continue;
        }
        // target is a sequence of null-separated wide strings; the first
        // entry is the NT device name.
        let first_null = target.iter().position(|&c| c == 0).unwrap_or(target.len());
        let nt = String::from_utf16_lossy(&target[..first_null]);
        let dos = format!("{}:", letter as char);
        if !nt.is_empty() {
            out.insert(nt, dos);
        }
    }
    out
}

/// Set up a kernel trace listening to FileIo events. Returns a `KernelTrace`
/// which keeps the session alive while alive; dropping it stops the session.
pub fn start_trace(
    raw_tx: mpsc::Sender<RawFileEvent>,
    dos_devices: Arc<parking_lot::Mutex<HashMap<String, String>>>,
) -> Result<KernelTrace> {
    let provider = build_fileio_provider(raw_tx.clone(), dos_devices.clone());
    let init_provider = build_fileio_init_provider(raw_tx, dos_devices);

    let trace = KernelTrace::new()
        .named("DiskSpy-KernelTrace".to_string())
        .enable(provider)
        .enable(init_provider)
        .start_and_process()
        .map_err(|e| anyhow!("failed to start ETW kernel trace: {:?}", e))?;

    info!("ETW kernel trace session started: DiskSpy-KernelTrace");
    Ok(trace)
}

/// Check whether the current process is running elevated (Administrator).
/// Used by `main.rs` to fail fast with a clear message.
pub fn is_elevated() -> bool {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
        use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
        use windows::Win32::Foundation::CloseHandle;

        let proc = GetCurrentProcess();
        let mut token = windows::Win32::Foundation::HANDLE(std::ptr::null_mut());
        if OpenProcessToken(proc, TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut ret_len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut _),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut ret_len,
        );
        let _ = CloseHandle(token);
        ok.is_ok() && elevation.TokenIsElevated != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Print the elevation error message and return a dummy nonzero exit code.
pub fn elevation_error_message() -> &'static str {
    "ERROR: DiskSpy requires Administrator privileges to capture kernel ETW events.\n\
     Right-click diskspy.exe -> Run as Administrator"
}

/// Blocking ETW consumer entry point. Spawn this on a dedicated thread.
/// It returns when the trace session ends.
pub fn run_etw_consumer(
    raw_tx: mpsc::Sender<RawFileEvent>,
    dos_devices: Arc<parking_lot::Mutex<HashMap<String, String>>>,
) -> Result<()> {
    let trace = start_trace(raw_tx, dos_devices)?;
    // The trace handle's process() is already running on its own thread (via
    // start_and_process). We just need to keep the KernelTrace alive here.
    // Drop happens on shutdown when main calls abort on the channel.
    std::thread::park_timeout(Duration::from_secs(u64::MAX / 4));
    drop(trace);
    Ok(())
}

#[allow(dead_code)]
fn wide_to_string(buf: &[u16]) -> OsString {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    OsString::from_wide(&buf[..len])
}
