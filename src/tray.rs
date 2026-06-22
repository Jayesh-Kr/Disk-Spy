//! System tray icon and headless-mode glue.
//!
//! DiskSpy can run either in the foreground (with a console window showing
//! logs) or in the background (no console, system tray icon only). The
//! `--background` / `--tray` CLI flag opts in to headless mode. In that
//! mode we hide the console window at startup, redirect all logging to a
//! rolling file under `%LOCALAPPDATA%\DiskSpy\diskspy.log`, and put a
//! tray icon in the notification area. The tray menu lets the user
//! open the dashboard in their browser, or quit DiskSpy cleanly.

use std::path::Path;

use anyhow::Result;
use tokio::sync::mpsc;
use tracing::{info, warn};

/// Messages the tray can send back to the main task.
#[derive(Debug, Clone)]
pub enum TrayMessage {
    OpenDashboard,
    ShowLogFile,
    OpenDataFolder,
    Quit,
}

/// Spawn the tray icon on its own OS thread. Returns a channel the main
/// task can poll for menu actions. Returns `Ok(None)` if tray creation
/// failed (e.g. no GUI subsystem available); callers should fall back to
/// console mode in that case.
pub fn spawn_tray() -> Result<Option<mpsc::Receiver<TrayMessage>>> {
    // Tray creation is done on a dedicated thread because the underlying
    // Win32 message loop must run on the thread that called into it.
    let (tx, rx) = mpsc::channel::<TrayMessage>(16);
    let result = std::thread::Builder::new()
        .name("diskspy-tray".into())
        .spawn(move || {
            if let Err(e) = run_tray(tx) {
                warn!(?e, "tray icon thread exited with error");
            }
        });

    match result {
        Ok(_) => Ok(Some(rx)),
        Err(e) => {
            warn!(?e, "failed to spawn tray thread");
            Ok(None)
        }
    }
}

fn run_tray(tx: mpsc::Sender<TrayMessage>) -> Result<()> {
    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::TrayIconBuilder;
    use tray_icon::TrayIconEvent;

    let open_item = MenuItem::new("Open Dashboard", true, None);
    let log_item = MenuItem::new("Show Log File", true, None);
    let data_item = MenuItem::new("Open Data Folder", true, None);
    let sep = PredefinedMenuItem::separator();
    let quit_item = MenuItem::new("Quit DiskSpy", true, None);

    let menu = Menu::new();
    menu.append_items(&[&open_item, &log_item, &data_item, &sep, &quit_item])?;

    // Use a small built-in icon (a 16x16 colored square) so we don't have
    // to ship a .ico file. Real product would embed a proper icon.
    let icon = build_simple_icon();

    let _tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("DiskSpy - monitoring")
        .with_icon(icon)
        .build()?;

    info!("tray icon installed");

    // Listen for menu clicks. This blocks until quit_item fires.
    let menu_channel = tray_icon::menu::MenuEvent::receiver();
    let icon_channel = tray_icon::TrayIconEvent::receiver();

    loop {
        if let Ok(event) = menu_channel.try_recv() {
            if let Err(e) = handle_menu_event(&event, &tx) {
                warn!(?e, "menu event handler error");
            }
        }
        if let Ok(_event) = icon_channel.try_recv() {
            // Default: left-click opens the dashboard.
            let _ = tx.blocking_send(TrayMessage::OpenDashboard);
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

fn handle_menu_event(
    event: &tray_icon::menu::MenuEvent,
    tx: &mpsc::Sender<TrayMessage>,
) -> Result<()> {
    let id = event.id.as_ref();
    // Match by the menu item's ID text. tray-icon assigns IDs from
    // the labels we gave, so we match on string.
    if id.contains("Open Dashboard") {
        let _ = tx.blocking_send(TrayMessage::OpenDashboard);
    } else if id.contains("Show Log") {
        let _ = tx.blocking_send(TrayMessage::ShowLogFile);
    } else if id.contains("Open Data") {
        let _ = tx.blocking_send(TrayMessage::OpenDataFolder);
    } else if id.contains("Quit") {
        let _ = tx.blocking_send(TrayMessage::Quit);
    }
    Ok(())
}

/// Build a 16x16 RGBA icon: a blue square with a darker border.
/// Inline-encoded so we don't need an .ico asset in the repo.
fn build_simple_icon() -> tray_icon::Icon {
    let size = 16u32;
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let border = x == 0 || y == 0 || x == size - 1 || y == size - 1;
            // Outer 2 px: dark blue. Inner: light blue.
            let in_corner_band =
                (x < 2 || x >= size - 2) || (y < 2 || y >= size - 2);
            let (r, g, b, a) = if border || in_corner_band {
                (30, 64, 175, 255) // dark blue
            } else {
                (96, 165, 250, 255) // light blue
            };
            rgba.extend_from_slice(&[r, g, b, a]);
        }
    }
    tray_icon::Icon::from_rgba(rgba, size, size).expect("icon encode")
}

/// Hide the current console window. Used in --background mode so the
/// user does not see a stray pop-up. No-op if no console exists
/// (e.g. compiled with `#![windows_subsystem = "windows"]`).
pub fn hide_console() {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::System::Console::GetConsoleWindow;
        use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE};
        let hwnd: HWND = GetConsoleWindow();
        if !hwnd.0.is_null() {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }
}

/// Show the current console window again (used by the tray "Show Log"
/// action if it ever grows into one).
#[allow(dead_code)]
pub fn show_console() {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::System::Console::GetConsoleWindow;
        use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_SHOW};
        let hwnd: HWND = GetConsoleWindow();
        if !hwnd.0.is_null() {
            let _ = ShowWindow(hwnd, SW_SHOW);
        }
    }
}

/// Open `path` in the default file handler (Explorer for folders, the
/// default app for files). Used by the tray menu.
pub fn open_in_default_app(path: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOW;
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let result = unsafe {
            ShellExecuteW(
                HWND(std::ptr::null_mut()),
                PCWSTR::from_raw(wide.as_ptr()),
                PCWSTR::from_raw(wide.as_ptr()),
                PCWSTR::from_raw(wide.as_ptr()),
                PCWSTR::from_raw(wide.as_ptr()),
                SW_SHOW,
            )
        };
        if result.0.is_null() {
            anyhow::bail!("ShellExecuteW returned null handle");
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Ok(())
    }
}
