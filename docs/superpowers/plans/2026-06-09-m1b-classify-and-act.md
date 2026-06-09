# M1b: Classify & Act Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Turn M1a's raw size scan into a ranked list of *classified* items (category + purpose + cleanup risk), let the user select items by number, and delete the selected ones to the Recycle Bin — plus snapshot the scan so re-runs don't need an Administrator MFT read.

**Architecture:** A curated embedded **catalog** maps directory paths to {category, purpose, risk}. A single non-overlapping **cut** DFS over M1a's `Index` + `aggregate` totals produces `Item`s: known caches/dirs (from the catalog), large loose files (extension heuristic), and each directory's *unclaimed residual* (Unknown). Scans serialize to a JSON **snapshot** (no admin needed to re-analyze). The **CLI** prints a numbered, risk-annotated list; the user types numbers; selected items are deleted to the **Recycle Bin** (`trash` crate) after a typed confirmation. System-risk items are never deleted.

**Tech Stack:** Builds on M1a (Rust 2021; `model`/`index`/`paths`/`aggregate`/`tree`/`mft_scan`/`volume`). Adds `serde` + `serde_json` (snapshot) and `trash` (Recycle Bin). `cargo` is at `C:\Users\dongm\.cargo\bin\cargo.exe` (NOT on PATH — use full path in PowerShell: `& "$env:USERPROFILE\.cargo\bin\cargo.exe" ...`).

---

## File Structure

```
src/
├─ model.rs       # MODIFY: add Risk, Source, Item; derive serde on RawRecord
├─ catalog.rs     # NEW: CatalogEntry + embedded table + match_path()      (pure, TDD)
├─ cut.rs         # NEW: non-overlapping cut DFS → Vec<Item> + file heuristic (pure, TDD)
├─ snapshot.rs    # NEW: save/load Vec<RawRecord> as JSON                   (TDD, tempfile)
├─ select.rs      # NEW: parse_selection("1 3 5", max) → indices           (pure, TDD)
├─ delete.rs      # NEW: full_path(), DeletionPlan, delete_to_recycle_bin   (pure + tempfile)
├─ lib.rs         # MODIFY: declare new modules
└─ main.rs        # MODIFY: snapshot flags, classified list, interactive select+delete
```

Pure/TDD: `catalog`, `cut`, `select`, the pure parts of `delete`, snapshot round-trip. The interactive CLI loop in `main.rs` is verified by running (with a saved snapshot — no admin needed) + `--dry-run`.

---

### Task 1: Model additions + dependencies

**Files:** Modify `Cargo.toml`, `src/model.rs`, `src/lib.rs`

- [ ] **Step 1: Add dependencies**

In `Cargo.toml`, add to `[dependencies]` (keep existing `clap`, `mft`; keep the windows-only block and dev `tempfile`):

```toml
serde = { version = "1", features = ["derive"] }
serde_json = "1"
trash = "5"
```

- [ ] **Step 2: Extend `src/model.rs`** — add the classification types and put serde on `RawRecord`. Replace the file's current contents with:

