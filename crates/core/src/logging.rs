// each process keeps a log in the data folder, which people can copy from settings when something goes wrong
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use log::{Level, LevelFilter, Log, Metadata, Record};

// a full log moves aside to `name.1.log`, so at most twice this is ever kept
const LIMIT: u64 = 2 * 1024 * 1024;

struct Logger {
    path: PathBuf,
    file: Mutex<Option<File>>,
}

// logs mux's own messages from info up and other crates' only when they warn, then records panics too
pub fn init(file_name: &str) {
    let path = crate::protocol::data_dir().join(file_name);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let logger = Box::leak(Box::new(Logger {
        file: Mutex::new(open(&path)),
        path,
    }));
    if log::set_logger(logger).is_err() {
        return;
    }
    log::set_max_level(LevelFilter::Info);
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        log::error!(target: "mux::panic", "{info}\n{backtrace}");
        previous(info);
    }));
}

pub fn path(file_name: &str) -> PathBuf {
    crate::protocol::data_dir().join(file_name)
}

fn open(path: &Path) -> Option<File> {
    OpenOptions::new().create(true).append(true).open(path).ok()
}

impl Log for Logger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        let ours = metadata.target().starts_with("mux");
        metadata.level() <= if ours { Level::Info } else { Level::Warn }
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        // later lines of a message are indented, so a reader can tell where each entry starts
        let message = record.args().to_string().replace('\n', "\n    ");
        let line = format!(
            "{} {:<5} {}: {message}\n",
            timestamp(SystemTime::now()),
            record.level(),
            record.target(),
        );
        let Ok(mut file) = self.file.lock() else {
            return;
        };
        let full = file
            .as_ref()
            .and_then(|f| f.metadata().ok())
            .is_some_and(|m| m.len() > LIMIT);
        if full {
            let _ = std::fs::rename(&self.path, self.path.with_extension("1.log"));
            *file = open(&self.path);
        }
        if let Some(file) = file.as_mut() {
            let _ = file.write_all(line.as_bytes());
        }
    }

    fn flush(&self) {
        if let Ok(mut file) = self.file.lock()
            && let Some(file) = file.as_mut()
        {
            let _ = file.flush();
        }
    }
}

// utc, so the app's and the server's lines sort together and read the same wherever someone lives
pub fn timestamp(at: SystemTime) -> String {
    let since = at.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = since.as_secs() as i64;
    let (year, month, day) = civil_from_days(secs.div_euclid(86_400));
    let of_day = secs.rem_euclid(86_400);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}.{:03}Z",
        of_day / 3600,
        of_day / 60 % 60,
        of_day % 60,
        since.subsec_millis(),
    )
}

// the calendar date for a count of days since 1970, after howard hinnant's algorithm
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let of_era = days - era * 146_097;
    let year_of_era = (of_era - of_era / 1460 + of_era / 36_524 - of_era / 146_096) / 365;
    let of_year = of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted = (5 * of_year + 2) / 153;
    let day = of_year - (153 * shifted + 2) / 5 + 1;
    let month = if shifted < 10 {
        shifted + 3
    } else {
        shifted - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

// the end of a log, at most `bytes` long and starting on a whole line
pub fn tail(path: &Path, bytes: u64) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = File::open(path) else {
        return String::new();
    };
    let len = file.metadata().map_or(0, |m| m.len());
    let start = len.saturating_sub(bytes);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut buf = Vec::new();
    let _ = file.read_to_end(&mut buf);
    let text = String::from_utf8_lossy(&buf).into_owned();
    if start == 0 {
        return text;
    }
    match text.find('\n') {
        Some(cut) => text[cut + 1..].to_owned(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn formats_utc_timestamps() {
        let at = UNIX_EPOCH + Duration::from_millis(1_793_190_636_123);
        assert_eq!(timestamp(at), "2026-10-28 12:30:36.123Z");
        assert_eq!(timestamp(UNIX_EPOCH), "1970-01-01 00:00:00.000Z");
    }

    #[test]
    fn tails_start_on_a_whole_line() {
        let path = std::env::temp_dir().join(format!("mux-tail-{}.log", std::process::id()));
        std::fs::write(&path, "first line\nsecond line\nthird\n").unwrap();
        assert_eq!(tail(&path, 14), "third\n");
        assert_eq!(tail(&path, 1000), "first line\nsecond line\nthird\n");
        let _ = std::fs::remove_file(&path);
    }
}
