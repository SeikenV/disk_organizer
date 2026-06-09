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

/// Parse a contiguous MFT image into `RawRecord`s (one per in-use FILE record).
///
/// Records that are not allocated, are extension records (`base_reference.entry
/// != 0`), or lack a usable `$FILE_NAME` are skipped — a documented first-cut
/// simplification (rare under-count on heavily fragmented files).
pub fn parse_records(mft_bytes: Vec<u8>) -> Vec<RawRecord> {
    let mut parser = match MftParser::from_buffer(mft_bytes) {
        Ok(p) => p,
        Err(_) => return Vec::new(),
    };

    let mut out = Vec::new();
    for entry in parser.iter_entries() {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue, // torn / corrupt record
        };

        // In-use records only.
        if !entry.is_allocated() {
            continue;
        }
        // Skip extension records: their attributes belong to a base FRN.
        if entry.header.base_reference.entry != 0 {
            continue;
        }

        let frn = entry.header.record_number;

        // Best (non-DOS) name + its parent. find_best_name_attribute already
        // excludes the DOS 8.3 alias.
        let name_attr = match entry.find_best_name_attribute() {
            Some(n) => n,
            None => continue,
        };
        let parent_frn = name_attr.parent.entry;
        let name = name_attr.name; // already a decoded String in mft 0.7

        let is_dir = entry.is_dir();

        // Reparse: the $FILE_NAME flags carry FILE_ATTRIBUTE_REPARSE_POINT (0x400).
        let is_reparse = name_attr
            .flags
            .contains(FileAttributeFlags::FILE_ATTRIBUTE_REPARSE_POINT);

        // Size: scan $DATA attributes, use the unnamed (default) stream only.
        let (logical_size, physical_size) = unnamed_data_size(&entry);

        out.push(RawRecord {
            frn,
            parent_frn,
            name,
            is_dir,
            is_reparse,
            logical_size,
            physical_size,
            hard_link_count: entry.header.hard_link_count,
            in_use: true,
        });
    }

    out
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
