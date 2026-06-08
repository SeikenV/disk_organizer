# M1a Scanner Core Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a correct, parallel disk-size scanner CLI that prints the Top-N largest directories and files on a Windows NTFS path, with hardlink dedup and reparse-point skipping (so WinSxS does not over-count).

**Architecture:** A `jwalk` parallel walk produces a flat stream of file entries. We skip reparse-point directories (no descent) and deduplicate hardlinked files (by `same-file::Handle`, gated to files ≥ a size threshold). Files are aggregated into per-directory subtree totals via ancestor accumulation. The CLI prints Top-N directories (by subtree total) and Top-N files (by size).

**Tech Stack:** Rust (edition 2021), `jwalk` (parallel walk), `same-file` (file identity / hardlink dedup), `clap` (CLI), `tempfile` (dev-only test fixtures). Windows-only target.

---

## File Structure

```
disk_organizer/
├─ Cargo.toml
├─ src/
│  ├─ main.rs        # CLI entry: parse args, run scan, print tables
│  ├─ model.rs       # FileEntry, DirAgg, ScanResult (shared types)
│  ├─ attrs.rs       # is_reparse_attr(u32) -> bool  (pure, cross-platform)
│  ├─ identity.rs    # Deduper (HashSet<same_file::Handle>) hardlink dedup
│  ├─ tree.rs        # aggregate(), top_n_dirs(), top_n_files()  (pure)
│  ├─ format.rs      # human(bytes) -> String  (pure)
│  └─ scanner.rs     # scan(root, threshold) -> ScanResult  (jwalk + windows attrs)
└─ tests/
   └─ scan_integration.rs  # end-to-end scan over a temp fixture tree
```

Responsibilities:
- `model.rs` — plain data types shared across modules. No logic.
- `attrs.rs`, `tree.rs`, `format.rs` — **pure** functions, unit-tested without touching the filesystem.
- `identity.rs` — filesystem identity; tested with a real hardlink fixture.
- `scanner.rs` — the only module using Windows-specific metadata + jwalk; tested via integration test.
- `main.rs` — wiring only.

---

### Task 1: Scaffold cargo project + dependencies

**Files:**
- Create: `Cargo.toml`
- Create: `src/main.rs`

- [ ] **Step 1: Initialize the cargo binary project**

Run (from repo root `c:/Users/dongm/github/disk_organizer`):
```
cargo init --name disk_organizer
```
Expected: creates `Cargo.toml` and `src/main.rs` with a hello-world.

- [ ] **Step 2: Set dependencies in `Cargo.toml`**

Replace the `[dependencies]` section (and add `[dev-dependencies]`) so the file reads:

```toml
[package]
name = "disk_organizer"
version = "0.1.0"
edition = "2021"

[dependencies]
jwalk = "0.8"
same-file = "1.0"
clap = { version = "4", features = ["derive"] }

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 3: Replace `src/main.rs` with a minimal placeholder**

```rust
fn main() {
    println!("disk_organizer scanner");
}
```

- [ ] **Step 4: Verify it builds**

Run: `cargo build`
Expected: `Finished` with no errors (dependencies download on first run).

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/main.rs
git commit -m "chore: scaffold disk_organizer cargo project with deps"
```

---

### Task 2: Reparse-point bit check (`attrs.rs`)

`FILE_ATTRIBUTE_REPARSE_POINT = 0x400`. Junctions and symlinks set this bit; we must not descend into them.

**Files:**
- Create: `src/attrs.rs`
- Modify: `src/main.rs` (declare module)

- [ ] **Step 1: Write the failing test**

Create `src/attrs.rs`:

