use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// MFT record number of the volume root directory.
/// Re-exported from `crate::consts` — the canonical definition lives there.
pub use crate::consts::ROOT_FRN;

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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Risk {
    Safe,    // cache/temp/regenerable — deleting loses nothing important
    Caution, // possibly wanted (downloads, media, user data)
    System,  // OS/app critical — never auto-deletable
    Unknown, // not covered by rules
}

/// Where an item's classification came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Source {
    Rule,      // matched the catalog
    Heuristic, // file-extension guess
    LLM,       // inferred by LLM enrichment
    Unknown,   // unclassified residual
}

/// A unit shown to the user and selectable for deletion. Items never overlap:
/// every counted byte belongs to exactly one Item.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub frn: u64,
    pub path: PathBuf,       // absolute, e.g. C:\Users\me\AppData\Local\npm-cache
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
