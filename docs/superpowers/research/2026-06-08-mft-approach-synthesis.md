# MFT-Direct Scanner — Approach Synthesis & Algorithm

> 2026-06-08 · Synthesis of three parallel reference evaluations (ColinFinck/ntfs, omerbenamram/mft, windirstat+NTFS-File-Search). Decision + the algorithm to port.

## Decision

Build the scanner as **OUR FSCTL volume layer (WinDirStat recipe) + the `mft` crate for record parsing + OUR path/dedup/aggregation**.

- **Reuse `mft`** (omerbenamram, v0.7.0, MIT/Apache-2.0, `forbid(unsafe_code)`): a flat MFT byte parser. `MftEntry::from_buffer(bytes, record_number)` applies fixups and exposes every field we need — `$FILE_NAME` (name, parent ref, namespace), `$DATA` logical + allocated size, `hard_link_count`, base reference, dir/in-use flags, reparse attribute bit. This is the genuinely hard part we don't want to rewrite.
- **Write our own volume layer** using the `windows` crate + the WinDirStat FSCTL recipe (below). Simpler/faster than either crate's I/O and handles `$MFT` fragmentation via the OS.
- **Do NOT** use ColinFinck/ntfs (no caching → re-parses `$MFT` runlist on every `Ntfs::file` call, O(n²) on a full sweep) or the `crates/ntfs` sibling (heavy deps: vendored openssl/aes/des/flate2, v0.1.0).

Why not just depend on a crate? See [reference analysis](2026-06-08-reference-projects-hardlink-analysis.md) — no crate reads a live `\\.\C:` volume; that FFI is the one thing we must write regardless, and once we have the MFT bytes, `mft` parses them cleanly.

## The seam

The FFI/volume code produces a plain vector; everything after is pure and unit-testable.

```rust
pub struct RawRecord {
    pub frn: u64,            // file record number — the hardlink dedup key
    pub parent_frn: u64,     // from the best (non-DOS) $FILE_NAME
    pub name: String,        // best name; DOS-only 8.3 names skipped
    pub is_dir: bool,
    pub is_reparse: bool,
    pub logical_size: u64,   // $DATA FileSize (unnamed stream)
    pub physical_size: u64,  // on-disk allocated (compressed/sparse aware)
    pub hard_link_count: u16,
    pub in_use: bool,
}
```

## Algorithm (port target, cited from windirstat `FinderNtfs.cpp` / `Item.Extended.cpp`)

### 1. Raw MFT access (admin required)
1. Volume path → `\\.\C:` (strip trailing `\`).
2. `CreateFileW(path, FILE_READ_DATA|FILE_READ_ATTRIBUTES|SYNCHRONIZE, FILE_SHARE_READ|WRITE|DELETE, OPEN_EXISTING, FILE_FLAG_NO_BUFFERING|FILE_FLAG_OVERLAPPED)`.
3. `DeviceIoControl(FSCTL_GET_NTFS_VOLUME_DATA)` → `NTFS_VOLUME_DATA_BUFFER`: `BytesPerCluster`, `BytesPerFileRecordSegment` (record size, usually 1024), `BytesPerSector`.
4. Open `\\.\C:\$MFT` (or use the volume's `$MFT::$DATA`), `DeviceIoControl(FSCTL_GET_RETRIEVAL_POINTERS, STARTING_VCN_INPUT_BUFFER{0})` → `RETRIEVAL_POINTERS_BUFFER`; loop doubling buffer while `ERROR_MORE_DATA`. This returns the **already-resolved** `$MFT` extents (handles fragmentation).
5. Extents → `(startLCN, clusterCount)`: `clusterCount = NextVcn[i] - (i==0 ? StartingVcn : NextVcn[i-1])`.
6. For each extent, read `clusterCount * BytesPerCluster` bytes at volume offset `startLCN * BytesPerCluster`, in sector-aligned 4 MiB chunks (`SetFilePointerEx` + `ReadFile`, or `seek_read`).

> `windows` crate modules to use (verify exact paths during the spike): `Win32::Storage::FileSystem` (CreateFileW, ReadFile, SetFilePointerEx), `Win32::System::Ioctl` (FSCTL_* constants + NTFS_VOLUME_DATA_BUFFER, RETRIEVAL_POINTERS_BUFFER, STARTING_VCN_INPUT_BUFFER), `Win32::System::IO::DeviceIoControl`.

### 2. Record parsing (delegate to `mft` crate)
- Walk the MFT buffer in `BytesPerFileRecordSegment` strides; for stride `n`, `MftEntry::from_buffer(slice, n)` (it validates `FILE` magic + applies fixups).
- Skip if not in-use (`!is_allocated()`).
- **Extension records:** if `base_reference.entry != 0`, the record's attributes belong to its base FRN — merge into the base (or, simplest first cut, skip standalone extension records and accept rare under-count on huge fragmented files; revisit).

### 3. Names & paths
- Iterate `$FILE_NAME` attrs; **skip `namespace == DOS (2)`** (the 8.3 alias) — use `find_best_name_attribute()`. Skip `.`/`..`.
- Record `parent_frn = file_name.parent.entry`. A hardlinked file has multiple names/parents → emit one `RawRecord` per (kept) name but all share the same `frn`. (For M1a first cut we keep the first/best name → first parent; multi-parent placement is a later refinement.)
- Reconstruct full path by walking `parent_frn` to root **FRN 5**, memoizing directory paths. Guard against cycles/orphans.

### 4. Size
- Use the **unnamed** `$DATA` only (skip ADS / named streams).
- `logical_size` = non-resident `file_size` else resident `data_size`.
- `physical_size` = `(compressed||sparse) ? compressed_size : allocated_length`; resident → `(len+7)&!7`.
- Directories: size 0; their total is the sum of children, accumulated up the tree.

### 5. Hardlink dedup (the whole point)
- **One base FRN = one physical file.** When aggregating physical size up the tree, keep `seen: HashSet<u64>`; add a record's `physical_size` only on the **first** encounter of its `frn`, contribute 0 after. Attribute the single count to the first path seen (WizTree-style). `hard_link_count > 1` is just a hint.

### 6. Edge cases
- Reserved metafiles FRN < 16 (root = 5): keep root as tree origin, flag the rest reserved.
- Reparse points: detect via `FILE_ATTRIBUTE_REPARSE_POINT` (0x400); **do not descend** (target FRN may be on another volume).
- Deleted/torn records: skip (not in-use; fixup mismatch).
- **Non-NTFS / no-admin fallback:** the whole MFT path needs admin + NTFS. Fall back to the jwalk scanner (the original M1a plan, now deferred) for FAT/exFAT/network or when not elevated.

## Build approach
- **Spike** the volume FFI + `mft` parse (`cargo build` to compile-check; **run elevated** on a real NTFS volume to verify — can't unit-test raw volume reads). Goal: produce `Vec<RawRecord>` and print a few.
- **TDD** everything downstream (index build, path reconstruction, hardlink dedup, aggregation, top-N, formatting) against synthetic `RawRecord` fixtures — no admin, no volume needed.

## Licenses
`mft`: MIT/Apache-2.0. `windows` crate: MIT/Apache-2.0. WinDirStat (algorithm reference only, not copied): GPL-2.0 — we reimplement the algorithm, we do not copy code.
