# Reference Projects Analysis — Hardlink Dedup on Windows

> 2026-06-08 · Goal: avoid reinventing the wheel. Which OSS already solves parallel scan + hardlink-correct sizing on Windows, and can we use/reference it?

Cloned into `references/` (gitignored, reference-only): `dua-cli`, `dust`, `parallel-disk-usage` (pdu), `diskonaut`.

## TL;DR

**No project provides clean, stable-Rust, performant hardlink dedup on Windows.** The ecosystem either punts (dust), requires nightly (dua), or supports it only on Unix (pdu). The single reusable asset is **dust's `winapi-util` limited-handle recipe** for reading a file ID on stable Rust cheaply. We adopt that recipe and keep our own size-gated dedup + classification layer.

## Findings per project

| Project | Stable on Windows | Windows hardlink dedup | How it gets file ID on Windows | License |
|---------|:---:|---|---|:---:|
| **dust** (bootandy) | ✅ | ❌ Plain files counted per-link by design (perf) | `winapi-util` + handle opened with `FILE_READ_ATTRIBUTES` only — for non-trivial files | Apache-2.0 |
| **dua** (Byron) | ❌ needs nightly | ✅ via std `file_index()`/`number_of_links()` | nightly std, gated by `#![feature(windows_by_handle)]` | MIT |
| **pdu** (KSXGitHub) | ✅ | ❌ Unix-only; disabled on Windows | N/A (`DeviceId(())` on non-Unix) | Apache-2.0 |
| **diskonaut** (imsnif) | ✅ | ❌ none | N/A | — |

### dust — `references/dust/src/platform.rs`
- Stable. Key comment: *"Hard links: Unresolved. We don't get inode/file index, so hard links count once for each link."* — for **plain** files it deliberately skips dedup to avoid opening a handle per file.
- It only opens a handle for *non-trivial* files (reparse points, OneDrive sparse, etc.) via `get_metadata_expensive`.
- **The reusable recipe:** `handle_from_path_limited` opens with `OpenOptions::new().access_mode(FILE_READ_ATTRIBUTES)` (0x0080), wraps in `winapi_util::Handle::from_file`, then `winapi_util::file::information(&h)` → `.file_index()`, `.volume_serial_number()`. Avoiding `FILE_READ_DATA` is the whole trick: it skips the cost and the Defender scan. Measured 30 min → 8 sec on a large volume.

### dua — `references/dua-cli/src/inodefilter.rs`, `src/lib.rs:4`
- `lib.rs` line 4: `#![cfg_attr(windows, feature(windows_by_handle))]` → **nightly-only on Windows**. Also `#![forbid(unsafe_code)]`, so it won't use winapi.
- `InodeFilter` is an elegant nlink-countdown map (frees entries once all links seen). Nice algorithm, but depends on nightly std on Windows. Tree is a coupled `petgraph::StableGraph`.
- **Conclusion:** can't depend on `dua` as a library without forcing our whole project onto nightly.

### pdu — `references/parallel-disk-usage/src/hardlink/aware.rs`, `src/fs_tree_builder/device_id.rs`
- `aware.rs:17` `use std::os::unix::fs::MetadataExt;`; `InodeNumber::get`/`DeviceNumber::get` are `#[cfg(unix)]`. `device_id.rs` returns `DeviceId(())` on non-Unix → cross-device + hardlink detection effectively off on Windows.
- Compiles on stable 1.95.0 *because* the hardlink path is Unix-gated.
- Has the cleanest **architecture** to learn from (`DataTree`, `par_deduplicate_hardlinks`) even though the Windows dedup isn't there.

### diskonaut
- No hardlink/inode code in `src/` at all.

## Decision

1. **Reuse `jwalk`** for the parallel walk (already our choice; it's Byron's crate, same as dua uses).
2. **Adopt dust's `winapi-util` limited-handle recipe** to read `(volume_serial_number, file_index)` on stable Rust. Attribute dust (Apache-2.0) in the source comment.
3. **Keep our own size-gated dedup**: only open a handle for files ≥ threshold (default 1 MiB). This is *more* complete than dust for our use case (dust skips plain-file dedup entirely; we dedup the big files that actually drive WinSxS bloat) while staying fast (no handle opens for the millions of tiny files).
4. **Do not depend on dua** (nightly) or **pdu** (no Windows dedup) as libraries. Learn from pdu's `DataTree` design for later milestones.

## License notes
- `winapi-util` (BurntSushi): Unlicense/MIT — permissive.
- `dust`: Apache-2.0 — copying the small recipe requires attribution (added as a source comment).
- If we later vendor any larger chunk from pdu/dust, preserve their license headers.
