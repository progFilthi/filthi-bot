//! Append-only JSONL transcript of every wire message in both directions,
//! plus a final summary record. Used to verify the Phase 0 round trip.

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct Transcript {
    writer: Mutex<Option<BufWriter<File>>>,
    path: String,
}

impl Transcript {
    pub fn new(path: &str) -> anyhow::Result<Self> {
        if let Some(parent) = Path::new(path).parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(Self {
            writer: Mutex::new(None),
            path: path.into(),
        })
    }

    fn open(&self) -> std::io::Result<()> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        *self.writer.lock().unwrap() = Some(BufWriter::new(file));
        Ok(())
    }

    /// Record one raw wire line. `dir` is `"in"` (EA -> engine) or `"out"`.
    pub fn record(&self, dir: &str, raw: &str) {
        let mut guard = self.writer.lock().unwrap();
        if guard.is_none() {
            if let Err(e) = self.open() {
                eprintln!("transcript open failed: {e}");
                return;
            }
        }
        let ts_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let rec = serde_json::json!({
            "ts_ms": ts_ms,
            "dir": dir,
            "raw": raw,
        });
        if let Some(w) = guard.as_mut() {
            let _ = writeln!(w, "{rec}");
            let _ = w.flush();
        }
    }

    /// Record a structured summary event.
    pub fn summary(&self, value: &serde_json::Value) {
        self.record("summary", &value.to_string());
    }
}
