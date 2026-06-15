use crate::classify::catalog::match_path;
use crate::scan::index::Index;
use crate::model::{DirAgg, Item, Risk, Source, ROOT_FRN};
use crate::scan::paths::path_for;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Produce non-overlapping, classified items, ranked largest-first.
pub fn cut(index: &Index, totals: &HashMap<u64, DirAgg>, threshold: u64) -> Vec<Item> {
    let mut out: Vec<Item> = Vec::new();
    let mut cache: HashMap<u64, PathBuf> = HashMap::new();
    let mut visited: HashSet<u64> = HashSet::new();
    walk_iterative(index, totals, threshold, &mut out, &mut cache, &mut visited);
    out.sort_by_key(|it| (std::cmp::Reverse(it.physical_size), it.frn));
    out
}

fn agg(totals: &HashMap<u64, DirAgg>, frn: u64) -> (u64, u64) {
    totals.get(&frn).map(|a| (a.physical_size, a.file_count)).unwrap_or((0, 0))
}

/// A single frame in the iterative DFS stack, tracking one directory's
/// traversal state so the compiler never pushes a call frame per level.
struct Frame {
    frn: u64,
    is_root: bool,
    /// Bytes already claimed by emitted items in this subtree.
    claimed: u64,
    /// Total physical_size of this subtree (from aggregate).
    self_total: u64,
    /// File count of this subtree (from aggregate).
    self_count: u64,
    /// Pre-computed volume-relative path (None for root).
    path: Option<PathBuf>,
    /// Index into index.children[frn] for the next child to process.
    child_index: usize,
}

/// Iterative DFS traversal — uses an explicit Vec-based stack so that
/// arbitrarily deep directory trees cannot overflow the call stack.
fn walk_iterative(
    index: &Index,
    totals: &HashMap<u64, DirAgg>,
    threshold: u64,
    out: &mut Vec<Item>,
    cache: &mut HashMap<u64, PathBuf>,
    visited: &mut HashSet<u64>,
) {
    let mut stack: Vec<Frame> = Vec::new();

    // Bootstrap: push the volume root.
    let (root_total, root_count) = agg(totals, ROOT_FRN);
    stack.push(Frame {
        frn: ROOT_FRN,
        is_root: true,
        claimed: 0,
        self_total: root_total,
        self_count: root_count,
        path: None,
        child_index: 0,
    });
    visited.insert(ROOT_FRN);

    loop {
        // --- Decide what to do for the top frame ---
        let action = {
            let frame = match stack.last_mut() {
                Some(f) => f,
                None => break, // all done
            };

            let children_slice: &[u64] = index
                .children
                .get(&frame.frn)
                .map(|c| c.as_slice())
                .unwrap_or(&[]);

            if frame.child_index >= children_slice.len() {
                Action::Pop
            } else {
                let child_frn = children_slice[frame.child_index];
                frame.child_index += 1;
                Action::ProcessChild(child_frn)
            }
        };

        // --- Execute the action ---
        match action {
            Action::Pop => {
                let frame = stack.pop().unwrap(); // safe: we just peeked it
                if !frame.is_root {
                    let residual = frame.self_total.saturating_sub(frame.claimed);
                    let mut subtree_claimed = frame.claimed;
                    if residual >= threshold {
                        out.push(Item {
                            frn: frame.frn,
                            path: frame.path.expect("non-root frame must have path"),
                            is_dir: true,
                            physical_size: residual,
                            file_count: frame.self_count,
                            category: "Unknown".to_string(),
                            purpose: "Unclassified directory contents".to_string(),
                            risk: Risk::Unknown,
                            source: Source::Unknown,
                        });
                        subtree_claimed += residual;
                    }
                    // Propagate claimed bytes upward.
                    if let Some(parent) = stack.last_mut() {
                        parent.claimed += subtree_claimed;
                    }
                }
            }
            Action::ProcessChild(child_frn) => {
                // Cycle guard.
                if !visited.insert(child_frn) {
                    continue;
                }

                let rec = match index.by_frn.get(&child_frn) {
                    Some(r) => r,
                    None => continue,
                };

                if rec.is_dir {
                    let (total, count) = agg(totals, child_frn);

                    // Below threshold → folds into parent's residual; no frame needed.
                    if total < threshold {
                        continue;
                    }

                    let path = path_for(child_frn, index, cache);

                    // Catalog match → whole subtree as one item; don't descend.
                    if let Some(entry) = match_path(&path) {
                        out.push(Item {
                            frn: child_frn,
                            path,
                            is_dir: true,
                            physical_size: total,
                            file_count: count,
                            category: entry.category.to_string(),
                            purpose: entry.purpose.to_string(),
                            risk: entry.risk,
                            source: Source::Rule,
                        });
                        // Claim the whole subtree size on the parent.
                        if let Some(parent) = stack.last_mut() {
                            parent.claimed += total;
                        }
                    } else {
                        // Unknown directory — push a frame and descend.
                        stack.push(Frame {
                            frn: child_frn,
                            is_root: false,
                            claimed: 0,
                            self_total: total,
                            self_count: count,
                            path: Some(path),
                            child_index: 0,
                        });
                    }
                } else if rec.physical_size >= threshold {
                    // Large loose file.
                    let cpath = path_for(child_frn, index, cache);
                    let (category, purpose, risk, source) = match match_path(&cpath) {
                        Some(entry) => (
                            entry.category.to_string(),
                            entry.purpose.to_string(),
                            entry.risk,
                            Source::Rule,
                        ),
                        None => {
                            let (cat, pur, risk) = classify_file(&cpath);
                            (cat, pur, risk, Source::Heuristic)
                        }
                    };
                    out.push(Item {
                        frn: child_frn,
                        path: cpath,
                        is_dir: false,
                        physical_size: rec.physical_size,
                        file_count: 1,
                        category,
                        purpose,
                        risk,
                        source,
                    });
                    if let Some(parent) = stack.last_mut() {
                        parent.claimed += rec.physical_size;
                    }
                }
                // else: file below threshold → folds into parent's residual
            }
        }
    }
}

