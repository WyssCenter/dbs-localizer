// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! The few Win32 calls the application needs outside egui: the parent console for a
//! windows-subsystem release build, a message box when the window cannot be created, the
//! default browser / Settings for the driver help, and the display adapters' hardware IDs.
//! Every function is a no-op or empty result on other platforms.

use std::time::Duration;

#[cfg(windows)]
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Attach the process to the console it was launched from, so a release build (windows
/// subsystem, no console of its own) still prints to a terminal. Nothing happens when there is
/// no parent console or the process already has one.
pub fn attach_parent_console() {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
        // SAFETY: AttachConsole takes a process id constant and touches no memory of ours.
        unsafe {
            AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
}

/// A modal error box with the full message; prints to stderr on other platforms.
pub fn error_box(title: &str, text: &str) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            MessageBoxW, MB_ICONERROR, MB_OK, MB_SETFOREGROUND,
        };
        let title = wide(title);
        let text = wide(text);
        // SAFETY: both strings are NUL-terminated UTF-16 buffers that outlive the call.
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                text.as_ptr(),
                title.as_ptr(),
                MB_OK | MB_ICONERROR | MB_SETFOREGROUND,
            );
        }
    }
    #[cfg(not(windows))]
    {
        eprintln!("{title}: {text}");
    }
}

#[cfg(windows)]
struct WindowSearch {
    title: Vec<u16>,
    process: u32,
    found: windows_sys::Win32::Foundation::HWND,
}

#[cfg(windows)]
unsafe extern "system" fn match_own_window(
    hwnd: windows_sys::Win32::Foundation::HWND,
    param: windows_sys::Win32::Foundation::LPARAM,
) -> i32 {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
    };
    // SAFETY: `param` is the address of the `WindowSearch` that `EnumWindows` was given and
    // that outlives the enumeration; the text buffer length is passed with the buffer.
    unsafe {
        let search = &mut *(param as *mut WindowSearch);
        let mut process = 0u32;
        GetWindowThreadProcessId(hwnd, &mut process);
        if process != search.process || IsWindowVisible(hwnd) == 0 {
            return 1;
        }
        let mut buf = vec![0u16; search.title.len() + 1];
        let n = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
        if n > 0 && buf[..n as usize] == search.title[..] {
            search.found = hwnd;
            return 0;
        }
        1
    }
}

pub fn capture_window(title: &str) -> Option<(usize, usize, Vec<u8>)> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{POINT, RECT};
        use windows_sys::Win32::Graphics::Gdi::{
            BitBlt, ClientToScreen, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC,
            DeleteObject, GetDC, GetDIBits, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER,
            BI_RGB, DIB_RGB_COLORS, SRCCOPY,
        };
        use windows_sys::Win32::Storage::Xps::{PrintWindow, PW_CLIENTONLY};
        use windows_sys::Win32::System::Threading::GetCurrentProcessId;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            EnumWindows, GetClientRect, SetForegroundWindow,
        };
        const PW_RENDERFULLCONTENT: u32 = 2;
        let mut search = WindowSearch {
            title: title.encode_utf16().collect(),
            process: unsafe { GetCurrentProcessId() },
            found: std::ptr::null_mut(),
        };
        // SAFETY: every handle is checked before use and released on all paths; the buffers
        // passed to the GDI calls are sized from the client rectangle they describe, and the
        // bitmap is deselected from the memory device context before its pixels are read.
        unsafe {
            EnumWindows(
                Some(match_own_window),
                &mut search as *mut WindowSearch as isize,
            );
            let hwnd = search.found;
            if hwnd.is_null() {
                return None;
            }
            SetForegroundWindow(hwnd);
            std::thread::sleep(Duration::from_millis(400));
            let mut rect: RECT = std::mem::zeroed();
            if GetClientRect(hwnd, &mut rect) == 0 {
                return None;
            }
            let width = (rect.right - rect.left).max(0);
            let height = (rect.bottom - rect.top).max(0);
            if width == 0 || height == 0 {
                return None;
            }
            let mut origin = POINT { x: 0, y: 0 };
            if ClientToScreen(hwnd, &mut origin) == 0 {
                return None;
            }
            let screen = GetDC(std::ptr::null_mut());
            if screen.is_null() {
                return None;
            }
            let memory = CreateCompatibleDC(screen);
            let bitmap = CreateCompatibleBitmap(screen, width, height);
            let mut pixels = Vec::new();
            if !memory.is_null() && !bitmap.is_null() {
                let previous = SelectObject(memory, bitmap);
                let mut copied =
                    PrintWindow(hwnd, memory, PW_CLIENTONLY | PW_RENDERFULLCONTENT) != 0;
                if !copied {
                    copied = BitBlt(
                        memory, 0, 0, width, height, screen, origin.x, origin.y, SRCCOPY,
                    ) != 0;
                }
                SelectObject(memory, previous);
                if copied {
                    let mut info: BITMAPINFO = std::mem::zeroed();
                    info.bmiHeader = BITMAPINFOHEADER {
                        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                        biWidth: width,
                        biHeight: -height,
                        biPlanes: 1,
                        biBitCount: 32,
                        biCompression: BI_RGB,
                        biSizeImage: 0,
                        biXPelsPerMeter: 0,
                        biYPelsPerMeter: 0,
                        biClrUsed: 0,
                        biClrImportant: 0,
                    };
                    pixels = vec![0u8; width as usize * height as usize * 4];
                    let rows = GetDIBits(
                        memory,
                        bitmap,
                        0,
                        height as u32,
                        pixels.as_mut_ptr().cast(),
                        &mut info,
                        DIB_RGB_COLORS,
                    );
                    if rows == 0 {
                        pixels.clear();
                    }
                }
            }
            if !bitmap.is_null() {
                DeleteObject(bitmap);
            }
            if !memory.is_null() {
                DeleteDC(memory);
            }
            ReleaseDC(std::ptr::null_mut(), screen);
            if pixels.is_empty() {
                return None;
            }
            for px in pixels.as_chunks_mut::<4>().0 {
                px.swap(0, 2);
                px[3] = 255;
            }
            Some((width as usize, height as usize, pixels))
        }
    }
    #[cfg(not(windows))]
    {
        let _ = title;
        None
    }
}

