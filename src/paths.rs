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
