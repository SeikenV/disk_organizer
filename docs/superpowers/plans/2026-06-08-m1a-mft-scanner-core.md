# M1a (MFT) Scanner Core Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.
>
> **Supersedes** the jwalk-based `2026-06-08-m1a-scanner-core.md` (that approach is now the non-NTFS / no-admin fallback, deferred). Read [MFT approach synthesis](../research/2026-06-08-mft-approach-synthesis.md) first — it carries the full algorithm and citations.

**Goal:** Read the NTFS MFT directly (WizTree-style) to enumerate every file on a volume with correct sizes and correct hardlink handling, and print the Top-N largest directories and files via a CLI.

**Architecture:** A Windows FFI layer opens `\\.\C:` (admin), uses `FSCTL_GET_NTFS_VOLUME_DATA` + `FSCTL_GET_RETRIEVAL_POINTERS` to bulk-read the `$MFT` into memory, and the `mft` crate parses each FILE record. This produces a plain `Vec<RawRecord>` — the seam. Everything downstream (index, path reconstruction, hardlink-dedup aggregation, top-N, formatting) is pure and unit-tested. Hardlinks are correct for free: we iterate physical MFT records (one FRN = one file), not directory links.

**Tech Stack:** Rust 2021; `mft` (record parsing); `windows` (raw volume + FSCTL FFI); `clap` (CLI); `tempfile` (dev tests). Windows + NTFS + Administrator required (non-NTFS/no-admin → jwalk fallback, later).

---

## File Structure

```
src/
├─ main.rs        # CLI: parse args → scan → aggregate → print
├─ lib.rs         # module exports for integration tests
├─ model.rs       # RawRecord, DirAgg, ROOT_FRN  (pure types)
├─ index.rs       # build_index() → by_frn + children maps  (pure, TDD)
├─ paths.rs       # path_for() parent-walk reconstruction  (pure, TDD)
├─ aggregate.rs   # aggregate() hardlink-dedup roll-up  (pure, TDD)
├─ tree.rs        # top_n_dirs() / top_n_files()  (pure, TDD)
├─ format.rs      # human(bytes)  (pure, TDD)
├─ volume.rs      # FFI: open volume, FSCTL geometry + MFT runlist, bulk read  (SPIKE)
└─ mft_scan.rs    # MftParser over the buffer → Vec<RawRecord>  (SPIKE)
```

