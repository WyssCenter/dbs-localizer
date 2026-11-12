// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock, TryLockError};
use std::thread::ThreadId;
use std::time::{SystemTime, UNIX_EPOCH};

use eframe::egui;

const KEEP_FILES: usize = 20;
const SIZE_CAP: u64 = 50 * 1024 * 1024;
const FILE_PREFIX: &str = "dbs-localizer-";
const FILE_SUFFIX: &str = ".log";
const END_MARKER: &str = "session ended normally";
const TAIL_BYTES: u64 = 8192;
const LIMIT_LINE: &str =
    "log size limit reached; only warnings and errors are recorded from now on";
const HARD_LIMIT_LINE: &str =
    "log hard size limit reached; only a crash report and the end of the session are recorded";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Info,
    Warn,
    Error,
    Panic,
}

impl Level {
    fn label(self) -> &'static str {
        match self {
            Level::Info => "INFO",
            Level::Warn => "WARN",
            Level::Error => "ERROR",
            Level::Panic => "PANIC",
        }
    }
}

struct Logger {
    file: File,
    written: u64,
    cap: u64,
    capped: bool,
    hard_capped: bool,
}

impl Logger {
    fn new(file: File, cap: u64) -> Self {
        Self {
            file,
            written: 0,
            cap,
            capped: false,
            hard_capped: false,
        }
    }

    fn write(&mut self, level: Level, message: &str) {
        if self.written >= self.cap.saturating_mul(2) {
            if level == Level::Panic {
                self.emit(level, message);
            } else if !self.hard_capped {
                self.hard_capped = true;
                self.emit(Level::Warn, HARD_LIMIT_LINE);
            }
            return;
        }
        if self.written >= self.cap {
            if !self.capped {
                self.capped = true;
                self.emit(Level::Warn, LIMIT_LINE);
            }
            if level < Level::Warn {
                return;
            }
        }
        self.emit(level, message);
    }

    fn finish(&mut self) {
        self.emit(Level::Info, END_MARKER);
    }

    fn emit(&mut self, level: Level, message: &str) {
        let line = format_line(&timestamp(SystemTime::now()), level, message);
        if self.file.write_all(line.as_bytes()).is_ok() {
            self.written += line.len() as u64;
        }
        let _ = self.file.flush();
    }
}

static LOGGER: OnceLock<Mutex<Logger>> = OnceLock::new();
static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();
static MAIN_THREAD: OnceLock<ThreadId> = OnceLock::new();
static PREVIOUS_UNCLEAN: AtomicBool = AtomicBool::new(false);
static NOTICE_DISMISSED: AtomicBool = AtomicBool::new(false);
static UI_ERRORS: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

pub fn report_ui_error(title: &str, body: &str) {
    let mut queue = UI_ERRORS.lock().unwrap_or_else(|p| p.into_inner());
    queue.push((title.to_owned(), body.to_owned()));
}

pub fn take_ui_errors() -> Vec<(String, String)> {
    let mut queue = UI_ERRORS.lock().unwrap_or_else(|p| p.into_inner());
    std::mem::take(&mut *queue)
}

fn days_to_civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (year + i64::from(month <= 2), month, day)
}

struct Utc {
    year: i64,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
    millis: u32,
}

fn utc(time: SystemTime) -> Utc {
    let since = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = since.as_secs() as i64;
    let (year, month, day) = days_to_civil(secs.div_euclid(86_400));
    let rem = secs.rem_euclid(86_400) as u32;
    Utc {
        year,
        month,
        day,
        hour: rem / 3600,
        minute: rem % 3600 / 60,
        second: rem % 60,
        millis: since.subsec_millis(),
    }
}

pub fn timestamp(time: SystemTime) -> String {
    let t = utc(time);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        t.year, t.month, t.day, t.hour, t.minute, t.second, t.millis
    )
}

fn file_stamp(time: SystemTime) -> String {
    let t = utc(time);
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        t.year, t.month, t.day, t.hour, t.minute, t.second
    )
}

