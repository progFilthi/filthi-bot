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

    /// Open the file for the already-locked writer slot. Takes the slot by
    /// reference instead of re-locking: `record` holds the mutex, and a
    /// same-thread second `lock()` here deadlocked the whole session task
    /// on the first inbound line (the EA's hello never got recorded).
    fn open(path: &str, slot: &mut Option<BufWriter<File>>) -> std::io::Result<()> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        *slot = Some(BufWriter::new(file));
        Ok(())
    }

    /// Record one raw wire line. `dir` is `"in"` (EA -> engine) or `"out"`.
    pub fn record(&self, dir: &str, raw: &str) {
        let mut guard = self.writer.lock().unwrap();
        if guard.is_none() {
            if let Err(e) = Self::open(&self.path, &mut guard) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    /// Regression: `record` used to lock the writer mutex and then call
    /// `open`, which locked it again — same-thread deadlock on the very
    /// first recorded line. This test must complete.
    #[test]
    fn record_does_not_deadlock_and_writes_all_lines() {
        let dir = std::env::temp_dir().join(format!(
            "xau_transcript_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = dir.join("t.jsonl");
        let t = Transcript::new(path.to_str().unwrap()).unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            t.record("in", r#"{"type":"hello"}"#);
            t.record("out", r#"{"type":"heartbeat"}"#);
            t.summary(&serde_json::json!({"event": "session_end", "ok": true}));
            let _ = tx.send(());
        });
        assert!(
            rx.recv_timeout(Duration::from_secs(5)).is_ok(),
            "record deadlocked"
        );
        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 3, "expected 3 transcript lines, got {lines:?}");
        assert!(lines[0].contains(r#""dir":"in""#));
        assert!(lines[2].contains(r#""dir":"summary""#));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