```rust
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// MFT record number of the volume root directory.
pub const ROOT_FRN: u64 = 5;

/// One physical file/directory parsed from a single MFT record.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RawRecord {
    pub frn: u64,
    pub parent_frn: u64,
    pub name: String,
    pub is_dir: bool,
    pub is_reparse: bool,
    pub logical_size: u64,
    pub physical_size: u64,
    pub hard_link_count: u16,
    pub in_use: bool,
}

/// Aggregated totals for a directory subtree.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DirAgg {
    pub logical_size: u64,
    pub physical_size: u64,
    pub file_count: u64,
}

/// Cleanup risk. Decided by rules, never by guesses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Risk {
    Safe,    // cache/temp/regenerable — deleting loses nothing important
    Caution, // possibly wanted (downloads, media, user data)
    System,  // OS/app critical — never auto-deletable
    Unknown, // not covered by rules
}

/// Where an item's classification came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Rule,      // matched the catalog
    Heuristic, // file-extension guess
    Unknown,   // unclassified residual
}

/// A unit shown to the user and selectable for deletion. Items never overlap:
/// every counted byte belongs to exactly one Item.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub frn: u64,
    pub path: PathBuf,       // volume-relative, e.g. \Users\me\AppData\Local\npm-cache
    pub is_dir: bool,
    pub physical_size: u64,
    pub file_count: u64,
    pub category: String,
    pub purpose: String,
    pub risk: Risk,
    pub source: Source,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_frn_is_five() {
        assert_eq!(ROOT_FRN, 5);
        assert_eq!(DirAgg::default(), DirAgg { logical_size: 0, physical_size: 0, file_count: 0 });
    }

    #[test]
    fn rawrecord_serde_round_trips() {
        let r = RawRecord {
            frn: 20, parent_frn: 10, name: "x.bin".into(), is_dir: false, is_reparse: false,
            logical_size: 100, physical_size: 128, hard_link_count: 1, in_use: true,
        };
        let json = serde_json::to_string(&r).unwrap();
        let back: RawRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }
}
```

- [ ] **Step 3: Declare new modules in `src/lib.rs`** — add these lines (keep existing ones):

```rust
pub mod catalog;
pub mod cut;
pub mod delete;
pub mod select;
pub mod snapshot;
```

(`catalog`/`cut`/etc. files are created in later tasks; if you need an intermediate green build, create each as an empty file first, or add each `pub mod` line as its task lands. Keep the project compiling after every task.)

- [ ] **Step 4: Verify** — `& "$env:USERPROFILE\.cargo\bin\cargo.exe" test model`
Expected: `root_frn_is_five` and `rawrecord_serde_round_trips` pass. (If `lib.rs` references not-yet-created modules, temporarily comment those `pub mod` lines until their task — re-add per task.)

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/model.rs src/lib.rs
git commit -m "feat: classification model (Risk/Source/Item) + serde + deps"
```

---

### Task 2: Catalog (`catalog.rs`)

**Files:** Create `src/catalog.rs`

- [ ] **Step 1: Write the failing test + implementation**

Path matching rule: lowercase the path, split on `\` and `/`, and check whether the path's trailing components equal the pattern's components, where `*` matches any single component. So pattern `appdata/local/temp` matches `\Users\me\AppData\Local\Temp`.

```rust
use crate::model::Risk;
use std::path::Path;

/// One catalog rule: a path-suffix pattern → what the directory is + its risk.
pub struct CatalogEntry {
    /// Lowercase, '/'-separated trailing components; `*` matches one component.
    pub pattern: &'static str,
    pub category: &'static str,
    pub purpose: &'static str,
    pub risk: Risk,
}

