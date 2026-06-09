use crate::index::Index;
use crate::model::{DirAgg, ROOT_FRN};
use std::collections::{HashMap, HashSet};

/// Roll each file's size up its ancestor directories. A given physical file
/// (FRN) contributes its physical_size only once across the whole tree
/// (hardlink dedup); logical_size is added along its parent chain.
pub fn aggregate(index: &Index) -> HashMap<u64, DirAgg> {
    let mut totals: HashMap<u64, DirAgg> = HashMap::new();
    let mut physical_seen: HashSet<u64> = HashSet::new();

    for (&frn, rec) in &index.by_frn {
        if rec.is_dir {
            continue;
        }
        let phys = if physical_seen.insert(frn) { rec.physical_size } else { 0 };

        let mut cur = rec.parent_frn;
        let mut guard = 0;
        loop {
            let agg = totals.entry(cur).or_default();
            agg.logical_size += rec.logical_size;
            agg.physical_size += phys;
            agg.file_count += 1;
            if cur == ROOT_FRN {
                break;
            }
            match index.by_frn.get(&cur) {
                Some(parent) if parent.parent_frn != cur => cur = parent.parent_frn,
                _ => break,
            }
            guard += 1;
            if guard > 4096 {
                break;
            }
        }
    }
    totals
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::RawRecord;

    fn dir(frn: u64, parent: u64) -> RawRecord {
        RawRecord { frn, parent_frn: parent, name: format!("d{frn}"), is_dir: true,
            is_reparse: false, logical_size: 0, physical_size: 0, hard_link_count: 1, in_use: true }
    }
    fn file(frn: u64, parent: u64, size: u64) -> RawRecord {
        RawRecord { frn, parent_frn: parent, name: format!("f{frn}"), is_dir: false,
            is_reparse: false, logical_size: size, physical_size: size, hard_link_count: 1, in_use: true }
    }

    #[test]
    fn rolls_sizes_up_the_tree() {
        // root(5) > Users(10) > {a(20)=100, b(21)=50}
        let idx = crate::index::build_index(vec![
            dir(10, ROOT_FRN), file(20, 10, 100), file(21, 10, 50),
        ]);
        let totals = aggregate(&idx);
        assert_eq!(totals[&10].physical_size, 150);
        assert_eq!(totals[&10].file_count, 2);
        assert_eq!(totals[&ROOT_FRN].physical_size, 150); // bubbles to root
    }
}
