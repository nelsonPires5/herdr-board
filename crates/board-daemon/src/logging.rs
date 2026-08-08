//! Structured daemon diagnostics: private daily NDJSON files and retention.

use std::fs::{self, OpenOptions};
use std::io;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use board_core::paths;

const LOG_PREFIX: &str = "daemon.";
const LOG_SUFFIX: &str = ".ndjson";
const RETENTION_DAYS: i64 = 30;
const SECONDS_PER_DAY: i64 = 86_400;

/// Initialize the process subscriber after pruning expired owned logs.
pub(crate) fn init_logging(foreground: bool) {
    use tracing_subscriber::fmt::writer::MakeWriterExt;
    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let log_dir = paths::log_dir();
    let mut deferred = Vec::new();
    if ensure_private_dir(&log_dir).is_err() {
        deferred.push("directory");
    }
    let now = unix_now();
    if prune_logs(&log_dir, now).is_err() {
        deferred.push("retention");
    }

    let writer = DailyWriter {
        dir: log_dir.clone(),
        fallback_reported: Arc::new(AtomicBool::new(false)),
    };
    let installed = if foreground {
        tracing_subscriber::fmt()
            .json()
            .with_env_filter(filter)
            .with_writer(writer.and(io::stderr))
            .try_init()
    } else {
        tracing_subscriber::fmt()
            .json()
            .with_env_filter(filter)
            .with_writer(writer)
            .try_init()
    };

    match installed {
        Ok(()) => {
            for operation in deferred {
                tracing::warn!(
                    target: "diagnostic",
                    error_category = "logging_setup",
                    operation,
                    "diagnostic logging setup failed"
                );
            }
        }
        Err(_) => {
            for operation in deferred {
                eprintln!("boardd: diagnostic logging {operation} setup failed");
            }
            eprintln!("boardd: no tracing subscriber could be installed");
        }
    }
}

pub(crate) async fn retention_task(mut shutdown: tokio::sync::watch::Receiver<bool>) {
    let mut interval =
        tokio::time::interval(std::time::Duration::from_secs(SECONDS_PER_DAY as u64));
    // Startup pruning is done synchronously before the subscriber is installed.
    interval.tick().await;
    loop {
        tokio::select! {
            _ = interval.tick() => {
                if prune_logs(&paths::log_dir(), unix_now()).is_err() {
                    tracing::warn!(target: "diagnostic", error_category = "retention", "diagnostic log pruning failed");
                }
            }
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() { break; }
            }
        }
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn ensure_private_dir(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_dir() => {
            return Err(io::Error::other("diagnostic log path is not a directory"));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir_all(path)?,
        Err(error) => return Err(error),
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

/// Remove only exact owned regular daily files modified more than 30 days ago.
/// Symlinks, directories, malformed names, future files, and the exact boundary
/// fail closed and remain untouched.
fn prune_logs(dir: &Path, now_unix: i64) -> io::Result<()> {
    let cutoff = UNIX_EPOCH
        + Duration::from_secs(
            now_unix
                .saturating_sub(RETENTION_DAYS * SECONDS_PER_DAY)
                .max(0) as u64,
        );
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if !file_type.is_file() || file_type.is_symlink() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if parse_owned_day(&name).is_none() {
            continue;
        }
        let modified = entry.metadata()?.modified()?;
        if modified < cutoff {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

fn parse_owned_day(name: &str) -> Option<i64> {
    let date = name.strip_prefix(LOG_PREFIX)?.strip_suffix(LOG_SUFFIX)?;
    let bytes = date.as_bytes();
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !bytes[0..4].iter().all(u8::is_ascii_digit)
        || !bytes[5..7].iter().all(u8::is_ascii_digit)
        || !bytes[8..10].iter().all(u8::is_ascii_digit)
    {
        return None;
    }
    let year: i32 = date[0..4].parse().ok()?;
    let month: u32 = date[5..7].parse().ok()?;
    let day: u32 = date[8..10].parse().ok()?;
    if !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) {
        return None;
    }
    Some(days_from_civil(year, month, day))
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 31,
    }
}

// Howard Hinnant's civil calendar algorithms, relative to 1970-01-01.
fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let year = year - i32::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let shifted = month as i32 + if month > 2 { -3 } else { 9 };
    let doy = (153 * shifted + 2) / 5 + day as i32 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era * 146_097 + doe - 719_468) as i64
}

fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year as i32, month as u32, day as u32)
}

