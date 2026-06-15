use crate::index::Index;
use crate::model::ROOT_FRN;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// Full path for `frn`, walking parent links to ROOT_FRN, memoized in `cache`.
///
/// Iterative and cycle-guarded: a deep parent chain cannot overflow the stack,
/// and a parent cycle (A->B->A, which a real/corrupt MFT can contain) terminates
/// with a `<cycle:N>` marker instead of recursing forever.
pub fn path_for(frn: u64, index: &Index, cache: &mut HashMap<u64, PathBuf>) -> PathBuf {
    if let Some(p) = cache.get(&frn) {
        return p.clone();
    }
    // Walk upward collecting the chain until we hit a cached path, the root, an
    // orphan, a self-reference, or a cycle.
    let mut chain: Vec<u64> = Vec::new();
    let mut seen: HashSet<u64> = HashSet::new();
    let mut cur = frn;
    let base: PathBuf = loop {
        if let Some(p) = cache.get(&cur) {
            break p.clone();
        }
        if cur == ROOT_FRN {
            break PathBuf::from("\\");
        }
        match index.by_frn.get(&cur) {
            None => break PathBuf::from(format!("<orphan:{cur}>")),
            Some(rec) => {
                if !seen.insert(cur) {
                    break PathBuf::from(format!("<cycle:{cur}>"));
                }
                chain.push(cur);
                if rec.parent_frn == cur {
                    break PathBuf::from("\\"); // self-parent: treat as a top-level node
                }
                cur = rec.parent_frn;
            }
        }
    };
    // Build downward from the base, memoizing every directory on the way.
    let mut path = base;
    for &node in chain.iter().rev() {
        if let Some(rec) = index.by_frn.get(&node) {
            path.push(&rec.name);
        }
        cache.insert(node, path.clone());
    }
    cache.get(&frn).cloned().unwrap_or(path)
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

    #[test]
    fn cycle_terminates() {
        // A(10).parent=B(11) and B(11).parent=A(10): a 2-cycle must not recurse forever.
        let idx = crate::index::build_index(vec![
            rec(10, 11, "A", true),
            rec(11, 10, "B", true),
        ]);
        let mut cache = HashMap::new();
        let p = path_for(10, &idx, &mut cache);
        assert!(p.to_string_lossy().contains("cycle"));
    }
}