pub fn format_line(timestamp: &str, level: Level, message: &str) -> String {
    let mut out = String::with_capacity(message.len() + 40);
    for (i, line) in message.lines().enumerate() {
        if i == 0 {
            let _ = writeln!(out, "{timestamp} {} {line}", level.label());
        } else {
            let _ = writeln!(out, "    {line}");
        }
    }
    if out.is_empty() {
        let _ = writeln!(out, "{timestamp} {}", level.label());
    }
    out
}

pub fn level_from_line(line: &str) -> Level {
    let trimmed = line.trim_start();
    let trimmed = trimmed
        .strip_prefix("[out]")
        .or_else(|| trimmed.strip_prefix("[err]"))
        .unwrap_or(trimmed)
        .trim_start();
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("error") || trimmed.starts_with('✖') {
        Level::Error
    } else if lower.starts_with("warn") || trimmed.starts_with('⚠') {
        Level::Warn
    } else {
        Level::Info
    }
}

fn is_log_name(name: &str) -> bool {
    name.starts_with(FILE_PREFIX) && name.ends_with(FILE_SUFFIX)
}

fn log_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(is_log_name)
        })
        .collect();
    files.sort();
    files
}

pub fn rotate(dir: &Path, keep: usize) {
    let files = log_files(dir);
    let excess = files.len().saturating_sub(keep);
    for path in files.into_iter().take(excess) {
        let _ = std::fs::remove_file(path);
    }
}

pub fn ended_normally(contents: &str) -> bool {
    contents
        .lines()
        .rev()
        .take(4)
        .any(|l| l.contains(END_MARKER))
}

fn read_tail(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(TAIL_BYTES)))
        .ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

fn held_by_live_session(path: &Path) -> bool {
    File::open(path).is_ok_and(|f| matches!(f.try_lock(), Err(std::fs::TryLockError::WouldBlock)))
}

pub fn previous_session_unclean(dir: &Path) -> bool {
    let Some(newest) = log_files(dir)
        .into_iter()
        .rev()
        .find(|p| !held_by_live_session(p))
    else {
        return false;
    };
    match read_tail(&newest) {
        Some(tail) => !ended_normally(&tail),
        None => false,
    }
}

fn log_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("DBSLOC_LOG_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    if crate::app::screenshot_mode() {
        return None;
    }
    crate::app::config_base_dir().map(|b| b.join("dbsloc").join("logs"))
}

pub fn init() {
    let _ = MAIN_THREAD.set(std::thread::current().id());
    install_panic_hook();
    let Some(dir) = log_dir() else {
        return;
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!(
            "dbs-localizer: cannot create the log folder {}: {e}",
            dir.display()
        );
        return;
    }
    PREVIOUS_UNCLEAN.store(
        !crate::app::screenshot_mode() && previous_session_unclean(&dir),
        Ordering::Relaxed,
    );
    let now = SystemTime::now();
    let path = dir.join(format!(
        "{FILE_PREFIX}{}-{}{FILE_SUFFIX}",
        file_stamp(now),
        std::process::id()
    ));
    let file = match OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(&path)
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!(
                "dbs-localizer: cannot open the log file {}: {e}",
                path.display()
            );
            return;
        }
    };
    let _ = file.try_lock();
    rotate(&dir, KEEP_FILES);
    let _ = LOG_PATH.set(path.clone());
    if LOGGER.set(Mutex::new(Logger::new(file, SIZE_CAP))).is_err() {
        return;
    }
    install_native_crash_filter();
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "unknown".to_owned());
    info(&format!(
        "{} session started\nsystem: {} {}\nexecutable: {exe}\nstarted: {}\nlog file: {}",
        crate::app::version_line(),
        std::env::consts::OS,
        std::env::consts::ARCH,
        timestamp(now),
        path.display()
    ));
    if PREVIOUS_UNCLEAN.load(Ordering::Relaxed) {
        warn("the previous session did not end normally");
    }
}

fn with_logger(level: Level, message: &str) {
    let Some(lock) = LOGGER.get() else {
        return;
    };
    let mut guard = match lock.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.write(level, message);
}