```rust
/// Windows file attribute bit indicating a reparse point (junction/symlink).
pub const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

/// Returns true if the given Windows file-attributes bitmask marks a reparse point.
pub fn is_reparse_attr(attrs: u32) -> bool {
    attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_reparse_bit() {
        assert!(is_reparse_attr(0x400));
        assert!(is_reparse_attr(0x410)); // reparse + other bits
    }

    #[test]
    fn ignores_when_bit_absent() {
        assert!(!is_reparse_attr(0x0));
        assert!(!is_reparse_attr(0x10)); // directory, not reparse
    }
}
```

Add to top of `src/main.rs`:
```rust
mod attrs;
```

- [ ] **Step 2: Run test to verify it passes (logic is trivial; this guards regressions)**

Run: `cargo test attrs`
Expected: 2 tests pass.

- [ ] **Step 3: Commit**

```bash
git add src/attrs.rs src/main.rs
git commit -m "feat: reparse-point attribute detection"
```

---

### Task 3: Hardlink dedup (`identity.rs`)

**Files:**
- Create: `src/identity.rs`
- Modify: `src/main.rs` (declare module)

- [ ] **Step 1: Write the failing test**

Create `src/identity.rs`:

```rust
use same_file::Handle;
use std::collections::HashSet;
use std::path::Path;

/// Tracks file identities so a hardlinked file is counted only once.
pub struct Deduper {
    seen: HashSet<Handle>,
}

impl Deduper {
    pub fn new() -> Self {
        Deduper { seen: HashSet::new() }
    }

    /// Returns true the first time a given physical file is seen, false for
    /// subsequent hardlinks to the same file. If the handle cannot be opened
    /// (permissions, locked), defaults to counting it (true) to avoid undercount.
    pub fn should_count(&mut self, path: &Path) -> bool {
        match Handle::from_path(path) {
            Ok(handle) => self.seen.insert(handle),
            Err(_) => true,
        }
    }
}

impl Default for Deduper {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn counts_distinct_files_once_each() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.bin");
        let c = dir.path().join("c.bin");
        fs::write(&a, vec![0u8; 1024]).unwrap();
        fs::write(&c, b"different").unwrap();

        let mut d = Deduper::new();
        assert!(d.should_count(&a));
        assert!(d.should_count(&c));
    }

    #[test]
    fn dedups_hardlink_to_same_file() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.bin");
        let b = dir.path().join("b.bin");
        fs::write(&a, vec![0u8; 1024]).unwrap();
        fs::hard_link(&a, &b).unwrap();

        let mut d = Deduper::new();
        assert!(d.should_count(&a)); // first physical file
        assert!(!d.should_count(&b)); // same file via hardlink -> skip
    }
}
```

Add to `src/main.rs`:
```rust
mod identity;
```

- [ ] **Step 2: Run test to verify it passes**