/// The curated catalog of well-known directories. Ordered most-specific first;
/// match_path returns the first match.
pub fn catalog() -> &'static [CatalogEntry] {
    use Risk::*;
    &[
        // --- developer caches (Safe) ---
        CatalogEntry { pattern: "appdata/local/npm-cache", category: "Node.js package cache", purpose: "npm download cache; rebuilt on next install", risk: Safe },
        CatalogEntry { pattern: "appdata/roaming/npm-cache", category: "Node.js package cache", purpose: "npm download cache; rebuilt on next install", risk: Safe },
        CatalogEntry { pattern: "appdata/local/yarn/cache", category: "Yarn package cache", purpose: "Yarn download cache; rebuilt on next install", risk: Safe },
        CatalogEntry { pattern: ".gradle/caches", category: "Gradle build cache", purpose: "Gradle dependency/build cache; rebuilt on next build", risk: Safe },
        CatalogEntry { pattern: ".m2/repository", category: "Maven repository", purpose: "Maven dependency cache; re-downloaded on next build", risk: Safe },
        CatalogEntry { pattern: ".cargo/registry", category: "Rust cargo registry", purpose: "Cargo crate cache; re-downloaded on next build", risk: Safe },
        CatalogEntry { pattern: "appdata/local/pip/cache", category: "pip cache", purpose: "Python pip download cache; rebuilt on next install", risk: Safe },
        CatalogEntry { pattern: ".cache/pip", category: "pip cache", purpose: "Python pip download cache; rebuilt on next install", risk: Safe },
        CatalogEntry { pattern: "appdata/local/nvidia/dxcache", category: "NVIDIA shader cache", purpose: "GPU shader cache; regenerated automatically", risk: Safe },
        CatalogEntry { pattern: "appdata/local/nvidia/glcache", category: "NVIDIA shader cache", purpose: "GPU shader cache; regenerated automatically", risk: Safe },
        CatalogEntry { pattern: "appdata/local/d3dscache", category: "DirectX shader cache", purpose: "GPU shader cache; regenerated automatically", risk: Safe },
        CatalogEntry { pattern: "node_modules", category: "Node.js modules", purpose: "Project dependencies; re-installable with npm install", risk: Caution },
        CatalogEntry { pattern: "__pycache__", category: "Python bytecode cache", purpose: "Compiled .pyc files; regenerated automatically", risk: Safe },
        // --- Windows temp / update (Safe) ---
        CatalogEntry { pattern: "appdata/local/temp", category: "User temp files", purpose: "Per-user temporary files; safe to clear", risk: Safe },
        CatalogEntry { pattern: "windows/temp", category: "Windows temp files", purpose: "System temporary files; safe to clear", risk: Safe },
        CatalogEntry { pattern: "windows/softwaredistribution/download", category: "Windows Update cache", purpose: "Downloaded update files; rebuilt by Windows Update", risk: Safe },
        CatalogEntry { pattern: "windows/installer/$patchcache$", category: "Windows Installer patch cache", purpose: "MSI patch cache; removing can break repair/uninstall", risk: Caution },
        CatalogEntry { pattern: "$recycle.bin", category: "Recycle Bin", purpose: "Already-deleted files awaiting purge; empty to reclaim", risk: Caution },
        // --- browser caches (Safe) ---
        CatalogEntry { pattern: "appdata/local/google/chrome/user data/default/cache", category: "Chrome cache", purpose: "Browser cache; rebuilt automatically", risk: Safe },
        CatalogEntry { pattern: "appdata/local/microsoft/edge/user data/default/cache", category: "Edge cache", purpose: "Browser cache; rebuilt automatically", risk: Safe },
        // --- user data (Caution) ---
        CatalogEntry { pattern: "users/*/downloads", category: "Downloads", purpose: "Downloaded files; review before deleting", risk: Caution },
        CatalogEntry { pattern: "users/*/videos", category: "Videos", purpose: "User videos; review before deleting", risk: Caution },
        CatalogEntry { pattern: "users/*/pictures", category: "Pictures", purpose: "User pictures; review before deleting", risk: Caution },
        CatalogEntry { pattern: "users/*/documents", category: "Documents", purpose: "User documents; review before deleting", risk: Caution },
        CatalogEntry { pattern: "users/*/desktop", category: "Desktop", purpose: "Desktop files; review before deleting", risk: Caution },
        // --- system (System: never delete) ---
        CatalogEntry { pattern: "windows/winsxs", category: "Windows component store", purpose: "Servicing store (hardlinked); manage via DISM only", risk: System },
        CatalogEntry { pattern: "windows/system32", category: "Windows system files", purpose: "Core OS files; do not delete", risk: System },
        CatalogEntry { pattern: "pagefile.sys", category: "Virtual memory", purpose: "Paging file; resize via System settings, do not delete", risk: System },
        CatalogEntry { pattern: "hiberfil.sys", category: "Hibernation file", purpose: "Hibernation image; disable via powercfg, do not delete", risk: System },
        CatalogEntry { pattern: "swapfile.sys", category: "Swap file", purpose: "System swap file; managed by Windows", risk: System },
        CatalogEntry { pattern: "$mft", category: "Master File Table", purpose: "NTFS metadata; do not delete", risk: System },
        CatalogEntry { pattern: "program files/windowsapps", category: "Store apps", purpose: "Installed Store apps; uninstall via Settings, not by deleting", risk: System },
    ]
}

