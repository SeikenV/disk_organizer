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