fn try_with_logger(level: Level, message: &str) {
    let Some(lock) = LOGGER.get() else {
        return;
    };
    let mut guard = match lock.try_lock() {
        Ok(g) => g,
        Err(TryLockError::Poisoned(p)) => p.into_inner(),
        Err(TryLockError::WouldBlock) => return,
    };
    guard.write(level, message);
}

pub fn log(level: Level, message: &str) {
    with_logger(level, message);
}

pub fn info(message: &str) {
    with_logger(Level::Info, message);
}

pub fn warn(message: &str) {
    with_logger(Level::Warn, message);
}

pub fn error(message: &str) {
    with_logger(Level::Error, message);
}

pub fn session_ended() {
    let Some(lock) = LOGGER.get() else {
        return;
    };
    lock.lock().unwrap_or_else(|p| p.into_inner()).finish();
}

pub fn log_path() -> Option<&'static Path> {
    LOG_PATH.get().map(PathBuf::as_path)
}

pub fn open_log_folder() {
    let Some(dir) = log_path().and_then(Path::parent) else {
        return;
    };
    if let Err(e) = open_folder(dir) {
        report_ui_error("Could not open the log folder", &e);
    }
}

fn open_folder(dir: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        crate::win32::open_url(&dir.to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        let program = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        std::process::Command::new(program)
            .arg(dir)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("could not run {program}: {e}"))
    }
}

fn panic_message(info: &std::panic::PanicHookInfo<'_>) -> String {
    let payload = info.payload();
    let text = payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-text panic payload".to_owned());
    let location = info
        .location()
        .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
        .unwrap_or_else(|| "unknown location".to_owned());
    let thread = std::thread::current();
    format!(
        "thread '{}' panicked at {location}\n{text}\nbacktrace:\n{}",
        thread.name().unwrap_or("unnamed"),
        std::backtrace::Backtrace::force_capture()
    )
}

fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        try_with_logger(Level::Panic, &panic_message(info));
        previous(info);
        let on_main = MAIN_THREAD
            .get()
            .is_some_and(|id| *id == std::thread::current().id());
        if on_main && cfg!(all(windows, not(debug_assertions))) {
            crate::win32::error_box("Wyss DBS Localizer", &crash_box_text());
        }
    }));
}

fn crash_box_text() -> String {
    let mut text = String::from("Wyss DBS Localizer stopped because of an internal error.\n\n");
    match log_path() {
        Some(path) => {
            let _ = write!(
                text,
                "A log of this session was saved to:\n{}\n\nPlease send that file to the developers.",
                path.display()
            );
        }
        None => text.push_str("No log file could be written for this session."),
    }
    text
}

#[cfg(windows)]
struct ArrayWriter {
    buf: [u8; 256],
    len: usize,
}

#[cfg(windows)]
impl std::fmt::Write for ArrayWriter {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        let bytes = s.as_bytes();
        let room = self.buf.len() - self.len;
        let n = bytes.len().min(room);
        self.buf[self.len..self.len + n].copy_from_slice(&bytes[..n]);
        self.len += n;
        Ok(())
    }
}

#[cfg(windows)]
unsafe extern "system" fn native_crash_filter(
    info: *const windows_sys::Win32::System::Diagnostics::Debug::EXCEPTION_POINTERS,
) -> i32 {
    const EXCEPTION_CONTINUE_SEARCH: i32 = 0;
    if info.is_null() {
        return EXCEPTION_CONTINUE_SEARCH;
    }
    // SAFETY: Windows passes a valid EXCEPTION_POINTERS to the filter; the record pointer is
    let (code, address) = unsafe {
        let record = (*info).ExceptionRecord;
        if record.is_null() {
            return EXCEPTION_CONTINUE_SEARCH;
        }
        ((*record).ExceptionCode, (*record).ExceptionAddress as usize)
    };
    let Some(lock) = LOGGER.get() else {
        return EXCEPTION_CONTINUE_SEARCH;
    };
    let Ok(mut guard) = lock.try_lock() else {
        return EXCEPTION_CONTINUE_SEARCH;
    };
    let t = utc(SystemTime::now());
    let mut w = ArrayWriter {
        buf: [0; 256],
        len: 0,
    };
    let _ = writeln!(
        w,
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z FATAL native crash: exception code 0x{:08X} at address 0x{:X}",
        t.year, t.month, t.day, t.hour, t.minute, t.second, t.millis, code as u32, address
    );
    let _ = guard.file.write_all(&w.buf[..w.len]);
    let _ = guard.file.flush();
    EXCEPTION_CONTINUE_SEARCH
}

