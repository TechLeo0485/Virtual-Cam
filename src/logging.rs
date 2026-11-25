//! `[vcam] ...` messages. They go to stdout and, when [`init_file_log`] was called (the
//! windowed GUI has no console), also to `%LOCALAPPDATA%\vcam\vcam.log`.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::Mutex;

static LOG_FILE: Mutex<Option<File>> = Mutex::new(None);

/// Keep every message in `%LOCALAPPDATA%\vcam\vcam.log` (restarted when it grows past
/// 1 MB) so errors can be read after the fact.
pub fn init_file_log() {
    let dir = crate::util::app_data_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join("vcam.log");
    let too_big = std::fs::metadata(&path).map(|m| m.len() > 1_000_000).unwrap_or(false);
    let mut opts = OpenOptions::new();
    opts.create(true);
    if too_big {
        opts.write(true).truncate(true);
    } else {
        opts.append(true);
    }
    if let Ok(f) = opts.open(&path) {
        if let Ok(mut guard) = LOG_FILE.lock() {
            *guard = Some(f);
        }
    }
}

pub fn write_line(args: std::fmt::Arguments<'_>) {
    let line = format!("{}", args);
    println!("{}", line);
    if let Ok(mut guard) = LOG_FILE.lock() {
        if let Some(f) = guard.as_mut() {
            let _ = writeln!(f, "{}", line);
        }
    }
}

/// `vlog!("[vcam] ...")` — like `println!`, plus the log file.
#[macro_export]
macro_rules! vlog {
    ($($arg:tt)*) => {
        $crate::logging::write_line(format_args!($($arg)*))
    };
}
