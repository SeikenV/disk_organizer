use crate::model::RawRecord;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// A saved scan: the drive and every counted record. Lets us re-analyze
/// without another (Administrator) MFT read.
#[derive(Serialize, Deserialize, PartialEq, Debug)]
pub struct Snapshot {
    pub drive: String,
    pub records: Vec<RawRecord>,
}

/// Write a snapshot to `path` as JSON.
pub fn save(path: &Path, drive: &str, records: &[RawRecord]) -> std::io::Result<()> {
    let snap = Snapshot { drive: drive.to_string(), records: records.to_vec() };
    let json = serde_json::to_vec(&snap).map_err(std::io::Error::other)?;
    std::fs::write(path, json)
}

/// Read a snapshot from `path`.
pub fn load(path: &Path) -> std::io::Result<Snapshot> {
    let bytes = std::fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(std::io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(frn: u64, name: &str, size: u64) -> RawRecord {
        RawRecord { frn, parent_frn: 5, name: name.into(), is_dir: false, is_reparse: false,
            logical_size: size, physical_size: size, hard_link_count: 1, in_use: true }
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("scan.json");
        let records = vec![rec(20, "a.bin", 100), rec(21, "b.bin", 200)];

        save(&path, "C", &records).unwrap();
        let loaded = load(&path).unwrap();

        assert_eq!(loaded.drive, "C");
        assert_eq!(loaded.records, records);
    }
}