**Pure modules (model/index/paths/aggregate/tree/format)** are fully testable with synthetic `RawRecord`s — no admin, no volume. **Spike modules (volume/mft_scan)** require `cargo build` to compile-check and an **elevated run on a real NTFS volume** to verify (raw volume reads can't be unit-tested).

---

### Task 1: Scaffold + dependencies

**Files:** Create `Cargo.toml`, `src/main.rs`, `src/lib.rs`

- [ ] **Step 1: Init**

Run from repo root: `cargo init --name disk_organizer`

- [ ] **Step 2: `Cargo.toml`**

```toml
[package]
name = "disk_organizer"
version = "0.1.0"
edition = "2021"

[dependencies]
clap = { version = "4", features = ["derive"] }
mft = { version = "0.7", default-features = false }

[target.'cfg(windows)'.dependencies]
windows = { version = "0.58", features = [
    "Win32_Foundation",
    "Win32_Storage_FileSystem",
    "Win32_System_Ioctl",
    "Win32_System_IO",
] }

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 3: `src/lib.rs`** (modules added as tasks land; start minimal)

```rust
pub mod format;
pub mod index;
pub mod model;
pub mod paths;
pub mod tree;
pub mod aggregate;
```

- [ ] **Step 4: `src/main.rs`** placeholder

```rust
fn main() {
    println!("disk_organizer (MFT scanner)");
}
```

- [ ] **Step 5: Build** — `cargo build` → Finished. (`mft` default-features off drops its `mft_dump` binary deps.)

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/
git commit -m "chore: scaffold MFT scanner project with deps"
```

> Note: `src/lib.rs` references modules created in later tasks; create empty `src/format.rs` etc. as `// placeholder` if you need an intermediate green build, or add the `pub mod` lines as each module lands. Keep the project compiling after every task.

---

### Task 2: Core types (`model.rs`)

**Files:** Create `src/model.rs`

- [ ] **Step 1: Write the types + a guard test**

```rust
/// MFT record number of the volume root directory.
pub const ROOT_FRN: u64 = 5;

/// One physical file/directory parsed from a single MFT record.
#[derive(Clone, Debug, PartialEq)]
pub struct RawRecord {
    pub frn: u64,          // file record number — the hardlink dedup key
    pub parent_frn: u64,   // parent dir FRN from the best (non-DOS) $FILE_NAME
    pub name: String,      // best name; DOS-only 8.3 names excluded
    pub is_dir: bool,
    pub is_reparse: bool,
    pub logical_size: u64, // unnamed $DATA logical size
    pub physical_size: u64,// on-disk allocated size
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_frn_is_five() {
        assert_eq!(ROOT_FRN, 5);
        assert_eq!(DirAgg::default(), DirAgg { logical_size: 0, physical_size: 0, file_count: 0 });
    }
}
```

- [ ] **Step 2:** `cargo test model` → pass.
- [ ] **Step 3: Commit** — `git add src/model.rs src/lib.rs && git commit -m "feat: core RawRecord/DirAgg types"`

---

### Task 3: Index build (`index.rs`)

**Files:** Create `src/index.rs`

- [ ] **Step 1: Write the failing test + impl**

```rust
use crate::model::RawRecord;
use std::collections::HashMap;

/// FRN-keyed lookup plus parent→children adjacency, built from a record sweep.
pub struct Index {
    pub by_frn: HashMap<u64, RawRecord>,
    pub children: HashMap<u64, Vec<u64>>,
}

pub fn build_index(records: Vec<RawRecord>) -> Index {
    let mut by_frn = HashMap::new();
    let mut children: HashMap<u64, Vec<u64>> = HashMap::new();
    for rec in records {
        children.entry(rec.parent_frn).or_default().push(rec.frn);
        by_frn.insert(rec.frn, rec);
    }
    Index { by_frn, children }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(frn: u64, parent: u64, name: &str) -> RawRecord {
        RawRecord { frn, parent_frn: parent, name: name.into(), is_dir: true,
            is_reparse: false, logical_size: 0, physical_size: 0, hard_link_count: 1, in_use: true }
    }
    fn file(frn: u64, parent: u64, name: &str, size: u64) -> RawRecord {
        RawRecord { frn, parent_frn: parent, name: name.into(), is_dir: false,
            is_reparse: false, logical_size: size, physical_size: size, hard_link_count: 1, in_use: true }
    }

    #[test]
    fn builds_lookup_and_children() {
        let idx = build_index(vec![
            dir(10, 5, "Users"),
            file(20, 10, "a.bin", 100),
            file(21, 10, "b.bin", 50),
        ]);
        assert_eq!(idx.by_frn.len(), 3);
        assert_eq!(idx.by_frn[&20].name, "a.bin");
        let mut kids = idx.children[&10].clone();
        kids.sort();
        assert_eq!(kids, vec![20, 21]);
    }
}
```

- [ ] **Step 2:** `cargo test index` → pass.
- [ ] **Step 3: Commit** — `git add src/index.rs && git commit -m "feat: FRN index + children map"`

---

### Task 4: Path reconstruction (`paths.rs`)

**Files:** Create `src/paths.rs`

- [ ] **Step 1: Write the failing test + impl**

```rust
use crate::index::Index;
use crate::model::ROOT_FRN;
use std::collections::HashMap;
use std::path::PathBuf;

/// Full path for `frn`, walking parent links to ROOT_FRN, memoized in `cache`.
/// Healthy MFT parent chains are acyclic; self-references and orphans terminate.
pub fn path_for(frn: u64, index: &Index, cache: &mut HashMap<u64, PathBuf>) -> PathBuf {
    if let Some(p) = cache.get(&frn) {
        return p.clone();
    }
    let path = match index.by_frn.get(&frn) {
        None => PathBuf::from(format!("<orphan:{frn}>")),
        Some(_) if frn == ROOT_FRN => PathBuf::from("\\"),
        Some(rec) if rec.parent_frn == frn => PathBuf::from(&rec.name), // self-ref guard
        Some(rec) => {
            let mut parent = path_for(rec.parent_frn, index, cache);
            parent.push(&rec.name);
            parent
        }
    };
    cache.insert(frn, path.clone());
    path
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::RawRecord;

    fn rec(frn: u64, parent: u64, name: &str, is_dir: bool) -> RawRecord {
        RawRecord { frn, parent_frn: parent, name: name.into(), is_dir,
            is_reparse: false, logical_size: 0, physical_size: 0, hard_link_count: 1, in_use: true }
    }

    #[test]
    fn reconstructs_full_path() {
        let idx = crate::index::build_index(vec![
            rec(ROOT_FRN, ROOT_FRN, "", true),
            rec(10, ROOT_FRN, "Users", true),
            rec(20, 10, "x.bin", false),
        ]);
        let mut cache = HashMap::new();
        assert_eq!(path_for(20, &idx, &mut cache), PathBuf::from("\\Users\\x.bin"));
        assert_eq!(path_for(10, &idx, &mut cache), PathBuf::from("\\Users"));
    }

    #[test]
    fn orphan_terminates() {
        let idx = crate::index::build_index(vec![rec(20, 999, "lost.bin", false)]);
        let mut cache = HashMap::new();
        let p = path_for(20, &idx, &mut cache);
        assert!(p.to_string_lossy().contains("orphan:999"));
    }
}
```

- [ ] **Step 2:** `cargo test paths` → pass.
- [ ] **Step 3: Commit** — `git add src/paths.rs && git commit -m "feat: parent-walk path reconstruction"`

---

### Task 5: Hardlink-dedup aggregation (`aggregate.rs`)

**Files:** Create `src/aggregate.rs`

- [ ] **Step 1: Write the failing test + impl**

```rust
use crate::index::Index;
use crate::model::{DirAgg, ROOT_FRN};
use std::collections::{HashMap, HashSet};

/// Roll each file's size up its ancestor directories. A given physical file
/// (FRN) contributes its physical_size only once across the whole tree
/// (hardlink dedup); logical_size is added along its parent chain.
pub fn aggregate(index: &Index) -> HashMap<u64, DirAgg> {
    let mut totals: HashMap<u64, DirAgg> = HashMap::new();
    let mut physical_seen: HashSet<u64> = HashSet::new();

    for (&frn, rec) in &index.by_frn {
        if rec.is_dir {
            continue;
        }
        let phys = if physical_seen.insert(frn) { rec.physical_size } else { 0 };

        let mut cur = rec.parent_frn;
        let mut guard = 0;
        loop {
            let agg = totals.entry(cur).or_default();
            agg.logical_size += rec.logical_size;
            agg.physical_size += phys;
            agg.file_count += 1;
            if cur == ROOT_FRN {
                break;
            }
            match index.by_frn.get(&cur) {
                Some(parent) if parent.parent_frn != cur => cur = parent.parent_frn,
                _ => break,
            }
            guard += 1;
            if guard > 4096 {
                break;
            }
        }
    }
    totals
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::RawRecord;

    fn dir(frn: u64, parent: u64) -> RawRecord {
        RawRecord { frn, parent_frn: parent, name: format!("d{frn}"), is_dir: true,
            is_reparse: false, logical_size: 0, physical_size: 0, hard_link_count: 1, in_use: true }
    }
    fn file(frn: u64, parent: u64, size: u64) -> RawRecord {
        RawRecord { frn, parent_frn: parent, name: format!("f{frn}"), is_dir: false,
            is_reparse: false, logical_size: size, physical_size: size, hard_link_count: 1, in_use: true }
    }

    #[test]
    fn rolls_sizes_up_the_tree() {
        // root(5) > Users(10) > {a(20)=100, b(21)=50}
        let idx = crate::index::build_index(vec![
            dir(10, ROOT_FRN), file(20, 10, 100), file(21, 10, 50),
        ]);
        let totals = aggregate(&idx);
        assert_eq!(totals[&10].physical_size, 150);
        assert_eq!(totals[&10].file_count, 2);
        assert_eq!(totals[&ROOT_FRN].physical_size, 150); // bubbles to root
    }
}
```

> Hardlink note: with one `RawRecord` per FRN (the M1a first cut), each physical file is counted exactly once because we iterate MFT records, not directory links — so WinSxS cannot over-count. The `physical_seen` set is the safety net for a later refinement that places a hardlinked file under each of its parents.

- [ ] **Step 2:** `cargo test aggregate` → pass.
- [ ] **Step 3: Commit** — `git add src/aggregate.rs && git commit -m "feat: hardlink-dedup tree aggregation"`

---

### Task 6: Top-N selection (`tree.rs`)

**Files:** Create `src/tree.rs`

- [ ] **Step 1: Write the failing test + impl**

```rust
use crate::index::Index;
use crate::model::{DirAgg, RawRecord};
use std::collections::HashMap;

/// Directories with physical total >= `min_physical`, largest first, capped at `n`.
pub fn top_n_dirs(totals: &HashMap<u64, DirAgg>, min_physical: u64, n: usize) -> Vec<(u64, DirAgg)> {
    let mut v: Vec<(u64, DirAgg)> = totals
        .iter()
        .filter(|(_, a)| a.physical_size >= min_physical)
        .map(|(frn, a)| (*frn, a.clone()))
        .collect();
    v.sort_by(|a, b| b.1.physical_size.cmp(&a.1.physical_size));
    v.truncate(n);
    v
}

/// Files with physical size >= `min_physical`, largest first, capped at `n`.
pub fn top_n_files(index: &Index, min_physical: u64, n: usize) -> Vec<(u64, RawRecord)> {
    let mut v: Vec<(u64, RawRecord)> = index
        .by_frn
        .iter()
        .filter(|(_, r)| !r.is_dir && r.physical_size >= min_physical)
        .map(|(frn, r)| (*frn, r.clone()))
        .collect();
    v.sort_by(|a, b| b.1.physical_size.cmp(&a.1.physical_size));
    v.truncate(n);
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ROOT_FRN;

    fn file(frn: u64, parent: u64, size: u64) -> RawRecord {
        RawRecord { frn, parent_frn: parent, name: format!("f{frn}"), is_dir: false,
            is_reparse: false, logical_size: size, physical_size: size, hard_link_count: 1, in_use: true }
    }

    #[test]
    fn dirs_sorted_filtered_capped() {
        let mut totals = HashMap::new();
        totals.insert(10u64, DirAgg { logical_size: 0, physical_size: 1000, file_count: 1 });
        totals.insert(11u64, DirAgg { logical_size: 0, physical_size: 200, file_count: 1 });
        totals.insert(12u64, DirAgg { logical_size: 0, physical_size: 5, file_count: 1 });
        let top = top_n_dirs(&totals, 100, 2);
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].0, 10);
        assert!(top.iter().all(|(_, a)| a.physical_size >= 100));
    }

    #[test]
    fn files_sorted_filtered_capped() {
        let idx = crate::index::build_index(vec![
            file(20, ROOT_FRN, 1000), file(21, ROOT_FRN, 200), file(22, ROOT_FRN, 5),
        ]);
        let top = top_n_files(&idx, 100, 5);
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].1.physical_size, 1000);
    }
}
```

- [ ] **Step 2:** `cargo test tree` → pass.
- [ ] **Step 3: Commit** — `git add src/tree.rs && git commit -m "feat: top-N dirs and files"`

---

### Task 7: Size formatting (`format.rs`)

**Files:** Create `src/format.rs`

- [ ] **Step 1: Write the failing test + impl**

```rust
/// Format a byte count in binary units.
pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut idx = 0;
    while size >= 1024.0 && idx < UNITS.len() - 1 {
        size /= 1024.0;
        idx += 1;
    }
    if idx == 0 {
        format!("{} {}", bytes, UNITS[0])
    } else {
        format!("{:.1} {}", size, UNITS[idx])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_sizes() {
        assert_eq!(human(0), "0 B");
        assert_eq!(human(512), "512 B");
        assert_eq!(human(1536), "1.5 KB");
        assert_eq!(human(1024 * 1024), "1.0 MB");
    }
}
```

- [ ] **Step 2:** `cargo test format` → pass.
- [ ] **Step 3: Commit** — `git add src/format.rs && git commit -m "feat: human-readable byte formatting"`

> Checkpoint: Tasks 1–7 give a fully-tested pure pipeline (`Vec<RawRecord>` → printed Top-N). The remaining tasks fill in the FFI that produces `Vec<RawRecord>`.

---

### Task 8: Volume FFI — bulk-read the MFT (`volume.rs`) — SPIKE

This task is a **spike**: write it, `cargo build` to compile-check, and verify by an **elevated run** in Task 10 (raw volume reads can't be unit-tested). Follow the algorithm in [synthesis §Algorithm step 1](../research/2026-06-08-mft-approach-synthesis.md). Use the cloned `references/windirstat/.../FinderNtfs.cpp` (`FinderNtfsContext::LoadRoot`) as the line-by-line reference.

**Files:** Create `src/volume.rs`; add `#[cfg(windows)] pub mod volume;` to `src/lib.rs`.

- [ ] **Step 1: Implement `read_mft`** returning the MFT bytes + record size:

Target signature and behavior (fill in exact `windows` crate paths from docs.rs/`cargo build` errors — the crate's module layout is the only uncertain part):

```rust
/// Geometry + raw MFT bytes for a volume.
pub struct MftImage {
    pub bytes: Vec<u8>,
    pub record_size: usize, // BytesPerFileRecordSegment (usually 1024)
}

/// Open `\\.\<drive>:`, read NTFS geometry and the (possibly fragmented) $MFT
/// into memory. Requires Administrator. `drive` is like "C".
pub fn read_mft(drive: &str) -> std::io::Result<MftImage> {
    // 1. CreateFileW("\\.\C:", FILE_READ_DATA|FILE_READ_ATTRIBUTES|SYNCHRONIZE,
    //    FILE_SHARE_READ|WRITE|DELETE, OPEN_EXISTING, FILE_FLAG_NO_BUFFERING).
    // 2. DeviceIoControl(FSCTL_GET_NTFS_VOLUME_DATA) -> NTFS_VOLUME_DATA_BUFFER:
    //    bytes_per_cluster, bytes_per_file_record_segment.
    // 3. Open "\\.\C:\$MFT", DeviceIoControl(FSCTL_GET_RETRIEVAL_POINTERS,
    //    STARTING_VCN_INPUT_BUFFER{0}) -> RETRIEVAL_POINTERS_BUFFER; loop while
    //    GetLastError()==ERROR_MORE_DATA, doubling the output buffer.
    // 4. extents -> (start_lcn, cluster_count); cluster_count =
    //    next_vcn[i] - (i==0 ? starting_vcn : next_vcn[i-1]).
    // 5. For each extent: SetFilePointerEx(start_lcn*bytes_per_cluster) on the
    //    VOLUME handle, ReadFile cluster_count*bytes_per_cluster bytes (chunk in
    //    4 MiB, buffers must be sector-aligned for FILE_FLAG_NO_BUFFERING).
    // 6. Concatenate extents in VCN order -> contiguous MFT image. record_size =
    //    bytes_per_file_record_segment.
    unimplemented!()
}
```

- [ ] **Step 2: Compile** — `cargo build`. Resolve `windows` API names against build errors / docs.rs until it builds clean. Keep all `unsafe` blocks minimal and commented.

- [ ] **Step 3: Smoke-print behind a hidden flag** — temporarily, in `main.rs`, if argv contains `--dump-mft-len`, call `volume::read_mft` and `eprintln!` the byte length + record_size. (Removed/folded into the real CLI in Task 10.)

- [ ] **Step 4: Verify elevated** — in an **Administrator** PowerShell: `cargo run -- C --dump-mft-len`. Expected: a non-zero MFT byte length that is a multiple of record_size, no panic. A non-elevated run is expected to fail at `CreateFileW` — confirm it errors cleanly rather than panicking.

- [ ] **Step 5: Commit** — `git add src/volume.rs src/lib.rs src/main.rs && git commit -m "feat: FSCTL volume layer to bulk-read the MFT (spike)"`

---

### Task 9: Parse records via `mft` (`mft_scan.rs`) — SPIKE

**Files:** Create `src/mft_scan.rs`; add `pub mod mft_scan;` to `src/lib.rs`.

Follow [synthesis §Algorithm steps 2–4](../research/2026-06-08-mft-approach-synthesis.md). `MftParser::from_buffer(bytes)` treats the buffer as a contiguous MFT image and numbers records by buffer index — which equals the FRN, since we read `$MFT` in VCN order from 0.

- [ ] **Step 1: Implement `parse_records`**

```rust
use crate::model::RawRecord;

/// Parse a contiguous MFT image into RawRecords (one per in-use FILE record).
pub fn parse_records(mft_bytes: Vec<u8>) -> Vec<RawRecord> {
    // let mut parser = mft::MftParser::from_buffer(mft_bytes).expect(...);
    // for entry in parser.iter_entries() {
    //     let entry = match entry { Ok(e) => e, Err(_) => continue };
    //     if !entry.is_allocated() { continue; }            // in-use only
    //     if entry.header.base_reference.entry != 0 { continue; } // skip extension records (first cut)
    //     let frn = entry.header.record_number;
    //     let name_attr = match entry.find_best_name_attribute() { Some(n) => n, None => continue };
    //     // name_attr.namespace != DOS is guaranteed by find_best_name_attribute
    //     let parent_frn = name_attr.parent.entry;
    //     let name = name_attr.name; // already String/utf8
    //     let is_dir = entry.is_dir();
    //     // size: iterate $DATA attributes, take the unnamed one; logical vs allocated
    //     //   from the (non)resident header (synthesis step 4).
    //     // is_reparse: STANDARD_INFORMATION/FILE_NAME flags & 0x400.
    //     // hard_link_count: entry.header.hard_link_count.
    //     // push RawRecord { ... in_use: true }
    // }
    unimplemented!()
}
```

- [ ] **Step 2: Compile** — `cargo build`; resolve exact `mft` accessor names (`entry.header.*`, `find_best_name_attribute()`, `iter_attributes()` / attribute header `file_size`/`allocated_length`, namespace enum) against build errors and the cloned `references/omerbenamram-mft/src/` (`entry.rs`, `attribute/x30.rs`, `attribute/header.rs`).

- [ ] **Step 3: Verify elevated** — wire a temporary `eprintln!` of the first 10 RawRecords' (name, frn, parent_frn, physical_size) and run `cargo run -- C` as Administrator. Expect plausible file names and sizes.

- [ ] **Step 4: Commit** — `git add src/mft_scan.rs src/lib.rs && git commit -m "feat: parse MFT buffer into RawRecords via mft crate (spike)"`

---

### Task 10: CLI end-to-end (`main.rs`)

**Files:** Modify `src/main.rs`

- [ ] **Step 1: Wire the pipeline**

```rust
use clap::Parser;
use disk_organizer::aggregate::aggregate;
use disk_organizer::format::human;
use disk_organizer::index::build_index;
use disk_organizer::paths::path_for;
use disk_organizer::tree::{top_n_dirs, top_n_files};
use std::collections::HashMap;

#[derive(Parser)]
#[command(name = "disk_organizer", about = "Find the largest dirs/files on an NTFS volume via the MFT")]
struct Args {
    /// Drive letter to scan, e.g. C
    drive: String,
    #[arg(long, default_value_t = 30)]
    top: usize,
    #[arg(long, default_value_t = 100)]
    min_size_mb: u64,
}

fn main() -> std::io::Result<()> {
    let args = Args::parse();
    let min = args.min_size_mb * 1024 * 1024;

    eprintln!("Reading MFT for {}: (requires Administrator) ...", args.drive);
    let image = disk_organizer::volume::read_mft(&args.drive)?;
    let records = disk_organizer::mft_scan::parse_records(image.bytes);
    eprintln!("Parsed {} records.", records.len());

    let index = build_index(records);
    let totals = aggregate(&index);
    let mut cache = HashMap::new();

    println!("\n== Top {} directories (on-disk, hardlink-deduped) ==", args.top);
    for (frn, agg) in top_n_dirs(&totals, min, args.top) {
        println!("{:>10}  {:>8} files  {}", human(agg.physical_size), agg.file_count,
            path_for(frn, &index, &mut cache).display());
    }

    println!("\n== Top {} files ==", args.top);
    for (frn, rec) in top_n_files(&index, min, args.top) {
        println!("{:>10}  {}", human(rec.physical_size), path_for(frn, &index, &mut cache).display());
    }
    Ok(())
}
```

- [ ] **Step 2: Build + test** — `cargo build` and `cargo test` (pure tests still green).

- [ ] **Step 3: Verify elevated end-to-end** — Administrator PowerShell: `cargo run --release -- C --top 20 --min-size-mb 100`. Expect two ranked tables of real paths/sizes.

- [ ] **Step 4: Verify hardlink correctness (the core criterion)** — compare the reported `\Windows\WinSxS` total against Explorer's Properties for WinSxS. Ours should be **substantially smaller** (Explorer counts hardlinks repeatedly; we count each FRN once). Also sanity-check the volume root total is close to "used space" in Explorer.

- [ ] **Step 5: Commit** — `git add src/main.rs && git commit -m "feat: MFT scanner CLI end-to-end"`

---

## Self-Review

**Spec coverage (functional F1/F2 + synthesis algorithm):**
- F1 fast scan → Tasks 8–9 (bulk MFT read + parse). ✓
- F2 hardlink correctness → Tasks 5 + 9 (one FRN counted once; iterate records not links). ✓
- F2 reparse handling → Task 9 sets `is_reparse`; aggregation can skip descent later. ✓ (first cut records the flag; not-descending is trivial since we never follow links)
- Top-N dirs+files → Tasks 6 + 10. ✓
- Path reconstruction → Task 4. ✓
- Admin/NTFS requirement + fallback → documented; jwalk fallback deferred. ✓

**Placeholder scan:** Pure tasks (2–7) contain complete, compilable code with tests. Spike tasks (8–9) intentionally use `unimplemented!()` skeletons with the precise algorithm + reference citations + a compile/elevated-run verification loop — appropriate because raw-volume FFI must be resolved against the compiler and a live volume, not pre-asserted. This is a deliberate spike, not a vague placeholder.

**Type consistency:** `RawRecord` / `DirAgg` (model.rs) and `Index` (index.rs) are used identically across index/paths/aggregate/tree/main. `read_mft → MftImage{bytes, record_size}` feeds `parse_records(bytes) → Vec<RawRecord>` feeds `build_index → aggregate/top_n`. `path_for(frn, &index, &mut cache)` signature matches all call sites.

**Known first-cut simplifications (documented, revisit later):**
- Extension records (`base_reference != 0`) skipped — rare; may under-count very fragmented files. 
- A hardlinked file is placed under its single best name's parent (not under all parents) — still size-correct (counted once), just attributed to one location.
- ADS excluded from size (matches WinDirStat default).
- No non-NTFS / no-admin fallback yet (jwalk plan, deferred).
