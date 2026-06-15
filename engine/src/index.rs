use crate::model::RawRecord;
use std::collections::HashMap;

/// FRN-keyed lookup plus parent→children adjacency, built from a record sweep.
pub struct Index {
    pub by_frn: HashMap<u64, RawRecord>,
    pub children: HashMap<u64, Vec<u64>>,
}

pub fn build_index(records: Vec<RawRecord>) -> Index {
    let mut by_frn = HashMap::new();
    let mut children: HashMap<u64, Vec<u64>> = HashMap::new();
    for rec in records {
        children.entry(rec.parent_frn).or_default().push(rec.frn);
        by_frn.insert(rec.frn, rec);
    }
    Index { by_frn, children }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(frn: u64, parent: u64, name: &str) -> RawRecord {
        RawRecord { frn, parent_frn: parent, name: name.into(), is_dir: true,
            is_reparse: false, logical_size: 0, physical_size: 0, hard_link_count: 1, in_use: true }
    }
    fn file(frn: u64, parent: u64, name: &str, size: u64) -> RawRecord {
        RawRecord { frn, parent_frn: parent, name: name.into(), is_dir: false,
            is_reparse: false, logical_size: size, physical_size: size, hard_link_count: 1, in_use: true }
    }

    #[test]
    fn builds_lookup_and_children() {
        let idx = build_index(vec![
            dir(10, 5, "Users"),
            file(20, 10, "a.bin", 100),
            file(21, 10, "b.bin", 50),
        ]);
        assert_eq!(idx.by_frn.len(), 3);
        assert_eq!(idx.by_frn[&20].name, "a.bin");
        let mut kids = idx.children[&10].clone();
        kids.sort();
        assert_eq!(kids, vec![20, 21]);
    }
}