fn install_native_crash_filter() {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Diagnostics::Debug::SetUnhandledExceptionFilter;
        // SAFETY: the filter is a plain function with the signature Windows expects and stays
        unsafe {
            SetUnhandledExceptionFilter(Some(native_crash_filter));
        }
    }
}

pub fn notice(ui: &mut egui::Ui) {
    if !PREVIOUS_UNCLEAN.load(Ordering::Relaxed)
        || NOTICE_DISMISSED.load(Ordering::Relaxed)
        || log_path().is_none()
        || crate::app::screenshot_mode()
    {
        return;
    }
    let unit = crate::panels::em(ui);
    let pal = crate::panels::palette(ui);
    egui::Frame::new()
        .fill(pal.surface_raised)
        .stroke(egui::Stroke::new(1.0_f32, pal.warn))
        .corner_radius(6.0)
        .inner_margin(egui::Margin::same((unit * 0.7) as i8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(
                egui::Label::new(
                    "The previous session ended unexpectedly. Its log was saved; please send it \
                     to the developers.",
                )
                .wrap(),
            );
            ui.add_space(unit * 0.3);
            ui.horizontal_wrapped(|ui| {
                if ui
                    .button("Open log folder")
                    .on_hover_text("Show the folder with the saved log files")
                    .clicked()
                {
                    open_log_folder();
                }
                if ui
                    .button("Dismiss")
                    .on_hover_text("Hide this notice until the application is started again")
                    .clicked()
                {
                    NOTICE_DISMISSED.store(true, Ordering::Relaxed);
                }
            });
        });
    ui.add_space(unit * 0.5);
}

