//! Directory content analysis: for known-category directories, inspect
//! actual children and append observations to the purpose field.

use crate::scan::index::Index;
use crate::model::Item;
use std::collections::HashMap;

/// Enrich `purpose` for every directory item with observed content patterns.
pub fn analyze_directory_contents(items: &mut [Item], index: &Index) {
    for it in items.iter_mut() {
        if !it.is_dir {
            continue;
        }
        let content_note = summarize_children(it.frn, index);
        if !content_note.is_empty() {
            it.purpose = format!("{}. {}", it.purpose, content_note);
        }
    }
}

/// Produce a one-line summary of what's inside a directory.
pub fn summarize_children(frn: u64, index: &Index) -> String {
    let children = match index.children.get(&frn) {
        Some(c) => c,
        None => return String::new(),
    };

    let mut dirs: Vec<String> = Vec::new();
    let mut dir_frns: Vec<u64> = Vec::new(); // parallel to dirs, for O(1) child count lookup
    let mut exts: HashMap<String, (u64, usize)> = HashMap::new(); // total_size, count

    for &child_frn in children {
        let rec = match index.by_frn.get(&child_frn) {
            Some(r) => r,
            None => continue,
        };
        if rec.is_dir {
            dirs.push(rec.name.clone());
            dir_frns.push(child_frn);
        } else {
            let ext = file_ext_category(&rec.name);
            let e = exts.entry(ext).or_default();
            e.0 += rec.physical_size;
            e.1 += 1;
        }
    }

    // Detect if this is a git repository working tree (has .git subdirectory).
    let is_git_repo = dirs.iter().any(|d| d == ".git");

    let mut parts: Vec<String> = Vec::new();

    // Git repo badge.
    if is_git_repo {
        parts.push("Git repository".to_string());
    }

    // File count & extension summary.
    let total_files: usize = exts.values().map(|v| v.1).sum();
    if total_files > 0 {
        parts.push(format!("{} files", total_files));
    }

    // Top-4 extensions by total size.
    if !exts.is_empty() {
        let mut sorted: Vec<(String, (u64, usize))> = exts.into_iter().collect();
        sorted.sort_by_key(|(_, (s, _))| std::cmp::Reverse(*s));
        let ext_parts: Vec<String> = sorted
            .iter()
            .take(4)
            .map(|(ext, (_, cnt))| format!(".{ext}(×{cnt})"))
            .collect();
        parts.push(ext_parts.join(", "));
    }

    // Subdirectory count + largest (uses pre-collected FRNs for O(1) lookups).
    let n_dirs = dirs.len();
    if n_dirs > 0 {
        dirs.sort();
        // Find largest subdir by direct child count (O(n) via index.children lookup).
        let mut dir_sizes: Vec<(String, usize)> = dir_frns
            .iter()
            .zip(dirs.iter())
            .map(|(&cfrn, name)| (name.clone(), index.children.get(&cfrn).map(|c| c.len()).unwrap_or(0)))
            .collect();
        dir_sizes.sort_by_key(|(_, c)| std::cmp::Reverse(*c));
        if n_dirs <= 5 {
            let names: Vec<&str> = dirs.iter().map(|s| s.as_str()).collect();
            parts.push(format!("{} subdirs: {}", n_dirs, names.join(", ")));
        } else {
            parts.push(format!("{} subdirs", n_dirs));
            if let Some((name, cnt)) = dir_sizes.first() {
                if *cnt > 5 {
                    parts.push(format!("largest: {name}/({cnt} items)"));
                }
            }
        }
    }

    parts.join("; ")
}

/// Map a filename to a short extension category for grouping.
fn file_ext_category(name: &str) -> String {
    let ext = std::path::Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "" => "(noext)".to_string(),
        _ => ext,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::index::build_index;
    use crate::model::RawRecord;

    fn dir(frn: u64, parent: u64, name: &str) -> RawRecord {
        RawRecord {
            frn,
            parent_frn: parent,
            name: name.into(),
            is_dir: true,
            is_reparse: false,
            logical_size: 0,
            physical_size: 0,
            hard_link_count: 1,
            in_use: true,
        }
    }
    fn file(frn: u64, parent: u64, name: &str, size: u64) -> RawRecord {
        RawRecord {
            frn,
            parent_frn: parent,
            name: name.into(),
            is_dir: false,
            is_reparse: false,
            logical_size: size,
            physical_size: size,
            hard_link_count: 1,
            in_use: true,
        }
    }

    #[test]
    fn summarize_mixed_dir() {
        // root(5) > Downloads(10) > { old/(11)=dir, a.zip(20)=5000, b.exe(21)=3000, c.mp4(22)=2000 }
        let records = vec![
            dir(10, 5, "Downloads"),
            dir(11, 10, "old_backup"),
            file(20, 10, "a.zip", 5000),
            file(21, 10, "b.exe", 3000),
            file(22, 10, "c.mp4", 2000),
        ];
        let index = build_index(records);
        let summary = summarize_children(10, &index);
        assert!(summary.contains("3 files"));
        assert!(summary.contains(".zip"));
        assert!(summary.contains(".exe"));
        assert!(summary.contains("old_backup"));
    }

    #[test]
    fn empty_dir_gives_empty() {
        let index = build_index(vec![dir(10, 5, "empty")]);
        assert_eq!(summarize_children(10, &index), "");
    }

    #[test]
    fn noext_files_bucketed() {
        let index = build_index(vec![
            dir(10, 5, "stuff"),
            file(20, 10, "README", 100),
            file(21, 10, "Makefile", 200),
        ]);
        let summary = summarize_children(10, &index);
        assert!(summary.contains("(noext)"));
        assert!(summary.contains("2 files"));
    }

    #[test]
    fn detects_git_repository_working_tree() {
        // root(5) > myproject(10) > { .git/(11)=dir, src/(12)=dir, README.md(20)=5000 }
        let records = vec![
            dir(10, 5, "myproject"),
            dir(11, 10, ".git"),
            dir(12, 10, "src"),
            file(20, 10, "README.md", 5000),
        ];
        let index = build_index(records);
        let summary = summarize_children(10, &index);
        assert!(summary.contains("Git repository"));
        assert!(summary.contains(".md"));
    }
}
