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
    // Largest first; tie-break by FRN so output is deterministic across runs
    // (HashMap iteration order is otherwise random).
    v.sort_by_key(|(frn, a)| (std::cmp::Reverse(a.physical_size), *frn));
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
    v.sort_by_key(|(frn, r)| (std::cmp::Reverse(r.physical_size), *frn));
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
