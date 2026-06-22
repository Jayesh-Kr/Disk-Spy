//! PID → process name cache.
//!
//! Windows reuses PIDs. A PID that pointed at `docker.exe` a minute ago may
//! now point at `notepad.exe`. We cache every resolved (pid → name) pair for a
//! short TTL and re-query when the entry expires.

use std::time::{Duration, Instant};

use dashmap::DashMap;

#[cfg(windows)]
mod imp {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
    };
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::TH32CS_SNAPPROCESS;

    /// Walk the system snapshot to find the basename for `pid`.
    pub(super) fn resolve_pid(pid: u32) -> String {
        unsafe {
            let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
                return format!("unknown:{}", pid);
            };
            if snapshot.is_invalid() {
                return format!("unknown:{}", pid);
            }

            let mut entry = PROCESSENTRY32W {
                dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
                ..Default::default()
            };

            let mut found: Option<String> = None;

            if Process32FirstW(snapshot, &mut entry).is_ok() {
                loop {
                    if entry.th32ProcessID == pid {
                        let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
                        let name = OsString::from_wide(&entry.szExeFile[..len]);
                        if let Some(s) = name.to_str() {
                            found = Some(s.to_string());
                        }
                        break;
                    }
                    if Process32NextW(snapshot, &mut entry).is_err() {
                        break;
                    }
                }
            }

            let _ = CloseHandle(snapshot);

            match found {
                Some(name) => {
                    // Extract just the basename (strip any path prefix).
                    let basename = std::path::Path::new(&name)
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or(&name)
                        .to_string();
                    basename
                }
                None => {
                    // Process may have exited between event and snapshot.
                    // Verify the PID exists by opening it.
                    if let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
                        if !handle.is_invalid() {
                            let _ = CloseHandle(handle);
                            // It exists but the snapshot didn't include it (rare race).
                            format!("unknown:{}", pid)
                        } else {
                            format!("exited:{}", pid)
                        }
                    } else {
                        format!("exited:{}", pid)
                    }
                }
            }
        }
    }
}

#[cfg(not(windows))]
mod imp {
    pub(super) fn resolve_pid(pid: u32) -> String {
        format!("pid:{}", pid)
    }
}

/// A bounded TTL cache of `(pid → process_name)` mappings.
pub struct ProcessCache {
    inner: DashMap<u32, (String, Instant)>,
    ttl: Duration,
}

impl ProcessCache {
    pub fn new(ttl: Duration) -> Self {
        Self { inner: DashMap::new(), ttl }
    }

    /// Resolve a PID to its current process basename.
    /// Returns a fallback name when the process has exited or cannot be queried.
    pub fn resolve(&self, pid: u32) -> String {
        if let Some(entry) = self.inner.get(&pid) {
            if entry.value().1.elapsed() < self.ttl {
                return entry.value().0.clone();
            }
        }
        let name = imp::resolve_pid(pid);
        self.inner.insert(pid, (name.clone(), Instant::now()));
        name
    }

    /// Drop stale entries. Called periodically to keep the map from growing.
    pub fn evict_expired(&self) {
        self.inner.retain(|_, (_, ts)| ts.elapsed() < self.ttl);
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process;

    #[test]
    fn resolve_self_pid() {
        let cache = ProcessCache::new(Duration::from_secs(30));
        let pid = process::id();
        let name = cache.resolve(pid);
        // We are running inside `cargo test`, so we are the test harness exe.
        // Accept any non-fallback answer (cargo, diskspy, or test runner).
        assert!(!name.starts_with("unknown:"), "got {}", name);
        assert!(!name.starts_with("exited:"), "got {}", name);
        assert!(!name.is_empty());
    }

    #[test]
    fn unknown_pid_returns_fallback() {
        let cache = ProcessCache::new(Duration::from_secs(30));
        // Pick a PID that almost certainly doesn't exist.
        let name = cache.resolve(0x7FFFFFFE);
        assert!(
            name.starts_with("unknown:") || name.starts_with("exited:") || name == "System",
            "got {}",
            name
        );
    }

    #[test]
    fn cache_returns_same_name_within_ttl() {
        let cache = ProcessCache::new(Duration::from_secs(30));
        let pid = process::id();
        let a = cache.resolve(pid);
        let b = cache.resolve(pid);
        assert_eq!(a, b);
        assert_eq!(cache.len(), 1);
    }
}