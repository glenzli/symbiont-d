use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result};
use tracing_subscriber::{
    EnvFilter, Layer, filter::filter_fn, fmt, layer::SubscriberExt, util::SubscriberInitExt,
};

const DEFAULT_MAX_BYTES: u64 = 512 * 1024;
const DEFAULT_BACKUPS: usize = 4;
pub const TARGET: &str = "symbiont_runtime";

/// Installs the normal console diagnostics plus a deliberately low-volume,
/// size-bounded operational log. Only events emitted to [`TARGET`] enter the
/// rolling file; model text, prompts, PCP payloads, and execution traces remain
/// in their existing owners.
pub fn init(path: PathBuf) -> Result<()> {
    let writer = RollingLogWriter::open(path, DEFAULT_MAX_BYTES, DEFAULT_BACKUPS)?;
    let console_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("symbiont_d=info"));
    let runtime_filter = filter_fn(|metadata| metadata.target() == TARGET);

    tracing_subscriber::registry()
        .with(
            fmt::layer()
                .with_target(false)
                .compact()
                .with_filter(console_filter),
        )
        .with(
            fmt::layer()
                .with_target(false)
                .with_ansi(false)
                .compact()
                .with_writer(writer)
                .with_filter(runtime_filter),
        )
        .try_init()
        .context("install runtime logging")
}

#[derive(Clone)]
struct RollingLogWriter {
    state: Arc<Mutex<RollingLogState>>,
}

impl RollingLogWriter {
    fn open(path: PathBuf, max_bytes: u64, backups: usize) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create runtime log directory {}", parent.display()))?;
        }
        let file = open_append(&path)?;
        let bytes = file
            .metadata()
            .with_context(|| format!("inspect runtime log {}", path.display()))?
            .len();
        Ok(Self {
            state: Arc::new(Mutex::new(RollingLogState {
                path,
                file,
                bytes,
                max_bytes: max_bytes.max(1),
                backups,
            })),
        })
    }
}

impl<'a> fmt::MakeWriter<'a> for RollingLogWriter {
    type Writer = RollingLogFile;

    fn make_writer(&'a self) -> Self::Writer {
        RollingLogFile {
            state: Arc::clone(&self.state),
        }
    }
}

struct RollingLogFile {
    state: Arc<Mutex<RollingLogState>>,
}

impl Write for RollingLogFile {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| io::Error::other("runtime log lock poisoned"))?;
        if state.bytes >= state.max_bytes {
            state.rotate()?;
        }
        let written = state.file.write(buffer)?;
        state.bytes = state.bytes.saturating_add(written as u64);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.state
            .lock()
            .map_err(|_| io::Error::other("runtime log lock poisoned"))?
            .file
            .flush()
    }
}

struct RollingLogState {
    path: PathBuf,
    file: File,
    bytes: u64,
    max_bytes: u64,
    backups: usize,
}

impl RollingLogState {
    fn rotate(&mut self) -> io::Result<()> {
        self.file.flush()?;
        if self.backups > 0 {
            let oldest = backup_path(&self.path, self.backups);
            if oldest.exists() {
                fs::remove_file(oldest)?;
            }
            for index in (1..self.backups).rev() {
                let source = backup_path(&self.path, index);
                if source.exists() {
                    fs::rename(source, backup_path(&self.path, index + 1))?;
                }
            }
            if self.path.exists() {
                fs::rename(&self.path, backup_path(&self.path, 1))?;
            }
        } else if self.path.exists() {
            fs::remove_file(&self.path)?;
        }
        self.file = open_append(&self.path).map_err(io::Error::other)?;
        self.bytes = 0;
        Ok(())
    }
}

fn open_append(path: &Path) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open runtime log {}", path.display()))
}

fn backup_path(path: &Path, index: usize) -> PathBuf {
    PathBuf::from(format!("{}.{index}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::{fs, io::Write};

    use super::RollingLogWriter;
    use tracing_subscriber::fmt::MakeWriter;

    #[test]
    fn rotates_at_the_size_boundary_and_keeps_bounded_backups() {
        let directory = std::env::temp_dir().join(format!(
            "symbiont-runtime-log-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_micros()
        ));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("runtime.log");
        let writer = RollingLogWriter::open(path.clone(), 8, 2).unwrap();

        for line in ["12345678", "abcdefgh", "ABCDEFGH", "final"] {
            let mut file = writer.make_writer();
            file.write_all(line.as_bytes()).unwrap();
            file.flush().unwrap();
        }

        assert_eq!(fs::read_to_string(&path).unwrap(), "final");
        assert_eq!(
            fs::read_to_string(directory.join("runtime.log.1")).unwrap(),
            "ABCDEFGH"
        );
        assert_eq!(
            fs::read_to_string(directory.join("runtime.log.2")).unwrap(),
            "abcdefgh"
        );
        fs::remove_dir_all(directory).unwrap();
    }
}
