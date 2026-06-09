use crate::model::{Item, Risk};
use std::path::{Path, PathBuf};

/// Turn a volume-relative item path (e.g. `\Users\me\x`) into a real path on
/// `drive` (e.g. `C:\Users\me\x`).
pub fn full_path(drive: &str, item_path: &Path) -> PathBuf {
    let rel = item_path.to_string_lossy();
    let rel = rel.trim_start_matches(['\\', '/']);
    PathBuf::from(format!("{drive}:\\{rel}"))
}

/// What a deletion would do. System-risk items are excluded from `deletable`.
#[derive(Debug, PartialEq)]
pub struct DeletionPlan {
    pub deletable: Vec<Item>,
    pub skipped_system: Vec<Item>,
    pub total_bytes: u64,
    pub safe: usize,
    pub caution: usize,
}

/// Build a deletion plan from the chosen items (System items are skipped).
pub fn plan(selected: &[Item]) -> DeletionPlan {
    let mut deletable = Vec::new();
    let mut skipped_system = Vec::new();
    let (mut total, mut safe, mut caution) = (0u64, 0usize, 0usize);
    for item in selected {
        match item.risk {
            Risk::System => skipped_system.push(item.clone()),
            Risk::Safe => { safe += 1; total += item.physical_size; deletable.push(item.clone()); }
            _ => { caution += 1; total += item.physical_size; deletable.push(item.clone()); }
        }
    }
    DeletionPlan { deletable, skipped_system, total_bytes: total, safe, caution }
}

/// Move each path to the Recycle Bin. Returns per-path results (errors don't
/// abort the rest).
pub fn delete_to_recycle_bin(paths: &[PathBuf]) -> Vec<(PathBuf, Result<(), String>)> {
    paths
        .iter()
        .map(|p| (p.clone(), trash::delete(p).map_err(|e| e.to_string())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Source;

    fn item(risk: Risk, size: u64) -> Item {
        Item {
            frn: 1, path: PathBuf::from(r"\x"), is_dir: false, physical_size: size,
            file_count: 1, category: "c".into(), purpose: "p".into(), risk, source: Source::Rule,
        }
    }

    #[test]
    fn full_path_prepends_drive() {
        assert_eq!(full_path("C", Path::new(r"\Users\me\x")), PathBuf::from(r"C:\Users\me\x"));
        assert_eq!(full_path("D", Path::new(r"pagefile.sys")), PathBuf::from(r"D:\pagefile.sys"));
    }

    #[test]
    fn plan_excludes_system_and_sums_rest() {
        let p = plan(&[item(Risk::Safe, 100), item(Risk::Caution, 50), item(Risk::System, 999)]);
        assert_eq!(p.deletable.len(), 2);
        assert_eq!(p.skipped_system.len(), 1);
        assert_eq!(p.total_bytes, 150);
        assert_eq!(p.safe, 1);
        assert_eq!(p.caution, 1);
    }

    #[test]
    fn recycle_bin_removes_a_real_file() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("trash_me.txt");
        std::fs::write(&f, b"bye").unwrap();
        assert!(f.exists());

        let results = delete_to_recycle_bin(&[f.clone()]);
        assert_eq!(results.len(), 1);
        assert!(results[0].1.is_ok(), "trash failed: {:?}", results[0].1);
        assert!(!f.exists(), "file should be gone from its original location");
    }
}