pub fn help_rows(ui: &mut egui::Ui) {
    let Some(path) = log_path() else {
        ui.label("Log file: none for this session");
        return;
    };
    ui.horizontal_wrapped(|ui| {
        ui.add(egui::Label::new(format!("Log file: {}", path.display())).wrap());
        if ui
            .button("Open log folder")
            .on_hover_text("Show the folder with the saved log files")
            .clicked()
        {
            open_log_folder();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "dbsloc_app_log_{tag}_{}_{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn touch(dir: &Path, name: &str, contents: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn rotation_keeps_the_newest_twenty() {
        let dir = temp_dir("rotate");
        for i in 0..27 {
            touch(
                &dir,
                &format!("dbs-localizer-20260101-0000{i:02}-1.log"),
                "x",
            );
        }
        touch(&dir, "state.json", "{}");
        rotate(&dir, 20);
        let files = log_files(&dir);
        assert_eq!(files.len(), 20);
        assert!(files[0].ends_with("dbs-localizer-20260101-000007-1.log"));
        assert!(dir.join("state.json").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unclean_previous_session_is_detected() {
        let dir = temp_dir("unclean");
        assert!(!previous_session_unclean(&dir));
        touch(
            &dir,
            "dbs-localizer-20260101-000001-1.log",
            "a INFO session ended normally\n",
        );
        assert!(!previous_session_unclean(&dir));
        touch(
            &dir,
            "dbs-localizer-20260101-000002-1.log",
            "a INFO started\na ERROR boom\n",
        );
        assert!(previous_session_unclean(&dir));
        touch(
            &dir,
            "dbs-localizer-20260101-000003-1.log",
            "a INFO started\n2026 INFO session ended normally\n",
        );
        assert!(!previous_session_unclean(&dir));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn ended_normally_reads_the_footer() {
        assert!(ended_normally("x\nt INFO session ended normally\n"));
        assert!(!ended_normally("x\ny\n"));
        assert!(!ended_normally(""));
    }

    #[test]
    fn lines_are_formatted_and_continuations_indented() {
        let line = format_line("T", Level::Error, "first\nsecond\nthird");
        assert_eq!(line, "T ERROR first\n    second\n    third\n");
        assert_eq!(format_line("T", Level::Info, "one"), "T INFO one\n");
    }

    #[test]
    fn pipeline_line_level_follows_content() {
        assert_eq!(level_from_line("error: no CT found"), Level::Error);
        assert_eq!(level_from_line("[err] Error: bad"), Level::Error);
        assert_eq!(level_from_line("✖ coreg: failed"), Level::Error);
        assert_eq!(level_from_line("warning: slow"), Level::Warn);
        assert_eq!(level_from_line("[out] WARN thing"), Level::Warn);
        assert_eq!(level_from_line("▶ coreg"), Level::Info);
    }

    #[test]
    fn timestamp_uses_utc_calendar() {
        let t = UNIX_EPOCH + std::time::Duration::from_millis(1_784_116_800_123);
        assert_eq!(timestamp(t), "2026-07-15T12:00:00.123Z");
        assert_eq!(timestamp(UNIX_EPOCH), "1970-01-01T00:00:00.000Z");
        let leap = UNIX_EPOCH + std::time::Duration::from_secs(951_782_400);
        assert_eq!(timestamp(leap), "2000-02-29T00:00:00.000Z");
    }

    #[test]
    fn the_hard_cap_stops_everything_but_a_crash_and_the_session_end() {
        let dir = temp_dir("hardcap");
        let path = dir.join("dbs-localizer-20260101-000001-1.log");
        let file = File::create(&path).unwrap();
        let mut logger = Logger::new(file, 40);
        logger.write(Level::Info, "this first line fills the small cap quickly");
        logger.write(Level::Error, "an error that still fits under twice the cap");
        logger.write(Level::Error, "dropped error");
        logger.write(Level::Warn, "dropped warning");
        logger.write(Level::Panic, "kept panic");
        logger.finish();
        drop(logger);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("dropped"));
        assert_eq!(text.matches(HARD_LIMIT_LINE).count(), 1);
        assert!(text.contains("kept panic"));
        assert!(ended_normally(&text));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_log_of_a_running_session_is_not_judged() {
        let dir = temp_dir("live");
        touch(
            &dir,
            "dbs-localizer-20260101-000001-1.log",
            "a INFO session ended normally\n",
        );
        let live = touch(
            &dir,
            "dbs-localizer-20260101-000002-2.log",
            "a INFO started\n",
        );
        let holder = OpenOptions::new()
            .read(true)
            .append(true)
            .open(&live)
            .unwrap();
        holder.try_lock().unwrap();
        assert!(!previous_session_unclean(&dir));
        drop(holder);
        assert!(previous_session_unclean(&dir));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn interface_errors_are_queued_for_the_window() {
        report_ui_error("Title", "body");
        let taken = take_ui_errors();
        assert!(taken.contains(&("Title".to_owned(), "body".to_owned())));
        assert!(!take_ui_errors().contains(&("Title".to_owned(), "body".to_owned())));
    }

    #[test]
    fn size_cap_keeps_only_warnings_and_errors() {
        let dir = temp_dir("cap");
        let path = dir.join("dbs-localizer-20260101-000001-1.log");
        let file = File::create(&path).unwrap();
        let mut logger = Logger::new(file, 300);
        logger.write(
            Level::Info,
            &"this first line fills the small cap ".repeat(9),
        );
        logger.write(Level::Info, "dropped info one");
        logger.write(Level::Info, "dropped info two");
        logger.write(Level::Warn, "kept warning");
        logger.write(Level::Error, "kept error");
        logger.write(Level::Panic, "kept panic");
        drop(logger);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("this first line"));
        assert!(!text.contains("dropped info"));
        assert_eq!(text.matches(LIMIT_LINE).count(), 1);
        assert!(text.contains("kept warning"));
        assert!(text.contains("kept error"));
        assert!(text.contains("kept panic"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