fn daily_path(dir: &Path, now_unix: i64) -> PathBuf {
    let (year, month, day) = civil_from_days(now_unix.div_euclid(SECONDS_PER_DAY));
    dir.join(format!(
        "{LOG_PREFIX}{year:04}-{month:02}-{day:02}{LOG_SUFFIX}"
    ))
}

#[derive(Clone)]
struct DailyWriter {
    dir: PathBuf,
    fallback_reported: Arc<AtomicBool>,
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for DailyWriter {
    type Writer = DailyFile;
    fn make_writer(&'a self) -> Self::Writer {
        DailyFile::open(&self.dir).unwrap_or_else(|_| DailyFile::Fallback {
            report: !self.fallback_reported.swap(true, Ordering::Relaxed),
        })
    }
}

enum DailyFile {
    File(fs::File),
    // Detached stderr is bootstrap.log. Report one fixed, path-free line per
    // daemon lifetime, then drop records until the daily writer recovers. This
    // deterministically bounds fallback growth even under repeated failures.
    Fallback { report: bool },
}

impl DailyFile {
    fn open(dir: &Path) -> io::Result<Self> {
        ensure_private_dir(dir)?;
        let path = daily_path(dir, unix_now());
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        Ok(Self::File(file))
    }
}

impl io::Write for DailyFile {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self {
            Self::File(file) => io::Write::write(file, bytes),
            Self::Fallback { report } => {
                if std::mem::take(report) {
                    io::Write::write_all(
                        &mut io::stderr(),
                        b"boardd: diagnostic log unavailable; records dropped\n",
                    )?;
                }
                Ok(bytes.len())
            }
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::File(file) => io::Write::flush(file),
            Self::Fallback { .. } => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::process::Command;
    use std::time::{Duration, UNIX_EPOCH};

    const SENTINEL: &str = "DIAGNOSTIC_SECRET_PROMPT_7b90";