Run: `cargo test identity`
Expected: 2 tests pass. (If `dedups_hardlink_to_same_file` fails with both `true`, the temp dir is on a filesystem that doesn't share file IDs — rerun on an NTFS path.)

- [ ] **Step 3: Commit**

```bash
git add src/identity.rs src/main.rs
git commit -m "feat: hardlink dedup via same-file Handle"
```

---

### Task 4: Shared types + directory aggregation (`model.rs`, `tree.rs`)

**Files:**
- Create: `src/model.rs`
- Create: `src/tree.rs`
- Modify: `src/main.rs` (declare modules)

- [ ] **Step 1: Create the shared types**

Create `src/model.rs`:

```rust
use std::collections::HashMap;
use std::path::PathBuf;

/// A single counted file (already hardlink-deduplicated).
#[derive(Clone, Debug, PartialEq)]
pub struct FileEntry {
    pub path: PathBuf,
    pub size: u64,
}

/// Aggregated totals for a directory's whole subtree.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DirAgg {
    pub total_size: u64,
    pub file_count: u64,
}

/// Output of a scan: every counted file, plus per-directory subtree totals.
#[derive(Debug)]
pub struct ScanResult {
    pub files: Vec<FileEntry>,
    pub dir_totals: HashMap<PathBuf, DirAgg>,
}
```

Add to `src/main.rs`:
```rust
mod model;
mod tree;
```

- [ ] **Step 2: Write the failing test for `aggregate`**

Create `src/tree.rs`:

```rust
use crate::model::{DirAgg, FileEntry};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Accumulate each file's size into every ancestor directory up to (and
/// including) `root`. Ancestors above `root` are ignored.
pub fn aggregate(root: &Path, files: &[FileEntry]) -> HashMap<PathBuf, DirAgg> {
    let mut map: HashMap<PathBuf, DirAgg> = HashMap::new();
    for f in files {
        let mut cur = f.path.parent();
        while let Some(dir) = cur {
            if !dir.starts_with(root) {
                break;
            }
            let agg = map.entry(dir.to_path_buf()).or_default();
            agg.total_size += f.size;
            agg.file_count += 1;
            if dir == root {
                break;
            }
            cur = dir.parent();
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fe(path: PathBuf, size: u64) -> FileEntry {
        FileEntry { path, size }
    }

    #[test]
    fn aggregates_sizes_up_the_tree() {
        let root = PathBuf::from("root");
        let files = vec![
            fe(root.join("a").join("x.bin"), 100),
            fe(root.join("a").join("y.bin"), 50),
            fe(root.join("b").join("z.bin"), 30),
        ];
        let m = aggregate(&root, &files);

        assert_eq!(m[&root].total_size, 180);
        assert_eq!(m[&root].file_count, 3);
        assert_eq!(m[&root.join("a")].total_size, 150);
        assert_eq!(m[&root.join("a")].file_count, 2);
        assert_eq!(m[&root.join("b")].total_size, 30);
    }
}
```

- [ ] **Step 3: Run test to verify it passes**

Run: `cargo test tree::tests::aggregates_sizes_up_the_tree`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add src/model.rs src/tree.rs src/main.rs
git commit -m "feat: directory subtree aggregation"
```

---

### Task 5: Top-N selection (`tree.rs`)

**Files:**
- Modify: `src/tree.rs`

- [ ] **Step 1: Write the failing test (append to `src/tree.rs` above the `#[cfg(test)]` for the new functions, then add tests inside the existing `tests` module)**

Add these functions to `src/tree.rs` (after `aggregate`):

```rust
/// Directories with subtree total >= `min_size`, largest first, capped at `n`.
pub fn top_n_dirs(map: &HashMap<PathBuf, DirAgg>, min_size: u64, n: usize) -> Vec<(PathBuf, DirAgg)> {
    let mut v: Vec<(PathBuf, DirAgg)> = map
        .iter()
        .filter(|(_, a)| a.total_size >= min_size)
        .map(|(p, a)| (p.clone(), a.clone()))
        .collect();
    v.sort_by(|a, b| b.1.total_size.cmp(&a.1.total_size));
    v.truncate(n);
    v
}

/// Files with size >= `min_size`, largest first, capped at `n`.
pub fn top_n_files(files: &[FileEntry], min_size: u64, n: usize) -> Vec<FileEntry> {
    let mut v: Vec<FileEntry> = files.iter().filter(|f| f.size >= min_size).cloned().collect();
    v.sort_by(|a, b| b.size.cmp(&a.size));
    v.truncate(n);
    v
}
```

Add these tests inside the existing `mod tests` block in `src/tree.rs`:

```rust
    #[test]
    fn top_dirs_sorted_filtered_capped() {
        let root = PathBuf::from("root");
        let files = vec![
            fe(root.join("big").join("x.bin"), 1000),
            fe(root.join("mid").join("y.bin"), 200),
            fe(root.join("small").join("z.bin"), 5),
        ];
        let m = aggregate(&root, &files);
        let top = top_n_dirs(&m, 100, 2);

        assert_eq!(top.len(), 2);
        assert_eq!(top[0].0, root); // root holds everything -> largest
        assert!(top[0].1.total_size >= top[1].1.total_size);
        assert!(top.iter().all(|(_, a)| a.total_size >= 100));
    }

    #[test]
    fn top_files_sorted_filtered_capped() {
        let files = vec![
            fe(PathBuf::from("a"), 1000),
            fe(PathBuf::from("b"), 200),
            fe(PathBuf::from("c"), 5),
        ];
        let top = top_n_files(&files, 100, 5);

        assert_eq!(top.len(), 2);
        assert_eq!(top[0].size, 1000);
        assert_eq!(top[1].size, 200);
    }
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test tree`
Expected: all `tree` tests pass.

- [ ] **Step 3: Commit**

```bash
git add src/tree.rs
git commit -m "feat: top-N directory and file selection"
```

---

### Task 6: Human-readable size formatting (`format.rs`)

**Files:**
- Create: `src/format.rs`
- Modify: `src/main.rs` (declare module)

- [ ] **Step 1: Write the failing test**

Create `src/format.rs`:

```rust
/// Format a byte count as a human-readable string (binary units).
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
    fn formats_bytes() {
        assert_eq!(human(0), "0 B");
        assert_eq!(human(512), "512 B");
    }

    #[test]
    fn formats_larger_units() {
        assert_eq!(human(1536), "1.5 KB");
        assert_eq!(human(1024 * 1024), "1.0 MB");
        assert_eq!(human(3 * 1024 * 1024 * 1024), "3.0 GB");
    }
}
```

Add to `src/main.rs`:
```rust
mod format;
```

- [ ] **Step 2: Run test to verify it passes**

Run: `cargo test format`
Expected: 2 tests pass.

- [ ] **Step 3: Commit**

```bash
git add src/format.rs src/main.rs
git commit -m "feat: human-readable byte formatting"
```

---

### Task 7: The scanner (`scanner.rs`)

This is the only module using Windows-specific metadata. It walks in parallel, prunes reparse-point directories before descending, skips reparse-point files, dedups large hardlinks, and aggregates.

**Files:**
- Create: `src/scanner.rs`
- Modify: `src/main.rs` (declare module)

- [ ] **Step 1: Write the scanner**

Create `src/scanner.rs`:

```rust
use crate::attrs::is_reparse_attr;
use crate::identity::Deduper;
use crate::model::{FileEntry, ScanResult};
use crate::tree::aggregate;
use jwalk::{ClientState, DirEntry, WalkDir};
use std::os::windows::fs::MetadataExt;
use std::path::Path;

/// Files at or above this size are checked for hardlink duplication.
/// Smaller files are always counted (handle opens for millions of tiny files
/// would dominate runtime, and their double-counting impact is negligible).
pub const DEFAULT_IDENTITY_THRESHOLD: u64 = 1024 * 1024; // 1 MiB

fn is_reparse_dirent<C: ClientState>(entry: &DirEntry<C>) -> bool {
    entry
        .metadata()
        .ok()
        .map(|m| is_reparse_attr(m.file_attributes()))
        .unwrap_or(false)
}

/// Scan `root`, returning every counted file and per-directory subtree totals.
pub fn scan(root: &Path, identity_threshold: u64) -> ScanResult {
    let mut deduper = Deduper::new();
    let mut files: Vec<FileEntry> = Vec::new();

    let walker = WalkDir::new(root)
        .skip_hidden(false)
        .process_read_dir(|_depth, _path, _state, children| {
            // Do not descend into reparse points (junctions/symlinks).
            children.retain(|res| match res {
                Ok(entry) => !(entry.file_type().is_dir() && is_reparse_dirent(entry)),
                Err(_) => true,
            });
        });

    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue, // permission denied / vanished: skip
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if is_reparse_attr(meta.file_attributes()) {
            continue; // skip reparse-point files
        }
        let size = meta.len();
        let count = if size >= identity_threshold {
            deduper.should_count(&path)
        } else {
            true
        };
        if count {
            files.push(FileEntry { path, size });
        }
    }

    let dir_totals = aggregate(root, &files);
    ScanResult { files, dir_totals }
}
```

Add to `src/main.rs`:
```rust
mod scanner;
```

- [ ] **Step 2: Build to verify it compiles**

Run: `cargo build`
Expected: `Finished` with no errors. (If `process_read_dir`'s closure types fail to infer, the cause is usually a stray type annotation — leave the four params untyped as shown.)

- [ ] **Step 3: Write the integration test**

Create `tests/scan_integration.rs`:

```rust
use disk_organizer::model::ScanResult;
use disk_organizer::scanner::scan;
use std::fs;

// Helper: total bytes counted across all files in the result.
fn total(result: &ScanResult) -> u64 {
    result.files.iter().map(|f| f.size).sum()
}

#[test]
fn scans_and_aggregates_a_fixture_tree() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    fs::create_dir_all(root.join("a")).unwrap();
    fs::create_dir_all(root.join("b")).unwrap();
    fs::write(root.join("a").join("x.bin"), vec![0u8; 4096]).unwrap();
    fs::write(root.join("a").join("y.bin"), vec![0u8; 2048]).unwrap();
    fs::write(root.join("b").join("z.bin"), vec![0u8; 1024]).unwrap();

    // identity_threshold = 0 so dedup logic is exercised on every file.
    let result = scan(root, 0);

    assert_eq!(result.files.len(), 3);
    assert_eq!(total(&result), 4096 + 2048 + 1024);
    assert_eq!(result.dir_totals[&root.to_path_buf()].total_size, 7168);
    assert_eq!(result.dir_totals[&root.join("a")].total_size, 6144);
    assert_eq!(result.dir_totals[&root.join("b")].total_size, 1024);
}

#[test]
fn hardlinked_large_file_counted_once() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let a = root.join("a.bin");
    let b = root.join("b.bin");
    fs::write(&a, vec![0u8; 2 * 1024 * 1024]).unwrap(); // 2 MiB, above threshold
    fs::hard_link(&a, &b).unwrap();

    let result = scan(root, 1024 * 1024); // 1 MiB threshold

    // Only one of the two hardlinks is counted.
    assert_eq!(result.files.len(), 1);
    assert_eq!(total(&result), 2 * 1024 * 1024);
}
```

- [ ] **Step 4: Expose modules as a library so the integration test can import them**

The integration test imports `disk_organizer::scanner` / `disk_organizer::model`. Add a library target alongside the binary. Create `src/lib.rs`:

```rust
pub mod attrs;
pub mod format;
pub mod identity;
pub mod model;
pub mod scanner;
pub mod tree;
```

Then change `src/main.rs` so it uses the library crate instead of re-declaring modules. Replace the `mod ...;` lines at the top of `src/main.rs` with:

```rust
use disk_organizer::format::human;
use disk_organizer::scanner::{scan, DEFAULT_IDENTITY_THRESHOLD};
use disk_organizer::tree::{top_n_dirs, top_n_files};
```

(Keep the rest of `main` as the placeholder for now; Task 8 fills it in.)

- [ ] **Step 5: Run all tests**

Run: `cargo test`
Expected: all unit tests + both integration tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/scanner.rs src/lib.rs src/main.rs tests/scan_integration.rs
git commit -m "feat: parallel scanner with reparse skip + hardlink dedup"
```

---

### Task 8: CLI wiring (`main.rs`)

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 1: Implement the CLI**

Replace the entire contents of `src/main.rs` with:

```rust
use clap::Parser;
use disk_organizer::format::human;
use disk_organizer::scanner::{scan, DEFAULT_IDENTITY_THRESHOLD};
use disk_organizer::tree::{top_n_dirs, top_n_files};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "disk_organizer", about = "Find the largest directories and files on a drive")]
struct Args {
    /// Path to scan, e.g. C:\
    path: PathBuf,

