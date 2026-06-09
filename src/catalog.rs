use crate::model::Risk;
use std::path::Path;

/// One catalog rule: a path-suffix pattern → what the directory is + its risk.
pub struct CatalogEntry {
    /// Lowercase, '/'-separated trailing components; `*` matches one component.
    pub pattern: &'static str,
    pub category: &'static str,
    pub purpose: &'static str,
    pub risk: Risk,
}

/// The curated catalog of well-known directories. Ordered most-specific first;
/// match_path returns the first match.
pub fn catalog() -> &'static [CatalogEntry] {
    use Risk::*;
    &[
        // --- developer caches (Safe) ---
        CatalogEntry { pattern: "appdata/local/npm-cache", category: "Node.js package cache", purpose: "npm download cache; rebuilt on next install", risk: Safe },
        CatalogEntry { pattern: "appdata/roaming/npm-cache", category: "Node.js package cache", purpose: "npm download cache; rebuilt on next install", risk: Safe },
        CatalogEntry { pattern: "appdata/local/yarn/cache", category: "Yarn package cache", purpose: "Yarn download cache; rebuilt on next install", risk: Safe },
        CatalogEntry { pattern: ".gradle/caches", category: "Gradle build cache", purpose: "Gradle dependency/build cache; rebuilt on next build", risk: Safe },
        CatalogEntry { pattern: ".m2/repository", category: "Maven repository", purpose: "Maven dependency cache; re-downloaded on next build", risk: Safe },
        CatalogEntry { pattern: ".cargo/registry", category: "Rust cargo registry", purpose: "Cargo crate cache; re-downloaded on next build", risk: Safe },
        CatalogEntry { pattern: "appdata/local/pip/cache", category: "pip cache", purpose: "Python pip download cache; rebuilt on next install", risk: Safe },
        CatalogEntry { pattern: ".cache/pip", category: "pip cache", purpose: "Python pip download cache; rebuilt on next install", risk: Safe },
        CatalogEntry { pattern: "appdata/local/nvidia/dxcache", category: "NVIDIA shader cache", purpose: "GPU shader cache; regenerated automatically", risk: Safe },
        CatalogEntry { pattern: "appdata/local/nvidia/glcache", category: "NVIDIA shader cache", purpose: "GPU shader cache; regenerated automatically", risk: Safe },
        CatalogEntry { pattern: "appdata/local/d3dscache", category: "DirectX shader cache", purpose: "GPU shader cache; regenerated automatically", risk: Safe },
        CatalogEntry { pattern: "node_modules", category: "Node.js modules", purpose: "Project dependencies; re-installable with npm install", risk: Caution },
        CatalogEntry { pattern: "__pycache__", category: "Python bytecode cache", purpose: "Compiled .pyc files; regenerated automatically", risk: Safe },
        // --- Windows temp / update (Safe) ---
        CatalogEntry { pattern: "appdata/local/temp", category: "User temp files", purpose: "Per-user temporary files; safe to clear", risk: Safe },
        CatalogEntry { pattern: "windows/temp", category: "Windows temp files", purpose: "System temporary files; safe to clear", risk: Safe },
        CatalogEntry { pattern: "windows/softwaredistribution/download", category: "Windows Update cache", purpose: "Downloaded update files; rebuilt by Windows Update", risk: Safe },
        CatalogEntry { pattern: "windows/installer/$patchcache$", category: "Windows Installer patch cache", purpose: "MSI patch cache; removing can break repair/uninstall", risk: Caution },
        CatalogEntry { pattern: "$recycle.bin", category: "Recycle Bin", purpose: "Already-deleted files awaiting purge; empty to reclaim", risk: Caution },
        // --- browser caches (Safe) ---
        CatalogEntry { pattern: "appdata/local/google/chrome/user data/default/cache", category: "Chrome cache", purpose: "Browser cache; rebuilt automatically", risk: Safe },
        CatalogEntry { pattern: "appdata/local/microsoft/edge/user data/default/cache", category: "Edge cache", purpose: "Browser cache; rebuilt automatically", risk: Safe },
        // --- user data (Caution) ---
        CatalogEntry { pattern: "users/*/downloads", category: "Downloads", purpose: "Downloaded files; review before deleting", risk: Caution },
        CatalogEntry { pattern: "users/*/videos", category: "Videos", purpose: "User videos; review before deleting", risk: Caution },
        CatalogEntry { pattern: "users/*/pictures", category: "Pictures", purpose: "User pictures; review before deleting", risk: Caution },
        CatalogEntry { pattern: "users/*/documents", category: "Documents", purpose: "User documents; review before deleting", risk: Caution },
        CatalogEntry { pattern: "users/*/desktop", category: "Desktop", purpose: "Desktop files; review before deleting", risk: Caution },
        // --- system (System: never delete) ---
        CatalogEntry { pattern: "windows/winsxs", category: "Windows component store", purpose: "Servicing store (hardlinked); manage via DISM only", risk: System },
        CatalogEntry { pattern: "windows/system32", category: "Windows system files", purpose: "Core OS files; do not delete", risk: System },
        CatalogEntry { pattern: "pagefile.sys", category: "Virtual memory", purpose: "Paging file; resize via System settings, do not delete", risk: System },
        CatalogEntry { pattern: "hiberfil.sys", category: "Hibernation file", purpose: "Hibernation image; disable via powercfg, do not delete", risk: System },
        CatalogEntry { pattern: "swapfile.sys", category: "Swap file", purpose: "System swap file; managed by Windows", risk: System },
        CatalogEntry { pattern: "$mft", category: "Master File Table", purpose: "NTFS metadata; do not delete", risk: System },
        CatalogEntry { pattern: "program files/windowsapps", category: "Store apps", purpose: "Installed Store apps; uninstall via Settings, not by deleting", risk: System },
    ]
}

/// Lowercase '/'-separated components of a path.
fn components(path: &Path) -> Vec<String> {
    path.to_string_lossy()
        .to_lowercase()
        .split(['\\', '/'])
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

/// True if `path_comps` ends with `pat_comps` (with `*` matching any one component).
fn suffix_matches(path_comps: &[String], pat_comps: &[&str]) -> bool {
    if pat_comps.len() > path_comps.len() {
        return false;
    }
    let start = path_comps.len() - pat_comps.len();
    pat_comps
        .iter()
        .enumerate()
        .all(|(i, pat)| *pat == "*" || *pat == path_comps[start + i])
}

/// Return the first catalog entry whose pattern matches the tail of `path`.
pub fn match_path(path: &Path) -> Option<&'static CatalogEntry> {
    let comps = components(path);
    catalog().iter().find(|entry| {
        let pat: Vec<&str> = entry.pattern.split('/').filter(|s| !s.is_empty()).collect();
        suffix_matches(&comps, &pat)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn matches_npm_cache_anywhere() {
        let e = match_path(Path::new(r"\Users\dongm\AppData\Local\npm-cache")).unwrap();
        assert_eq!(e.category, "Node.js package cache");
        assert_eq!(e.risk, Risk::Safe);
    }

    #[test]
    fn wildcard_matches_any_user() {
        let e = match_path(Path::new(r"\Users\someone\Downloads")).unwrap();
        assert_eq!(e.risk, Risk::Caution);
    }

    #[test]
    fn matches_system_pagefile() {
        let e = match_path(Path::new(r"\pagefile.sys")).unwrap();
        assert_eq!(e.risk, Risk::System);
    }

    #[test]
    fn no_match_for_ordinary_dir() {
        assert!(match_path(Path::new(r"\Users\dongm\projects\myapp")).is_none());
    }
}
