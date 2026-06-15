//! Parse a contiguous in-memory MFT image into `RawRecord`s using the `mft`
//! crate (omerbenamram/mft). `MftParser::from_buffer` treats the buffer as a
//! flat MFT and numbers records by buffer index; since we read `$MFT` in VCN
//! order starting at 0, that index equals the FRN.
//!
//! See synthesis §Algorithm steps 2-4 for the field mapping.

use crate::model::RawRecord;

use mft::MftParser;
use mft::attribute::header::ResidentialHeader;
use mft::attribute::{FileAttributeFlags, MftAttributeType};

/// Parse a contiguous MFT image into `RawRecord`s (one per in-use base FILE
/// record).
///
/// A large/fragmented file's `$DATA` runlist can overflow into **extension
/// records** (linked via `$ATTRIBUTE_LIST`), and the attribute piece carrying
/// the real sizes (`vnc_first == 0`) may live in an extension record rather than
/// the base record. So we accumulate unnamed-`$DATA` size from EVERY record,
/// keyed by the base FRN, then attach the total to the base record. (Previously
/// extension records were skipped, which lost ~290 GB on a ~720 GB volume.)
pub fn parse_records(mft_bytes: Vec<u8>) -> Vec<RawRecord> {
    let mut parser = match MftParser::from_buffer(mft_bytes) {
        Ok(p) => p,
        Err(_) => return Vec::new(),
    };

    // base FRN -> (logical, physical) accumulated from unnamed $DATA everywhere.
    let mut size_by_base: std::collections::HashMap<u64, (u64, u64)> =
        std::collections::HashMap::new();

    // Per-base-record metadata; the RawRecord is built after the sweep so it can
    // pick up size contributed by its extension records.
    struct Meta {
        frn: u64,
        parent_frn: u64,
        name: String,
        is_dir: bool,
        is_reparse: bool,
        hard_link_count: u16,
    }
    let mut metas: Vec<Meta> = Vec::new();

    for entry in parser.iter_entries().filter_map(Result::ok) {
        // In-use records only.
        if !entry.is_allocated() {
            continue;
        }

        let is_extension = entry.header.base_reference.entry != 0;
        let base_frn = if is_extension {
            entry.header.base_reference.entry
        } else {
            entry.header.record_number
        };

        // Accumulate this record's unnamed-$DATA size onto the base FRN. The
        // size-bearing piece (vnc_first == 0) appears in exactly one record per
        // file, so summing across records never double-counts.
        let (logical, physical) = unnamed_data_size(&entry);
        if logical != 0 || physical != 0 {
            let slot = size_by_base.entry(base_frn).or_insert((0, 0));
            slot.0 += logical;
            slot.1 += physical;
        }

        if is_extension {
            continue; // name/parent/flags come from the base record
        }

        // Best name + its parent. find_best_name_attribute prefers a Win32 /
        // Win32AndDos name and only falls back to a DOS-only/POSIX name (rare).
        let name_attr = match entry.find_best_name_attribute() {
            Some(n) => n,
            None => continue,
        };
        metas.push(Meta {
            frn: entry.header.record_number,
            parent_frn: name_attr.parent.entry,
            name: name_attr.name, // already a decoded String in mft 0.7
            is_dir: entry.is_dir(),
            // Reparse: $FILE_NAME flags carry FILE_ATTRIBUTE_REPARSE_POINT (0x400).
            is_reparse: name_attr
                .flags
                .contains(FileAttributeFlags::FILE_ATTRIBUTE_REPARSE_POINT),
            hard_link_count: entry.header.hard_link_count,
        });
    }

    metas
        .into_iter()
        .map(|m| {
            let (logical_size, physical_size) =
                size_by_base.get(&m.frn).copied().unwrap_or((0, 0));
            RawRecord {
                frn: m.frn,
                parent_frn: m.parent_frn,
                name: m.name,
                is_dir: m.is_dir,
                is_reparse: m.is_reparse,
                logical_size,
                physical_size,
                hard_link_count: m.hard_link_count,
                in_use: true,
            }
        })
        .collect()
}

/// Compute `(logical_size, physical_size)` from the unnamed `$DATA` attribute,
/// following synthesis step 4 and the WinDirStat recipe:
///
/// - Named streams (ADS) are ignored.
/// - Non-resident: `logical = file_size`; `physical = total_allocated` when
///   compressed/sparse, else `allocated_length`. Only the first run
///   (`vnc_first == 0`) carries the file size.
/// - Resident: `logical = data_size`; `physical = (data_size + 7) & !7`.
fn unnamed_data_size(entry: &mft::entry::MftEntry) -> (u64, u64) {
    let mut logical = 0u64;
    let mut physical = 0u64;

    for attr in entry.iter_attributes().filter_map(Result::ok) {
        if attr.header.type_code != MftAttributeType::DATA {
            continue;
        }
        // Unnamed (default) stream only — skip alternate data streams.
        if attr.header.name_size != 0 {
            continue;
        }

        match &attr.header.residential_header {
            ResidentialHeader::NonResident(nr) => {
                // Only the base run (lowest VCN) carries the real sizes.
                if nr.vnc_first != 0 {
                    continue;
                }
                logical = nr.file_size;
                let compressed_or_sparse = attr.header.data_flags.contains(
                    mft::attribute::AttributeDataFlags::IS_COMPRESSED,
                ) || attr
                    .header
                    .data_flags
                    .contains(mft::attribute::AttributeDataFlags::SPARSE);
                let phys = if compressed_or_sparse {
                    nr.total_allocated.unwrap_or(nr.allocated_length)
                } else {
                    nr.allocated_length
                };
                if phys > 0 {
                    physical = phys;
                }
            }
            ResidentialHeader::Resident(r) => {
                logical = r.data_size as u64;
                physical = ((r.data_size as u64) + 7) & !7;
            }
        }
    }

    (logical, physical)
}