    #[test]
    fn logging_child_probe() {
        if std::env::var_os("BOARD_LOGGING_CHILD").is_none() {
            return;
        }
        super::init_logging(false);
        let event_count = std::env::var("BOARD_LOGGING_CHILD_EVENTS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(1);
        for _ in 0..event_count {
            tracing::info!(target: "board_rpc", operation_family = "board_rpc", method = "card.create", outcome = "ok", duration_ms = 3_u64, conn = 41_u64, req_id = 9_u64, "board RPC completed");
        }
    }

    #[test]
    fn daily_ndjson_is_private_redacted_and_prunes_only_expired_owned_files() {
        let dir = tempfile::tempdir().unwrap();
        let log_dir = dir.path().join("logs");
        fs::create_dir_all(&log_dir).unwrap();
        let expired = log_dir.join("daemon.2000-01-01.ndjson");
        let boundary = log_dir.join("daemon.2999-01-01.ndjson");
        let unrelated = log_dir.join("application.log");
        let positive_signed_year = log_dir.join("daemon.+123-01-01.ndjson");
        let negative_signed_year = log_dir.join("daemon.-123-01-01.ndjson");
        let nondigit_year = log_dir.join("daemon.2x23-01-01.ndjson");
        let directory = log_dir.join("daemon.1999-01-01.ndjson");
        let link = log_dir.join("daemon.1998-01-01.ndjson");
        fs::write(&expired, "{}\n").unwrap();
        fs::File::options()
            .write(true)
            .open(&expired)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(1)))
            .unwrap();
        fs::write(&boundary, "{}\n").unwrap();
        fs::set_permissions(&boundary, fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(&unrelated, SENTINEL).unwrap();
        fs::write(&positive_signed_year, "malformed").unwrap();
        fs::write(&negative_signed_year, "malformed").unwrap();
        fs::write(&nondigit_year, "malformed").unwrap();
        for malformed in [&positive_signed_year, &negative_signed_year, &nondigit_year] {
            fs::File::options()
                .write(true)
                .open(malformed)
                .unwrap()
                .set_times(fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(1)))
                .unwrap();
        }
        fs::create_dir(&directory).unwrap();
        symlink(&expired, &link).unwrap();
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "logging::tests::logging_child_probe",
                "--nocapture",
            ])
            .env("BOARD_LOGGING_CHILD", "1")
            .env("BOARD_LOG_DIR", &log_dir)
            .status()
            .unwrap();
        assert!(status.success());
        assert!(!expired.exists(), "expired owned daily file must be pruned");
        assert!(boundary.exists());
        assert!(unrelated.exists());
        assert!(
            positive_signed_year.exists(),
            "positive signed year is not an owned daily name"
        );
        assert!(
            negative_signed_year.exists(),
            "negative signed year is not an owned daily name"
        );
        assert!(nondigit_year.exists(), "non-digit year must fail closed");
        assert!(directory.is_dir());
        assert!(fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        let daily: Vec<_> = fs::read_dir(&log_dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|name| super::parse_owned_day(name).is_some())
                    && p.is_file()
                    && !p.is_symlink()
            })
            .collect();
        assert!(!daily.is_empty());
        let mut found_record = false;
        for path in daily {
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            for line in fs::read_to_string(&path)
                .unwrap()
                .lines()
                .filter(|l| !l.is_empty())
            {
                let value: serde_json::Value = serde_json::from_str(line).unwrap();
                if value.get("target").is_some() {
                    found_record = true;
                    assert!(value.get("timestamp").is_some());
                    assert!(value.get("level").is_some());
                }
            }
            assert!(!fs::read_to_string(&path).unwrap().contains(SENTINEL));
        }
        assert!(found_record, "generated diagnostic record must be present");
        assert_eq!(
            fs::metadata(&log_dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn exact_thirty_day_boundary_is_retained() {
        let dir = tempfile::tempdir().unwrap();
        let now = super::days_from_civil(2026, 8, 31) * super::SECONDS_PER_DAY;
        let boundary = dir.path().join("daemon.2026-08-01.ndjson");
        let expired = dir.path().join("daemon.2026-07-31.ndjson");
        fs::write(&boundary, "").unwrap();
        fs::write(&expired, "").unwrap();
        let boundary_time = UNIX_EPOCH
            + Duration::from_secs((now - super::RETENTION_DAYS * super::SECONDS_PER_DAY) as u64);
        fs::File::options()
            .write(true)
            .open(&boundary)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(boundary_time))
            .unwrap();
        fs::File::options()
            .write(true)
            .open(&expired)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(boundary_time - Duration::from_secs(1)))
            .unwrap();
        super::prune_logs(dir.path(), now).unwrap();
        assert!(boundary.exists());
        assert!(!expired.exists());
    }

    #[test]
    fn failed_daily_writer_has_a_bounded_private_detached_fallback() {
        use std::process::Stdio;

        let dir = tempfile::tempdir().unwrap();
        let log_dir = dir.path().join("logs");
        fs::create_dir_all(&log_dir).unwrap();
        let target = dir.path().join("symlink-target");
        fs::write(&target, "untouched").unwrap();
        symlink(&target, super::daily_path(&log_dir, super::unix_now())).unwrap();
        let bootstrap = dir.path().join("bootstrap.log");
        let stderr = fs::File::create(&bootstrap).unwrap();
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "logging::tests::logging_child_probe",
                "--nocapture",
            ])
            .env("BOARD_LOGGING_CHILD", "1")
            .env("BOARD_LOGGING_CHILD_EVENTS", "40")
            .env("BOARD_LOG_DIR", &log_dir)
            .stderr(Stdio::from(stderr))
            .status()
            .unwrap();
        assert!(status.success());
        assert!(
            fs::metadata(&bootstrap).unwrap().len() <= 512,
            "one daemon lifetime must not grow bootstrap fallback without bound"
        );
        assert_eq!(fs::read_to_string(&target).unwrap(), "untouched");
        assert!(
            fs::symlink_metadata(super::daily_path(&log_dir, super::unix_now()))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn daily_writer_restricts_an_existing_regular_file_before_use() {
        let dir = tempfile::tempdir().unwrap();
        let path = super::daily_path(dir.path(), super::unix_now());
        fs::write(&path, "existing\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

        let writer = super::DailyFile::open(dir.path()).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        drop(writer);
        assert_eq!(fs::read_to_string(path).unwrap(), "existing\n");
    }

    #[test]
    fn daily_writer_refuses_an_owned_name_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        fs::write(&target, "sentinel").unwrap();
        let path = super::daily_path(dir.path(), super::unix_now());
        symlink(&target, &path).unwrap();

        assert!(super::DailyFile::open(dir.path()).is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), "sentinel");
        assert!(fs::symlink_metadata(path).unwrap().file_type().is_symlink());
    }
}
