//! Persistable report writer.
//!
//! Writes enrichment diagnostics (probe log, timing, LLM summary) to a dated
//! file so users can inspect run history even after terminal scrollback is lost.

use std::fs::File;
use std::io::{BufWriter, Write, Result};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct ReportFile {
    path: PathBuf,
    buf: BufWriter<File>,
}

impl ReportFile {
    /// Create a new report file in the current directory with a timestamped name.
    pub fn create() -> Result<Self> {
        let dur = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let secs = dur.as_secs();
        // Build "YYYY-MM-DD_HH-MM-SS" manually — zero deps.
        let d = unix_to_ymdhms(secs);
        let name = format!(
            "enrichment_report_{:04}-{:02}-{:02}_{:02}-{:02}-{:02}.log",
            d.0, d.1, d.2, d.3, d.4, d.5
        );
        let path = PathBuf::from(&name);
        let file = File::create(&path)?;
        let mut report = Self {
            buf: BufWriter::new(file),
            path,
        };
        writeln!(report.buf, "Disk Organizer — Enrichment Report")?;
        writeln!(
            report.buf,
            "Started: {:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            d.0, d.1, d.2, d.3, d.4, d.5
        )?;
        writeln!(report.buf)?;
        report.buf.flush()?;
        Ok(report)
    }

    /// Append a string as a section header.
    pub fn section(&mut self, title: &str) -> Result<()> {
        writeln!(self.buf)?;
        writeln!(self.buf, "{}", "=".repeat(70))?;
        writeln!(self.buf, "  {title}")?;
        writeln!(self.buf, "{}", "=".repeat(70))?;
        Ok(())
    }

    /// Append a plain line.
    pub fn line(&mut self, s: &str) -> Result<()> {
        writeln!(self.buf, "{s}")?;
        Ok(())
    }

    /// Append a key: value line.
    pub fn kv(&mut self, key: &str, val: &str) -> Result<()> {
        writeln!(self.buf, "  {key}: {val}")?;
        Ok(())
    }

    /// Flush buffered writes to disk.
    pub fn flush(&mut self) -> Result<()> {
        self.buf.flush()?;
        Ok(())
    }

    /// Return the file path (for the final message).
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

/// Format a Duration as "X.Xs".
pub fn fmt_dur(d: std::time::Duration) -> String {
    format!("{:.1}s", d.as_secs_f64())
}

/// Decompose a UNIX timestamp into (Y, M, D, H, Min, S).
fn unix_to_ymdhms(ts: u64) -> (i32, u32, u32, u32, u32, u32) {
    // Days since epoch.
    let days = ts / 86400;
    let sec_of_day = ts % 86400;
    let h = (sec_of_day / 3600) as u32;
    let m = ((sec_of_day % 3600) / 60) as u32;
    let s = (sec_of_day % 60) as u32;

    // Algorithm from http://howardhinnant.github.io/date_algorithms.html
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32; // day of era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // year of era [0, 399]
    let y = (yoe as i64 + era * 400) as i32;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day of year [0, 365]
    let mp = (5 * doy + 2) / 153; // month phase
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = y + if mo <= 2 { 1 } else { 0 };
    (y, mo, d, h, m, s)
}