/// Diagnostic v2: pinpoint where on-disk size lives. Crucially this sums the
/// unnamed `$DATA` allocation across **all** records — including extension
/// records (base_reference != 0) that our scanner currently skips — to test
/// whether `$ATTRIBUTE_LIST` / fragmented files hold the missing space. Also
/// characterizes the "allocated_length==0 but file_size>0" group.
pub fn size_audit(mft_bytes: Vec<u8>) {
    use mft::attribute::AttributeDataFlags;

    let mut parser = match MftParser::from_buffer(mft_bytes) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("size_audit: parse error: {e}");
            return;
        }
    };
    let gb = |b: u64| b as f64 / 1024.0 / 1024.0 / 1024.0;

    // unnamed $DATA allocation (vnc_first==0), counted everywhere vs base-only vs ext-only
    let (mut base_unnamed, mut ext_unnamed) = (0u64, 0u64);
    let mut fn_phys = 0u64;
    let (mut n_files, mut attrlist_files) = (0u64, 0u64);
    // "allocated_length==0 but file_size>0" group (the 725K mystery)
    let (mut z_count, mut z_logical, mut z_compressed, mut z_sparse, mut z_attrlist, mut z_totalloc) =
        (0u64, 0u64, 0u64, 0u64, 0u64, 0u64);

    for entry in parser.iter_entries().filter_map(Result::ok) {
        if !entry.is_allocated() {
            continue;
        }
        let is_ext = entry.header.base_reference.entry != 0;
        let is_dir = entry.is_dir();

        let mut rec_unnamed = 0u64; // unnamed $DATA alloc in THIS record
        let mut has_attrlist = false;
        let mut z_local: Option<(u64, bool, bool, u64)> = None; // (logical, compressed, sparse, totalloc)

        for attr in entry.iter_attributes().filter_map(Result::ok) {
            if attr.header.type_code == MftAttributeType::AttributeList {
                has_attrlist = true;
                continue;
            }
            if attr.header.type_code != MftAttributeType::DATA || attr.header.name_size != 0 {
                continue; // unnamed $DATA only
            }
            match &attr.header.residential_header {
                ResidentialHeader::NonResident(nr) => {
                    if nr.vnc_first != 0 {
                        continue;
                    }
                    let compressed = attr.header.data_flags.contains(AttributeDataFlags::IS_COMPRESSED);
                    let sparse = attr.header.data_flags.contains(AttributeDataFlags::SPARSE);
                    let phys = if compressed || sparse {
                        nr.total_allocated.unwrap_or(nr.allocated_length)
                    } else {
                        nr.allocated_length
                    };
                    rec_unnamed += phys;
                    if nr.file_size > 0 && nr.allocated_length == 0 {
                        z_local = Some((nr.file_size, compressed, sparse, nr.total_allocated.unwrap_or(0)));
                    }
                }
                ResidentialHeader::Resident(r) => {
                    rec_unnamed += ((r.data_size as u64) + 7) & !7;
                }
            }
        }

        if is_ext {
            ext_unnamed += rec_unnamed;
        } else {
            base_unnamed += rec_unnamed;
        }

        if !is_ext && !is_dir {
            n_files += 1;
            if let Some(nm) = entry.find_best_name_attribute() {
                fn_phys += nm.physical_size;
            }
            if has_attrlist {
                attrlist_files += 1;
            }
            if let Some((logical, compressed, sparse, totalloc)) = z_local {
                z_count += 1;
                z_logical += logical;
                if compressed { z_compressed += 1; }
                if sparse { z_sparse += 1; }
                if has_attrlist { z_attrlist += 1; }
                z_totalloc += totalloc;
            }
        }
    }

    eprintln!("\n===== MFT SIZE AUDIT v2 =====");
    eprintln!("files (base, non-dir)                  : {n_files}");
    eprintln!("unnamed $DATA alloc, BASE records      : {:.1} GB  (= current scan)", gb(base_unnamed));
    eprintln!("unnamed $DATA alloc, EXTENSION records : {:.1} GB  (currently SKIPPED)", gb(ext_unnamed));
    eprintln!("unnamed $DATA alloc, ALL records       : {:.1} GB  (<- compare to OS used)", gb(base_unnamed + ext_unnamed));
    eprintln!("Sum $FILE_NAME allocated_size          : {:.1} GB", gb(fn_phys));
    eprintln!("files with $ATTRIBUTE_LIST             : {attrlist_files}");
    eprintln!("--- 'allocated_length==0 & file_size>0' group ---");
    eprintln!("  count                                : {z_count}");
    eprintln!("  sum file_size (logical)              : {:.1} GB", gb(z_logical));
    eprintln!("  sum total_allocated (compressed)     : {:.1} GB", gb(z_totalloc));
    eprintln!("  with IS_COMPRESSED flag              : {z_compressed}");
    eprintln!("  with SPARSE flag                     : {z_sparse}");
    eprintln!("  with $ATTRIBUTE_LIST                 : {z_attrlist}");
    eprintln!("=============================");
}
