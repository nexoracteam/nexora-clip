//! A small rolling log file in the data folder, for finding out what went wrong later.

use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;

static DIR: OnceLock<PathBuf> = OnceLock::new();

pub fn init(data_dir: &std::path::Path) {
    let dir = data_dir.join("logs");
    let _ = std::fs::create_dir_all(&dir);
    let _ = DIR.set(dir);
}

pub fn dir() -> Option<PathBuf> {
    DIR.get().cloned()
}

pub fn log(message: &str) {
    let Some(dir) = DIR.get() else { return };
    let file = dir.join("nexora-clip.log");
    if std::fs::metadata(&file).map(|m| m.len() > 2_000_000).unwrap_or(false) {
        let _ = std::fs::rename(&file, dir.join("nexora-clip.old.log"));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(file) {
        let _ = writeln!(f, "{} {}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"), message.replace('\n', " | "));
    }
}

/// Write panics to the log instead of losing them.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log(&format!("PANIC: {info}"));
        previous(info);
    }));
}