/// Internal action enum — only used within walk_iterative.
enum Action {
    Pop,
    ProcessChild(u64),
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
        let index = crate::scan::index::build_index(records);
        let totals = crate::scan::aggregate::aggregate(&index);
        (index, totals)
    }

    #[test]
    fn known_dir_is_cut_as_one_item() {
        let (index, totals) = fixture();
        let items = cut(&index, &totals, 100);
        // \Users\dongm now matches the "users/*" catalog entry (User profile, System).
        // It swallows all children, including npm-cache.
        let user = items.iter().find(|i| i.path.ends_with("dongm")).expect("User profile item");
        assert_eq!(user.source, Source::Rule);
        assert_eq!(user.risk, Risk::System);
        assert_eq!(user.physical_size, 1800);
        // npm-cache and its blob are swallowed — they must NOT appear.
        assert!(!items.iter().any(|i| i.path.ends_with("npm-cache")));
        assert!(!items.iter().any(|i| i.path.ends_with("blob")));
    }

    #[test]
    fn root_level_system_file_uses_catalog() {
        // A big file directly under root (e.g. pagefile.sys / $MFT) must be
        // classified by the catalog, not the generic extension heuristic.
        let index = crate::scan::index::build_index(vec![file(30, ROOT_FRN, "$MFT", 5000)]);
        let totals = crate::scan::aggregate::aggregate(&index);
        let items = cut(&index, &totals, 100);
        let mft = items.iter().find(|i| i.path.ends_with("$MFT")).expect("$MFT item");
        assert_eq!(mft.source, Source::Rule);
        assert_eq!(mft.risk, Risk::System);
        assert_eq!(mft.category, "Master File Table");
    }

    #[test]
    fn unknown_big_file_emitted_via_heuristic() {
        // Use a path that does NOT go through a user directory (no "users/*" match).
        let records = vec![
            dir(10, ROOT_FRN, "Projects"),
            dir(11, 10, "myapp"),
            file(21, 11, "big.bin", 800),
        ];
        let index = crate::scan::index::build_index(records);
        let totals = crate::scan::aggregate::aggregate(&index);
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
