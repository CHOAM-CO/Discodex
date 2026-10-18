use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::{Context, Result};
use tracing_subscriber::fmt::writer::MakeWriter;

const LOG_DIRECTORY: &str = "logs";
const ACTIVE_LOG: &str = "discodex.log";
const PREVIOUS_LOG: &str = "discodex.previous.log";
const MAX_LOG_BYTES: u64 = 4 * 1024 * 1024;

/// Initialize persistent diagnostics for the long-running bridge process.
/// The active log and one previous segment are each bounded to 4 MiB.
pub fn init_file_logging(state_dir: &Path) -> Result<()> {
    let directory = state_dir.join(LOG_DIRECTORY);
    let writer = LogMakeWriter::open(&directory, MAX_LOG_BYTES)
        .with_context(|| format!("open Discodex diagnostic log in {}", directory.display()))?;
    tracing_subscriber::fmt()
        .with_env_filter(env_filter())
        .with_ansi(false)
        .with_writer(writer)
        .try_init()
        .map_err(|error| anyhow::anyhow!("initialize persistent Discodex diagnostics: {error}"))?;
    install_panic_hook();
    tracing::info!(
        log_file = "logs/discodex.log",
        "persistent diagnostics initialized"
    );
    Ok(())
}

/// Keep ordinary CLI diagnostics on stderr. MCP commands return before this is called.
pub fn init_stderr_logging() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(env_filter())
        .with_writer(std::io::stderr)
        .try_init()
        .map_err(|error| anyhow::anyhow!("initialize stderr diagnostics: {error}"))
}

/// Open the native app-server stderr destination. One previous process log is retained.
pub fn native_stderr_file(state_dir: &Path) -> Result<File> {
    let directory = state_dir.join(LOG_DIRECTORY);
    fs::create_dir_all(&directory).with_context(|| {
        format!(
            "create native diagnostic log directory {}",
            directory.display()
        )
    })?;
    let active_path = directory.join("codex-app-server.stderr.log");
    let previous_path = directory.join("codex-app-server.stderr.previous.log");
    if active_path.exists() {
        remove_if_exists(&previous_path).with_context(|| {
            format!(
                "rotate previous native diagnostic log in {}",
                directory.display()
            )
        })?;
        fs::rename(&active_path, &previous_path)
            .with_context(|| format!("rotate native diagnostic log in {}", directory.display()))?;
    }
    OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&active_path)
        .with_context(|| format!("open native diagnostic log in {}", directory.display()))
}

fn env_filter() -> tracing_subscriber::EnvFilter {
    tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))
}

fn install_panic_hook() {
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic| {
        let thread = std::thread::current();
        let location = panic.location();
        tracing::error!(
            thread = thread.name().unwrap_or("unnamed"),
            line = location.map(|location| location.line()),
            column = location.map(|location| location.column()),
            "process panic"
        );
        previous_hook(panic);
    }));
}

#[derive(Clone)]
struct LogMakeWriter(Arc<Mutex<RotatingLog>>);

impl LogMakeWriter {
    fn open(directory: &Path, max_bytes: u64) -> io::Result<Self> {
        Ok(Self(Arc::new(Mutex::new(RotatingLog::open(
            directory, max_bytes,
        )?))))
    }

    fn lock(&self) -> MutexGuard<'_, RotatingLog> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl<'a> MakeWriter<'a> for LogMakeWriter {
    type Writer = LogWriter;

    fn make_writer(&'a self) -> Self::Writer {
        LogWriter(self.clone())
    }
}

struct LogWriter(LogMakeWriter);

impl Write for LogWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.lock().flush()
    }
}

impl Drop for LogWriter {
    fn drop(&mut self) {
        let _ = self.0.lock().flush();
    }
}

struct RotatingLog {
    active_path: PathBuf,
    previous_path: PathBuf,
    file: Option<File>,
    written: u64,
    max_bytes: u64,
}