/// Open a web address or a `ms-settings:` page with whatever handles it.
pub fn open_url(url: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        let verb = wide("open");
        let target = wide(url);
        // SAFETY: NUL-terminated UTF-16 buffers that outlive the call; null window, parameters
        let code = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                target.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        } as usize;
        if code > 32 {
            Ok(())
        } else {
            Err(format!(
                "Windows could not open {url} (ShellExecute code {code})"
            ))
        }
    }
    #[cfg(not(windows))]
    {
        Err(format!("opening {url} is only wired on Windows"))
    }
}

/// The Plug and Play hardware IDs of the display adapters (`PCI\VEN_10DE&DEV_…`), read from
/// the device tree with SetupAPI, so a card without any driver is still identified; a hidden
/// PowerShell query of Windows Management Instrumentation is the fallback. Empty after
/// `timeout`, on another platform or when both ways fail.
pub fn display_adapter_hardware_ids(timeout: Duration) -> Vec<String> {
    #[cfg(windows)]
    {
        let ids = setupapi_display_hardware_ids();
        if !ids.is_empty() {
            return ids;
        }
        powershell_display_hardware_ids(timeout)
    }
    #[cfg(not(windows))]
    {
        let _ = timeout;
        Vec::new()
    }
}

#[cfg(windows)]
fn setupapi_display_hardware_ids() -> Vec<String> {
    use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
        SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiGetClassDevsW,
        SetupDiGetDeviceRegistryPropertyW, DIGCF_PRESENT, GUID_DEVCLASS_DISPLAY, SPDRP_HARDWAREID,
        SP_DEVINFO_DATA,
    };
    const INVALID: isize = -1;
    // SAFETY: a class GUID that outlives the call, no enumerator, no parent window.
    let set = unsafe {
        SetupDiGetClassDevsW(
            &GUID_DEVCLASS_DISPLAY,
            std::ptr::null(),
            std::ptr::null_mut(),
            DIGCF_PRESENT,
        )
    };
    if set == INVALID {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut index = 0u32;
    loop {
        let mut data = SP_DEVINFO_DATA {
            cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32,
            ClassGuid: GUID_DEVCLASS_DISPLAY,
            DevInst: 0,
            Reserved: 0,
        };
        // SAFETY: `set` is a live device information set and `data` a correctly sized record.
        if unsafe { SetupDiEnumDeviceInfo(set, index, &mut data) } == 0 {
            break;
        }
        index += 1;
        let mut buf = vec![0u16; 4096];
        let mut needed = 0u32;
        // SAFETY: the buffer length in bytes is passed with the buffer; the property is a
        let ok = unsafe {
            SetupDiGetDeviceRegistryPropertyW(
                set,
                &data,
                SPDRP_HARDWAREID,
                std::ptr::null_mut(),
                buf.as_mut_ptr().cast(),
                (buf.len() * 2) as u32,
                &mut needed,
            )
        };
        if ok == 0 {
            continue;
        }
        let n = (needed as usize / 2).min(buf.len());
        let first: Vec<u16> = buf[..n].iter().copied().take_while(|&c| c != 0).collect();
        if !first.is_empty() {
            out.push(String::from_utf16_lossy(&first));
        }
    }
    // SAFETY: `set` came from SetupDiGetClassDevsW and is not used after this.
    unsafe {
        SetupDiDestroyDeviceInfoList(set);
    }
    out
}

#[cfg(windows)]
fn powershell_display_hardware_ids(timeout: Duration) -> Vec<String> {
    use std::io::Read;
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut child = match Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            "Get-CimInstance Win32_VideoController | ForEach-Object { $_.PNPDeviceID }",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let Some(mut out) = child.stdout.take() else {
        let _ = child.kill();
        return Vec::new();
    };
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = out.read_to_string(&mut text);
        text
    });
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(50));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
        }
    }
    let text = reader.join().unwrap_or_default();
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn both_display_adapter_lookups_agree_on_the_first_pci_vendor() {
        let started = std::time::Instant::now();
        let direct = setupapi_display_hardware_ids();
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "SetupAPI must be instant"
        );
        let shell = powershell_display_hardware_ids(Duration::from_secs(30));
        for id in direct.iter().chain(&shell) {
            eprintln!("display adapter: {id}");
            assert_eq!(id, id.trim());
            assert!(!id.is_empty());
        }
        let vendor = |ids: &[String]| {
            ids.iter().find_map(|id| {
                id.to_ascii_uppercase()
                    .find("VEN_")
                    .map(|at| id[at..at + 8].to_ascii_uppercase())
            })
        };
        if let (Some(a), Some(b)) = (vendor(&direct), vendor(&shell)) {
            assert_eq!(a, b);
        }
        assert_eq!(
            display_adapter_hardware_ids(Duration::from_secs(30)).is_empty(),
            direct.is_empty() && shell.is_empty()
        );
    }
}