/// Lowercase '/'-separated components of a path.
fn components(path: &Path) -> Vec<String> {
    path.to_string_lossy()
        .to_lowercase()
        .split(['\\', '/'])
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

/// True if `path_comps` ends with `pat_comps` (with `*` matching any one component).
fn suffix_matches(path_comps: &[String], pat_comps: &[&str]) -> bool {
    if pat_comps.len() > path_comps.len() {
        return false;
    }
    let start = path_comps.len() - pat_comps.len();
    pat_comps
        .iter()
        .enumerate()
        .all(|(i, pat)| *pat == "*" || *pat == path_comps[start + i])
}

/// Return the first catalog entry whose pattern matches the tail of `path`.
pub fn match_path(path: &Path) -> Option<&'static CatalogEntry> {
    let comps = components(path);
    catalog().iter().find(|entry| {
        let pat: Vec<&str> = entry.pattern.split('/').filter(|s| !s.is_empty()).collect();
        suffix_matches(&comps, &pat)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn matches_npm_cache_anywhere() {
        let e = match_path(Path::new(r"\Users\dongm\AppData\Local\npm-cache")).unwrap();
        assert_eq!(e.category, "Node.js package cache");
        assert_eq!(e.risk, Risk::Safe);
    }

    #[test]
    fn wildcard_matches_any_user() {
        let e = match_path(Path::new(r"\Users\someone\Downloads")).unwrap();
        assert_eq!(e.risk, Risk::Caution);
    }

    #[test]
    fn matches_system_pagefile() {
        let e = match_path(Path::new(r"\pagefile.sys")).unwrap();
        assert_eq!(e.risk, Risk::System);
    }

    #[test]
    fn no_match_for_ordinary_dir() {
        assert!(match_path(Path::new(r"\Users\dongm\projects\myapp")).is_none());
    }
}
```

- [ ] **Step 2: Verify** — `& "$env:USERPROFILE\.cargo\bin\cargo.exe" test catalog` → 4 tests pass.
- [ ] **Step 3: Commit** — `git add src/catalog.rs src/lib.rs && git commit -m "feat: known-directory catalog with suffix matching"`

---

### Task 3: Cut engine (`cut.rs`)

**Files:** Create `src/cut.rs`

Non-overlapping DFS from root. For each directory: if the catalog matches, emit one Known item for the whole subtree and stop. Otherwise recurse; emit large loose files (extension heuristic) and, after carving out everything claimed by children, emit the directory's **residual** (total minus claimed) as an Unknown item if it's still above the threshold. The function returns how many bytes it claimed so the parent can compute its residual.

- [ ] **Step 1: Write the failing test + implementation**

```rust
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
```

- [ ] **Step 2: Verify** — `& "$env:USERPROFILE\.cargo\bin\cargo.exe" test cut` → 3 tests pass.
- [ ] **Step 3: Commit** — `git add src/cut.rs src/lib.rs && git commit -m "feat: non-overlapping cut/classify engine"`

---

### Task 4: Snapshot persistence (`snapshot.rs`)

**Files:** Create `src/snapshot.rs`

- [ ] **Step 1: Write the failing test + implementation**

```rust
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
```

- [ ] **Step 2: Verify** — `& "$env:USERPROFILE\.cargo\bin\cargo.exe" test snapshot` → 1 test passes.
- [ ] **Step 3: Commit** — `git add src/snapshot.rs src/lib.rs && git commit -m "feat: JSON scan snapshot save/load"`

---

### Task 5: Selection parsing (`select.rs`)

**Files:** Create `src/select.rs`

- [ ] **Step 1: Write the failing test + implementation**

```rust
/// Parse a user selection string like "1 3 5" or "1,3,5" into 0-based indices.
/// `count` is the number of listed items (1..=count are valid). Returns an error
/// message string on any out-of-range or non-numeric token. Duplicates are removed.
pub fn parse_selection(input: &str, count: usize) -> Result<Vec<usize>, String> {
    let mut out: Vec<usize> = Vec::new();
    for tok in input.split([' ', ',', '\t']).filter(|s| !s.is_empty()) {
        let n: usize = tok.parse().map_err(|_| format!("not a number: '{tok}'"))?;
        if n < 1 || n > count {
            return Err(format!("out of range: {n} (valid 1..={count})"));
        }
        let idx = n - 1;
        if !out.contains(&idx) {
            out.push(idx);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_spaces_and_commas() {
        assert_eq!(parse_selection("1 3,5", 5).unwrap(), vec![0, 2, 4]);
    }

    #[test]
    fn dedups() {
        assert_eq!(parse_selection("2 2 2", 5).unwrap(), vec![1]);
    }

    #[test]
    fn rejects_out_of_range() {
        assert!(parse_selection("6", 5).is_err());
        assert!(parse_selection("0", 5).is_err());
    }

    #[test]
    fn rejects_non_numeric() {
        assert!(parse_selection("1 x", 5).is_err());
    }

    #[test]
    fn empty_is_empty() {
        assert_eq!(parse_selection("   ", 5).unwrap(), Vec::<usize>::new());
    }
}
```

- [ ] **Step 2: Verify** — `& "$env:USERPROFILE\.cargo\bin\cargo.exe" test select` → 5 tests pass.
- [ ] **Step 3: Commit** — `git add src/select.rs src/lib.rs && git commit -m "feat: parse numbered selection input"`

---

### Task 6: Deletion (`delete.rs`)

**Files:** Create `src/delete.rs`

System-risk items are never deletable. `full_path` turns a volume-relative item path into a real filesystem path by prepending the drive.

- [ ] **Step 1: Write the failing test + implementation**

```rust
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
```

- [ ] **Step 2: Verify** — `& "$env:USERPROFILE\.cargo\bin\cargo.exe" test delete` → 3 tests pass. (`recycle_bin_removes_a_real_file` uses a temp file and the real Recycle Bin; it should pass on any Windows session — no admin needed.)
- [ ] **Step 3: Commit** — `git add src/delete.rs src/lib.rs && git commit -m "feat: deletion plan + recycle-bin delete"`

---

### Task 7: Interactive CLI (`main.rs`)

**Files:** Modify `src/main.rs`

- [ ] **Step 1: Replace `src/main.rs` with the classified, interactive CLI**

```rust
use clap::Parser;
use disk_organizer::aggregate::aggregate;
use disk_organizer::cut::cut;
use disk_organizer::delete::{delete_to_recycle_bin, full_path, plan};
use disk_organizer::format::human;
use disk_organizer::index::build_index;
use disk_organizer::model::{Item, RawRecord, Risk};
use disk_organizer::select::parse_selection;
use disk_organizer::snapshot;
use std::io::Write;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "disk_organizer", about = "Classify and clean up disk usage via the NTFS MFT")]
struct Args {
    /// Drive letter to scan, e.g. C (omit when using --from-snapshot)
    drive: Option<String>,
    #[arg(long, default_value_t = 40)]
    top: usize,
    #[arg(long, default_value_t = 200)]
    min_size_mb: u64,
    /// Save the raw scan to a JSON snapshot
    #[arg(long)]
    save_snapshot: Option<PathBuf>,
    /// Analyze a saved snapshot instead of reading the MFT (no admin needed)
    #[arg(long)]
    from_snapshot: Option<PathBuf>,
    /// Print what would be deleted without deleting
    #[arg(long)]
    dry_run: bool,
}

fn risk_tag(r: Risk) -> &'static str {
    match r {
        Risk::Safe => "[SAFE]   ",
        Risk::Caution => "[CAUTION]",
        Risk::System => "[SYSTEM] ",
        Risk::Unknown => "[UNKNOWN]",
    }
}

fn main() -> std::io::Result<()> {
    let args = Args::parse();
    let min = args.min_size_mb.saturating_mul(1024 * 1024);

    // 1. Obtain records: from snapshot, or by reading the MFT.
    let (drive, records): (String, Vec<RawRecord>) = match &args.from_snapshot {
        Some(path) => {
            eprintln!("Loading snapshot {} ...", path.display());
            let snap = snapshot::load(path)?;
            (snap.drive, snap.records)
        }
        None => {
            let drive = args.drive.clone().unwrap_or_else(|| {
                eprintln!("error: provide a drive letter or --from-snapshot");
                std::process::exit(2);
            });
            eprintln!("Reading MFT for {drive}: (requires Administrator) ...");
            let image = disk_organizer::volume::read_mft(&drive)?;
            (drive, disk_organizer::mft_scan::parse_records(image.bytes))
        }
    };
    eprintln!("{} records.", records.len());

    if let Some(path) = &args.save_snapshot {
        snapshot::save(path, &drive, &records)?;
        eprintln!("Saved snapshot to {}", path.display());
    }

    // 2. Classify.
    let index = build_index(records);
    let totals = aggregate(&index);
    let mut items = cut(&index, &totals, min);
    items.truncate(args.top);

    // 3. Print the numbered, risk-annotated list.
    println!("\n#   Risk       Size        Category — path");
    for (i, it) in items.iter().enumerate() {
        println!(
            "{:>3} {} {:>10}  {} — {}",
            i + 1, risk_tag(it.risk), human(it.physical_size), it.category, it.path.display()
        );
    }
    println!("\nLegend: SAFE=cache/regenerable, CAUTION=review first, SYSTEM=never deleted, UNKNOWN=unclassified");

    // 4. Prompt for selection.
    print!("\nEnter numbers to delete (e.g. 1 3 5), or just Enter to quit: ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let chosen = match parse_selection(line.trim(), items.len()) {
        Ok(v) if v.is_empty() => { eprintln!("Nothing selected. Bye."); return Ok(()); }
        Ok(v) => v,
        Err(e) => { eprintln!("Invalid selection: {e}"); return Ok(()); }
    };

    let selected: Vec<Item> = chosen.iter().map(|&i| items[i].clone()).collect();
    let p = plan(&selected);

    // 5. Summarize + confirm.
    if !p.skipped_system.is_empty() {
        eprintln!("\nSkipping {} SYSTEM item(s) (never auto-deleted):", p.skipped_system.len());
        for it in &p.skipped_system {
            eprintln!("  - {}", it.path.display());
        }
    }
    if p.deletable.is_empty() {
        eprintln!("Nothing deletable selected. Bye.");
        return Ok(());
    }
    println!(
        "\nAbout to send {} item(s) to the Recycle Bin: {} (SAFE {}, CAUTION {}).",
        p.deletable.len(), human(p.total_bytes), p.safe, p.caution
    );

    let full: Vec<PathBuf> = p.deletable.iter().map(|it| full_path(&drive, &it.path)).collect();

    if args.dry_run {
        println!("[dry-run] would delete:");
        for path in &full {
            println!("  {}", path.display());
        }
        return Ok(());
    }

    print!("Type 'yes' to confirm: ");
    std::io::stdout().flush()?;
    let mut confirm = String::new();
    std::io::stdin().read_line(&mut confirm)?;
    if confirm.trim() != "yes" {
        eprintln!("Cancelled.");
        return Ok(());
    }

    // 6. Delete to Recycle Bin.
    let results = delete_to_recycle_bin(&full);
    let mut ok = 0;
    for (path, res) in &results {
        match res {
            Ok(()) => ok += 1,
            Err(e) => eprintln!("  FAILED {}: {e}", path.display()),
        }
    }
    println!("Moved {ok}/{} item(s) to the Recycle Bin.", results.len());
    Ok(())
}
```

- [ ] **Step 2: Build + test** — `& "$env:USERPROFILE\.cargo\bin\cargo.exe" build` and `... test`. All pure tests pass; the binary compiles.

- [ ] **Step 3: Verify without admin via a snapshot.** If `scan_output`/a snapshot isn't handy, create one in an Administrator shell once: `cargo run --release -- C --min-size-mb 200 --save-snapshot scan.snapshot.json` (then exit admin). Then **non-elevated**: `& "$env:USERPROFILE\.cargo\bin\cargo.exe" run --release -- --from-snapshot scan.snapshot.json --top 30`. Expected: a numbered, risk-tagged list; npm-cache/DXCache show `[SAFE]`, Downloads/Videos `[CAUTION]`, pagefile/WinSxS `[SYSTEM]`.

- [ ] **Step 4: Verify selection + dry-run.** Re-run with `--dry-run`, pick a couple of numbers at the prompt; confirm it prints the real `C:\...` paths it *would* delete (and skips any SYSTEM picks) without deleting. Optionally test one real deletion of a throwaway file via its number (it goes to the Recycle Bin, recoverable).

- [ ] **Step 5: Commit** — `git add src/main.rs && git commit -m "feat: interactive classified CLI with select + recycle-bin delete"`

---

## Self-Review

**Spec coverage (functional F4/F5-rules/F8/F9/F10 + architecture C3/C4/C8/C9):**
- F4 rule classification (catalog) → Task 2. ✓
- C4 recursive cut into non-overlapping items → Task 3. ✓
- Risk levels Safe/Caution/System/Unknown, rules-decided → Tasks 1–3. ✓
- F10 snapshot save/load → Task 4. ✓
- F8 numbered selection → Tasks 5 + 7. ✓
- F9 safe delete: Recycle Bin, typed confirmation, System excluded, dry-run → Tasks 6 + 7. ✓
- CLI display of classified list → Task 7. ✓
- **Deferred (documented):** LLM verification/summary of unknown dirs (M2 — Unknown items are exactly the hand-off points); BleachBit CleanerML import (catalog is curated for now); non-NTFS fallback (M1a jwalk plan); per-file drill-down.

**Placeholder scan:** Every task has complete, compilable code + tests. No TBD/TODO. The CLI interactive loop (Task 7) is verified by running with a snapshot + dry-run rather than unit tests, because stdin-driven flow isn't unit-testable — the pure helpers it calls (`cut`, `parse_selection`, `plan`, `full_path`) are all tested.

**Type consistency:** `Item`/`Risk`/`Source` defined once in `model.rs` (Task 1), used identically in `cut.rs`, `delete.rs`, `main.rs`. `match_path(&Path) -> Option<&CatalogEntry>` (Task 2) called by `cut` (Task 3). `cut(index, totals, threshold) -> Vec<Item>`, `parse_selection(&str, usize) -> Result<Vec<usize>, String>`, `plan(&[Item]) -> DeletionPlan`, `full_path(&str, &Path) -> PathBuf`, `delete_to_recycle_bin(&[PathBuf])`, `snapshot::{save,load}` signatures match all call sites in Task 7.

**Known first-cut simplifications (intentional):** suffix-component catalog matching can in principle over-match a deep folder that happens to share a full multi-component tail (low risk given multi-component patterns); residual Unknown items below threshold are not surfaced (their bytes are simply not listed); a directory item's deletion removes its whole subtree (expected — that's the cut unit).