    /// How many entries to show in each table
    #[arg(long, default_value_t = 30)]
    top: usize,

    /// Minimum size (in MB) for an entry to be listed
    #[arg(long, default_value_t = 100)]
    min_size_mb: u64,
}

fn main() {
    let args = Args::parse();
    let min_size = args.min_size_mb * 1024 * 1024;

    eprintln!("Scanning {} ...", args.path.display());
    let result = scan(&args.path, DEFAULT_IDENTITY_THRESHOLD);
    eprintln!("Counted {} files.", result.files.len());

    println!("\n== Top {} directories (subtree total) ==", args.top);
    for (path, agg) in top_n_dirs(&result.dir_totals, min_size, args.top) {
        println!(
            "{:>10}  {:>8} files  {}",
            human(agg.total_size),
            agg.file_count,
            path.display()
        );
    }

    println!("\n== Top {} files ==", args.top);
    for f in top_n_files(&result.files, min_size, args.top) {
        println!("{:>10}  {}", human(f.size), f.path.display());
    }
}
```

- [ ] **Step 2: Build and run the test suite**

Run: `cargo test`
Expected: all tests still pass (CLI change does not break library tests).

- [ ] **Step 3: Manually run against a small real folder**

Run: `cargo run --release -- "C:\Users" --top 15 --min-size-mb 50`
Expected: prints a "Top directories" table and a "Top files" table; runs without panicking. Permission-denied subfolders are silently skipped.

- [ ] **Step 4: Sanity-check WinSxS does not over-count (the core success criterion)**

Run (PowerShell, as Administrator so the folder is readable):
```
cargo run --release -- "C:\Windows\WinSxS" --top 5 --min-size-mb 50
```
Expected: the reported total for `WinSxS` is far smaller than the figure Explorer's "Properties" shows (Explorer counts hardlinks repeatedly; we dedup files ≥ 1 MiB). If the number looks inflated to match Explorer, hardlink dedup is not engaging — recheck Task 3 / Task 7.

- [ ] **Step 5: Commit**

```bash
git add src/main.rs
git commit -m "feat: CLI for top directories and files"
```

---

## Self-Review

**Spec coverage (against architecture doc §3 components for M1a + functional F1/F2):**
- F1 parallel scan → Task 7 (`jwalk`, parallel by default). ✓
- F2 hardlink dedup → Tasks 3 + 7 (size-gated `same-file::Handle`). ✓
- F2 reparse skip → Tasks 2 + 7 (`process_read_dir` prune + per-file skip). ✓
- F2 permission/locked tolerance → Task 7 (`Err(_) => continue`). ✓
- C2 tree aggregation → Tasks 4–5. ✓
- CLI Top-N → Task 8. ✓
- Success criterion "WinSxS not over-count" → Task 8 Step 4 verification. ✓
- **Out of scope for M1a (deferred to M1b plan):** catalog, recursive cut, classification, risk, selection, delete, snapshot. Long-path `\\?\` handling is also deferred — jwalk handles most paths; the `\\?\` retry lands in M1b alongside robust error reporting.

**Placeholder scan:** No TBD/TODO. Every code step shows complete code. The Task 7 Step 4 note about modifying `main.rs` is fully specified.

**Type consistency:** `FileEntry { path, size }`, `DirAgg { total_size, file_count }`, `ScanResult { files, dir_totals }` are defined once in `model.rs` (Task 4) and used identically in `tree.rs`, `scanner.rs`, integration tests, and `main.rs`. `scan(root, identity_threshold)`, `top_n_dirs(map, min_size, n)`, `top_n_files(files, min_size, n)`, `human(bytes)`, `is_reparse_attr(u32)`, `Deduper::should_count(&path)` signatures match across all call sites.

**Known limitation (documented, intentional):** hardlinked files **below** the identity threshold (default 1 MiB) may be double-counted. Impact on "why is C: full" is negligible since bloat is dominated by large components. Lowering the threshold trades accuracy for scan time.
