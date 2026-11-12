// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

/// Whether a desktop window can be shown.
///
/// `station` is the Windows window-station probe (`Ok(visible)` or the API error); only
/// consulted on Windows, where an error means **no display**. macOS: always. Elsewhere:
/// `DISPLAY` or `WAYLAND_DISPLAY` non-empty. Returns the verdict and, when a probe failed,
/// a note for the log.
pub fn display_available(
    os: &str,
    display: Option<&str>,
    wayland: Option<&str>,
    station: Option<Result<bool, String>>,
) -> (bool, Option<String>) {
    match os {
        "windows" => match station {
            Some(Ok(visible)) => (visible, None),
            Some(Err(e)) => (false, Some(format!("{e}; assuming no display"))),
            None => (
                false,
                Some("window-station probe not run; assuming no display".to_owned()),
            ),
        },
        "macos" => (true, None),
        _ => (
            display.is_some_and(|v| !v.is_empty()) || wayland.is_some_and(|v| !v.is_empty()),
            None,
        ),
    }
}

/// `WSF_VISIBLE` on the process window station: set only on the interactive station of a
/// desktop session, not for services, session 0 or most remote shells.
#[cfg(windows)]
pub fn window_station_visible() -> Result<bool, String> {
    use std::ffi::c_void;

    #[repr(C)]
    struct UserObjectFlags {
        f_inherit: i32,
        f_reserved: i32,
        dw_flags: u32,
    }

    #[link(name = "user32")]
    extern "system" {
        fn GetProcessWindowStation() -> *mut c_void;
        fn GetUserObjectInformationW(
            h_obj: *mut c_void,
            n_index: i32,
            pv_info: *mut c_void,
            n_length: u32,
            lpn_length_needed: *mut u32,
        ) -> i32;
    }

    const UOI_FLAGS: i32 = 1;
    const WSF_VISIBLE: u32 = 0x0001;

    // SAFETY: GetProcessWindowStation takes no arguments and returns a borrowed handle (or
    let station = unsafe { GetProcessWindowStation() };
    if station.is_null() {
        return Err(format!(
            "GetProcessWindowStation failed ({})",
            std::io::Error::last_os_error()
        ));
    }
    let mut flags = UserObjectFlags {
        f_inherit: 0,
        f_reserved: 0,
        dw_flags: 0,
    };
    let mut needed = 0u32;
    // SAFETY: `station` is a valid window-station handle; `flags` is a live, writable
    let ok = unsafe {
        GetUserObjectInformationW(
            station,
            UOI_FLAGS,
            (&mut flags as *mut UserObjectFlags).cast::<c_void>(),
            std::mem::size_of::<UserObjectFlags>() as u32,
            &mut needed,
        )
    };
    if ok == 0 {
        return Err(format!(
            "GetUserObjectInformationW failed ({})",
            std::io::Error::last_os_error()
        ));
    }
    Ok(flags.dw_flags & WSF_VISIBLE != 0)
}

#[cfg(not(windows))]
pub fn window_station_visible() -> Result<bool, String> {
    Err("window stations exist only on Windows".to_owned())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewerOutcome {
    Exited(i32),
    Cancelled,
    Failed(String),
}

pub fn run_viewer(
    cmd: &mut std::process::Command,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> ViewerOutcome {
    crate::outputs::hide_console_window(cmd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return ViewerOutcome::Failed(e.to_string()),
    };
    loop {
        if cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::SeqCst)) {
            let _ = child.kill();
            let _ = child.wait();
            return ViewerOutcome::Cancelled;
        }
        match child.try_wait() {
            Ok(Some(s)) => return ViewerOutcome::Exited(s.code().unwrap_or(-1)),
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(100)),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return ViewerOutcome::Failed(e.to_string());
            }
        }
    }
}

pub fn has_display() -> (bool, Option<String>) {
    let display = std::env::var("DISPLAY").ok();
    let wayland = std::env::var("WAYLAND_DISPLAY").ok();
    let os = std::env::consts::OS;
    let station = (os == "windows").then(window_station_visible);
    display_available(os, display.as_deref(), wayland.as_deref(), station)
}

#[cfg(test)]
mod display_tests {
    use super::display_available;

    #[test]
    fn windows_follows_the_window_station() {
        assert!(display_available("windows", None, None, Some(Ok(true))).0);
        assert!(!display_available("windows", Some(":0"), None, Some(Ok(false))).0);
    }

    #[test]
    fn windows_probe_failure_is_no_display_with_a_note() {
        let (ok, note) = display_available("windows", None, None, Some(Err("boom".into())));
        assert!(!ok);
        assert!(note.is_some_and(|n| n.contains("assuming no display")));
        assert!(!display_available("windows", None, None, None).0);
    }

    #[test]
    fn macos_needs_no_display_variable() {
        assert!(display_available("macos", None, None, None).0);
    }

    #[test]
    fn linux_follows_display_variables() {
        assert!(!display_available("linux", None, None, None).0);
        assert!(!display_available("linux", Some(""), None, None).0);
        assert!(display_available("linux", Some(":0"), None, None).0);
        assert!(display_available("linux", None, Some("wayland-0"), None).0);
    }

    #[cfg(windows)]
    #[test]
    fn the_test_process_has_a_readable_window_station() {
        assert!(super::window_station_visible().is_ok());
    }
}
