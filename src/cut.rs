use crate::catalog::match_path;
use crate::index::Index;
use crate::model::{DirAgg, Item, Risk, Source, ROOT_FRN};
use crate::paths::path_for;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Produce non-overlapping, classified items, ranked largest-first.
pub fn cut(index: &Index, totals: &HashMap<u64, DirAgg>, threshold: u64) -> Vec<Item> {
    let mut out: Vec<Item> = Vec::new();
    let mut cache: HashMap<u64, PathBuf> = HashMap::new();
    walk(ROOT_FRN, true, index, totals, threshold, &mut out, &mut cache);
    out.sort_by_key(|it| (std::cmp::Reverse(it.physical_size), it.frn));
    out
}

fn agg(totals: &HashMap<u64, DirAgg>, frn: u64) -> (u64, u64) {
    totals.get(&frn).map(|a| (a.physical_size, a.file_count)).unwrap_or((0, 0))
}

/// Returns bytes claimed (emitted) under and including `frn`.
fn walk(
    frn: u64,
    is_root: bool,
    index: &Index,
    totals: &HashMap<u64, DirAgg>,
    threshold: u64,
    out: &mut Vec<Item>,
    cache: &mut HashMap<u64, PathBuf>,
) -> u64 {
    // Known directory → emit whole subtree as one item, do not descend.
    if !is_root {
        let path = path_for(frn, index, cache);
        if let Some(entry) = match_path(&path) {
            let (size, count) = agg(totals, frn);
            out.push(Item {
                frn, path, is_dir: true, physical_size: size, file_count: count,
                category: entry.category.to_string(), purpose: entry.purpose.to_string(),
                risk: entry.risk, source: Source::Rule,
            });
            return size;
        }
    }

    let (total, count) = agg(totals, frn);
    if !is_root && total < threshold {
        return 0; // too small to surface; folds into an ancestor's residual
    }

    let mut claimed = 0u64;
    if let Some(children) = index.children.get(&frn) {
        for &child in children {
            match index.by_frn.get(&child) {
                Some(rec) if rec.is_dir => {
                    claimed += walk(child, false, index, totals, threshold, out, cache);
                }
                Some(rec) => {
                    if rec.physical_size >= threshold {
                        let cpath = path_for(child, index, cache);
                        let (category, purpose, risk) = classify_file(&cpath);
                        out.push(Item {
                            frn: child, path: cpath, is_dir: false,
                            physical_size: rec.physical_size, file_count: 1,
                            category, purpose, risk, source: Source::Heuristic,
                        });
                        claimed += rec.physical_size;
                    }
                }
                None => {}
            }
        }
    }

    if !is_root {
        let residual = total.saturating_sub(claimed);
        if residual >= threshold {
            let path = path_for(frn, index, cache);
            out.push(Item {
                frn, path, is_dir: true, physical_size: residual, file_count: count,
                category: "Unknown".to_string(),
                purpose: "Unclassified directory contents".to_string(),
                risk: Risk::Unknown, source: Source::Unknown,
            });
            claimed += residual;
        }
    }
    claimed
}

/// Classify a large loose file by extension.
fn classify_file(path: &Path) -> (String, String, Risk) {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    let (cat, purpose, risk) = match ext.as_str() {
        "mp4" | "mov" | "mkv" | "avi" | "wmv" | "flv" => ("Video", "Video file", Risk::Caution),
        "zip" | "7z" | "rar" | "iso" | "tar" | "gz" | "zst" => ("Archive", "Archive/compressed file", Risk::Caution),
        "msi" | "exe" => ("Installer/binary", "Installer or executable", Risk::Caution),
        "sys" => ("System file", "Windows system file", Risk::System),
        "vhd" | "vhdx" | "vmdk" => ("Disk image", "Virtual disk image", Risk::Caution),
        "bin" | "dat" | "tmp" | "cache" | "log" => ("Data/cache", "Binary data, cache or log file", Risk::Unknown),
        _ => ("Large file", "Large file", Risk::Unknown),
    };
    (cat.to_string(), purpose.to_string(), risk)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::RawRecord;

    fn dir(frn: u64, parent: u64, name: &str) -> RawRecord {
        RawRecord { frn, parent_frn: parent, name: name.into(), is_dir: true, is_reparse: false,
            logical_size: 0, physical_size: 0, hard_link_count: 1, in_use: true }
    }
    fn file(frn: u64, parent: u64, name: &str, size: u64) -> RawRecord {
        RawRecord { frn, parent_frn: parent, name: name.into(), is_dir: false, is_reparse: false,
            logical_size: size, physical_size: size, hard_link_count: 1, in_use: true }
    }

    // Build: \(5) > Users(10) > dongm(11) > {AppData(12)>Local(13)>npm-cache(14)>blob(20)=1000,
    //                                        myapp(15) > big.bin(21)=800 }
    fn fixture() -> (Index, HashMap<u64, DirAgg>) {
        let records = vec![
            dir(10, ROOT_FRN, "Users"),
            dir(11, 10, "dongm"),
            dir(12, 11, "AppData"),
            dir(13, 12, "Local"),
            dir(14, 13, "npm-cache"),
            file(20, 14, "blob", 1000),
            dir(15, 11, "myapp"),
            file(21, 15, "big.bin", 800),
        ];
        let index = crate::index::build_index(records);
        let totals = crate::aggregate::aggregate(&index);
        (index, totals)
    }

    #[test]
    fn known_dir_is_cut_as_one_item() {
        let (index, totals) = fixture();
        let items = cut(&index, &totals, 100);
        let npm = items.iter().find(|i| i.path.ends_with("npm-cache")).expect("npm-cache item");
        assert_eq!(npm.source, Source::Rule);
        assert_eq!(npm.risk, Risk::Safe);
        assert_eq!(npm.physical_size, 1000);
        // The blob inside npm-cache must NOT also appear as its own item.
        assert!(!items.iter().any(|i| i.path.ends_with("blob")));
    }

    #[test]
    fn unknown_big_file_emitted_via_heuristic() {
        let (index, totals) = fixture();
        let items = cut(&index, &totals, 100);
        let big = items.iter().find(|i| i.path.ends_with("big.bin")).expect("big.bin item");
        assert_eq!(big.source, Source::Heuristic);
        assert_eq!(big.physical_size, 800);
    }

    #[test]
    fn items_do_not_overlap_in_total() {
        let (index, totals) = fixture();
        let items = cut(&index, &totals, 100);
        // Sum of item sizes must not exceed the volume total (no double counting).
        let sum: u64 = items.iter().map(|i| i.physical_size).sum();
        assert!(sum <= totals[&ROOT_FRN].physical_size);
        assert_eq!(totals[&ROOT_FRN].physical_size, 1800);
    }
}