impl RotatingLog {
    fn open(directory: &Path, max_bytes: u64) -> io::Result<Self> {
        fs::create_dir_all(directory)?;
        let active_path = directory.join(ACTIVE_LOG);
        let previous_path = directory.join(PREVIOUS_LOG);
        if active_path.exists() {
            remove_if_exists(&previous_path)?;
            fs::rename(&active_path, &previous_path)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&active_path)?;
        let written = file.metadata()?.len();
        Ok(Self {
            active_path,
            previous_path,
            file: Some(file),
            written,
            max_bytes: max_bytes.max(1),
        })
    }

    fn rotate(&mut self) -> io::Result<()> {
        if let Some(mut file) = self.file.take() {
            file.flush()?;
        }
        remove_if_exists(&self.previous_path)?;
        if self.active_path.exists() {
            fs::rename(&self.active_path, &self.previous_path)?;
        }
        self.file = Some(
            OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&self.active_path)?,
        );
        self.written = 0;
        Ok(())
    }

    fn write(&mut self, mut bytes: &[u8]) -> io::Result<usize> {
        let original_len = bytes.len();
        while !bytes.is_empty() {
            if self.written >= self.max_bytes {
                self.rotate()?;
            }
            let capacity = (self.max_bytes - self.written) as usize;
            let chunk_len = capacity.min(bytes.len());
            let result = self
                .file
                .as_mut()
                .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "rotating log is closed"))
                .and_then(|file| file.write_all(&bytes[..chunk_len]));
            if let Err(error) = result {
                self.written = self
                    .file
                    .as_ref()
                    .and_then(|file| file.metadata().ok())
                    .map_or(self.written, |metadata| metadata.len());
                return Err(error);
            }
            self.written += chunk_len as u64;
            bytes = &bytes[chunk_len..];
        }
        Ok(original_len)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "rotating log is closed"))?
            .flush()
    }
}

fn remove_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_rotation_keeps_current_and_previous_segments_bounded() {
        let directory = tempfile::tempdir().unwrap();
        let mut log = RotatingLog::open(directory.path(), 5).unwrap();

        assert_eq!(log.write(b"12345").unwrap(), 5);
        assert_eq!(log.write(b"67").unwrap(), 2);
        assert_eq!(fs::read(&log.active_path).unwrap(), b"67");
        assert_eq!(fs::read(&log.previous_path).unwrap(), b"12345");

        assert_eq!(log.write(b"890").unwrap(), 3);
        assert_eq!(fs::read(&log.active_path).unwrap(), b"67890");
        assert_eq!(fs::read(&log.previous_path).unwrap(), b"12345");
        assert!(fs::metadata(&log.active_path).unwrap().len() <= 5);
        assert!(fs::metadata(&log.previous_path).unwrap().len() <= 5);
    }

    #[test]
    fn native_stderr_file_rotates_at_each_bridge_start() {
        let directory = tempfile::tempdir().unwrap();
        let state_dir = directory.path().join("state");
        {
            let mut stderr = native_stderr_file(&state_dir).unwrap();
            stderr.write_all(b"native diagnostic").unwrap();
        }

        let mut next_stderr = native_stderr_file(&state_dir).unwrap();
        next_stderr.write_all(b"next run").unwrap();
        let logs = state_dir.join(LOG_DIRECTORY);
        assert_eq!(
            fs::read(logs.join("codex-app-server.stderr.log")).unwrap(),
            b"next run"
        );
        assert_eq!(
            fs::read(logs.join("codex-app-server.stderr.previous.log")).unwrap(),
            b"native diagnostic"
        );
    }

    #[test]
    fn startup_rotates_the_last_run_and_starts_a_fresh_active_log() {
        let directory = tempfile::tempdir().unwrap();
        {
            let mut first = RotatingLog::open(directory.path(), 8).unwrap();
            first.write(b"old run").unwrap();
        }

        let second = RotatingLog::open(directory.path(), 8).unwrap();
        assert_eq!(fs::read(&second.active_path).unwrap(), b"");
        assert_eq!(fs::read(&second.previous_path).unwrap(), b"old run");
    }
}
