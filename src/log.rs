use parking_lot::Mutex;
use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::PathBuf,
    sync::Arc,
};

// -----------------------------
// Logging
// -----------------------------
#[derive(Clone)]
pub struct LogBuffer {
    inner: Arc<Mutex<Vec<String>>>,
    file: Arc<Mutex<Option<File>>>,
}

impl Default for LogBuffer {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Vec::new())),
            file: Arc::new(Mutex::new(None)),
        }
    }
}

impl LogBuffer {
    pub fn push(&self, s: impl Into<String>) {
        let line = Self::stamp(s.into());
        {
            let mut g = self.inner.lock();
            g.push(line.clone());
            let len = g.len();
            if len > 3000 {
                g.drain(0..(len - 3000));
            }
        }
        if let Some(f) = &mut *self.file.lock() {
            let _ = writeln!(f, "{}", line);
            let _ = f.flush();
        }
    }

    #[allow(dead_code)]
    pub fn snapshot(&self) -> Vec<String> {
        self.inner.lock().clone()
    }

    /// Initialize file logging, preferring the EXE directory. Truncates per session.
    pub fn try_init_file_prefer_exe_dir(&self) -> std::io::Result<PathBuf> {
        if let Ok(p) = std::env::current_exe() {
            if let Some(dir) = p.parent() {
                let mut log_path = dir.to_path_buf();
                log_path.push("UrsaMinorFFB.log");
                if self.attach_file_at(&log_path).is_ok() {
                    return Ok(log_path);
                }
            }
        }
        if let Some(base) = std::env::var_os("LOCALAPPDATA") {
            let mut p = PathBuf::from(base);
            p.push("UrsaMinorFFB");
            let _ = std::fs::create_dir_all(&p);
            p.push("UrsaMinorFFB.log");
            self.attach_file_at(&p)?;
            return Ok(p);
        }
        let mut p = std::env::temp_dir();
        p.push("UrsaMinorFFB.log");
        self.attach_file_at(&p)?;
        Ok(p)
    }

    pub fn attach_file_at(&self, path: &PathBuf) -> std::io::Result<()> {
        let f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)?;
        {
            let mut fg = self.file.lock();
            *fg = Some(f);
        }
        self.push(format!(
            "==== session start: v{} pid={} ====",
            env!("CARGO_PKG_VERSION"),
            std::process::id()
        ));
        Ok(())
    }

    #[inline]
    fn stamp(msg: String) -> String {
        let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
        format!("[{}] {}", ts, msg)
    }
}
