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
        // --- vendor GPU / driver caches ---
        CatalogEntry { pattern: "program files/nvidia corporation/installer2", category: "NVIDIA驱动缓存", purpose: "NVIDIA驱动安装源缓存；可安全删除", risk: Safe },
        CatalogEntry { pattern: "programdata/nvidia corporation/downloader", category: "NVIDIA下载缓存", purpose: "NVIDIA驱动下载临时文件；可安全删除", risk: Safe },
        CatalogEntry { pattern: "programdata/intel/package cache", category: "Intel驱动缓存", purpose: "Intel驱动安装源缓存；可安全删除", risk: Safe },
        CatalogEntry { pattern: "program files/realtek/audio", category: "Realtek驱动安装源", purpose: "Realtek声卡驱动缓存；驱动安装后可删除", risk: Caution },
        CatalogEntry { pattern: "node_modules", category: "Node.js modules", purpose: "Project dependencies; re-installable with npm install", risk: Caution },
        CatalogEntry { pattern: "__pycache__", category: "Python bytecode cache", purpose: "Compiled .pyc files; regenerated automatically", risk: Safe },
        // --- Windows temp / update (Safe) ---
        CatalogEntry { pattern: "appdata/local/temp", category: "User temp files", purpose: "Per-user temporary files; safe to clear", risk: Safe },
        CatalogEntry { pattern: "windows/temp", category: "Windows temp files", purpose: "System temporary files; safe to clear", risk: Safe },
        CatalogEntry { pattern: "windows/softwaredistribution/download", category: "Windows Update cache", purpose: "Downloaded update files; rebuilt by Windows Update", risk: Safe },
        CatalogEntry { pattern: "windows/installer/$patchcache$", category: "Windows Installer patch cache", purpose: "MSI patch cache; removing can break repair/uninstall", risk: Caution },
        CatalogEntry { pattern: "$recycle.bin", category: "Recycle Bin", purpose: "Already-deleted files awaiting purge; empty to reclaim", risk: Caution },
        // --- driver / OEM extraction directories ---
        CatalogEntry { pattern: "esupport/edriver", category: "驱动安装程序", purpose: "ASUS笔记本驱动安装文件；确认系统运行正常后可删除", risk: Caution },
        CatalogEntry { pattern: "esupport", category: "OEM支持文件", purpose: "ASUS笔记本出厂驱动/软件安装源；确认不需要后可清理", risk: Caution },
        // --- browser caches (Safe) ---
        CatalogEntry { pattern: "appdata/local/google/chrome/user data/default/cache", category: "Chrome cache", purpose: "Browser cache; rebuilt automatically", risk: Safe },
        CatalogEntry { pattern: "appdata/local/microsoft/edge/user data/default/cache", category: "Edge cache", purpose: "Browser cache; rebuilt automatically", risk: Safe },
        // --- user data (Caution) ---
        CatalogEntry { pattern: "users/*/downloads", category: "Downloads", purpose: "Downloaded files; review before deleting", risk: Caution },
        CatalogEntry { pattern: "users/*/videos", category: "Videos", purpose: "User videos; review before deleting", risk: Caution },
        CatalogEntry { pattern: "users/*/pictures", category: "Pictures", purpose: "User pictures; review before deleting", risk: Caution },
        CatalogEntry { pattern: "users/*/documents", category: "Documents", purpose: "User documents; review before deleting", risk: Caution },
        CatalogEntry { pattern: "users/*/desktop", category: "Desktop", purpose: "Desktop files; review before deleting", risk: Caution },
        CatalogEntry { pattern: "users/*", category: "User profile", purpose: "User home directory with personal files and settings; do NOT delete the entire profile", risk: System },
        // --- system (System: never delete) ---
        CatalogEntry { pattern: "windows/winsxs", category: "Windows component store", purpose: "Servicing store (hardlinked); manage via DISM only", risk: System },
        CatalogEntry { pattern: "windows/system32", category: "Windows system files", purpose: "Core OS files; do not delete", risk: System },
        CatalogEntry { pattern: "windows/syswow64", category: "Windows系统文件(WOW64)", purpose: "32位系统核心文件目录；与System32同等重要，绝不能删除", risk: System },
        CatalogEntry { pattern: "system volume information", category: "System Restore / VSS", purpose: "Restore points & shadow copies; manage via System Protection, do not delete", risk: System },
        CatalogEntry { pattern: "windows/installer", category: "Windows Installer cache", purpose: "MSI/MSP cache; needed for repair/uninstall — prune with care", risk: Caution },
        CatalogEntry { pattern: "pagefile.sys", category: "Virtual memory", purpose: "Paging file; resize via System settings, do not delete", risk: System },
        CatalogEntry { pattern: "hiberfil.sys", category: "Hibernation file", purpose: "Hibernation image; disable via powercfg, do not delete", risk: System },
        CatalogEntry { pattern: "swapfile.sys", category: "Swap file", purpose: "System swap file; managed by Windows", risk: System },
        CatalogEntry { pattern: "$mft", category: "Master File Table", purpose: "NTFS metadata; do not delete", risk: System },
        CatalogEntry { pattern: "program files/windowsapps", category: "Store apps", purpose: "Installed Store apps; uninstall via Settings, not by deleting", risk: System },
        // --- runtimes / shared frameworks (System: apps depend on these) ---
        CatalogEntry { pattern: "program files/dotnet", category: ".NET运行时/SDK", purpose: ".NET共享运行时与SDK；已安装应用依赖它运行，不要删除", risk: System },
        CatalogEntry { pattern: "program files (x86)/dotnet", category: ".NET运行时/SDK", purpose: ".NET共享运行时与SDK；已安装应用依赖它运行，不要删除", risk: System },
        CatalogEntry { pattern: "program files/common files", category: "共享安装组件", purpose: "各软件共享的安装组件；删除会破坏已安装软件", risk: System },
        CatalogEntry { pattern: "program files (x86)/common files", category: "共享安装组件", purpose: "各软件共享的安装组件；删除会破坏已安装软件", risk: System },
        CatalogEntry { pattern: "windows/microsoft.net", category: ".NET Framework", purpose: "Windows .NET Framework运行时；系统与应用依赖，不要删除", risk: System },
        // --- installed dev toolchains / IDEs / apps (NOT build artifacts; subdirs like bin/lib/plugins are part of the install) ---
        CatalogEntry { pattern: "program files/microsoft visual studio", category: "Visual Studio / 生成工具", purpose: "Visual Studio 及 MSVC C++ 链接库；C++/Rust 编译依赖，删除会破坏构建环境", risk: System },
        CatalogEntry { pattern: "program files (x86)/microsoft visual studio", category: "Visual Studio / 生成工具", purpose: "Visual Studio 及 MSVC C++ 链接库；C++/Rust 编译依赖，删除会破坏构建环境", risk: System },
        CatalogEntry { pattern: "xilinx", category: "Xilinx/Vivado 工具链", purpose: "Xilinx FPGA 开发工具(Vivado)安装；体积大但为已安装工具链，勿删子目录", risk: Caution },
        CatalogEntry { pattern: "texlive", category: "TeX Live 发行版", purpose: "LaTeX 排版系统完整安装；如不再使用可通过卸载程序整体移除，勿删子目录", risk: Caution },
        CatalogEntry { pattern: "program files/android/android studio", category: "Android Studio", purpose: "Android Studio IDE 安装目录；如不需要可整体卸载，勿删 plugins 等子目录", risk: Caution },
        CatalogEntry { pattern: "program files (x86)/android/android studio", category: "Android Studio", purpose: "Android Studio IDE 安装目录；如不需要可整体卸载，勿删 plugins 等子目录", risk: Caution },
        CatalogEntry { pattern: "program files/gnu octave", category: "GNU Octave", purpose: "GNU Octave 数值计算软件安装目录；删除 bin/lib 子目录会损坏程序", risk: Caution },
        CatalogEntry { pattern: "program files/blackmagic design", category: "DaVinci Resolve", purpose: "Blackmagic DaVinci Resolve 视频软件安装；勿删子目录，移除请用卸载程序", risk: Caution },
        // --- Windows maintenance paths ---
        CatalogEntry { pattern: "windows/winsxs/manifestcache", category: "WinSxS清单缓存", purpose: "Windows组件存储清单缓存；可安全清理", risk: Safe },
        CatalogEntry { pattern: "windows/prefetch", category: "预读取缓存", purpose: "Windows预读取文件；系统自动管理，可安全删除", risk: Safe },
        CatalogEntry { pattern: "windows/assembly/*", category: ".NET程序集缓存", purpose: ".NET全局程序集预编译缓存；系统管理，不建议手动删除", risk: Caution },
        CatalogEntry { pattern: "windows.old", category: "旧Windows安装", purpose: "系统升级残留；确认不需要回滚后可用磁盘清理删除", risk: Caution },
        CatalogEntry { pattern: "$windows.~bt", category: "Windows安装临时文件", purpose: "Windows安装/升级临时文件；安装完成后可安全删除", risk: Safe },
        CatalogEntry { pattern: "$windows.~ws", category: "Windows安装临时文件", purpose: "Windows安装/升级临时文件；安装完成后可安全删除", risk: Safe },
        CatalogEntry { pattern: "windows/system32/winevt/logs", category: "Windows事件日志", purpose: "系统事件日志文件；可安全清除，但会丢失诊断记录", risk: Caution },
        CatalogEntry { pattern: "windows/memory.dmp", category: "内存转储文件", purpose: "系统崩溃内存转储；如不需要调试可安全删除", risk: Safe },
        CatalogEntry { pattern: "windows/minidump", category: "小型崩溃转储", purpose: "小型内存转储文件；可安全删除", risk: Safe },
        CatalogEntry { pattern: "memory.dmp", category: "内存转储文件", purpose: "系统崩溃内存转储(根目录)；如不需要调试可安全删除", risk: Safe },
        CatalogEntry { pattern: "programdata/package cache", category: "WIX/MSI安装源", purpose: "MSI/WIX软件安装源缓存；安装后可安全删除", risk: Safe },
        CatalogEntry { pattern: "programdata/microsoft/visualstudio/packages", category: "VS安装缓存", purpose: "Visual Studio安装源缓存；可安全删除", risk: Safe },
        // --- application caches (Safe) ---
        CatalogEntry { pattern: "appdata/roaming/discord/cache", category: "Discord缓存", purpose: "Discord应用缓存；可安全删除", risk: Safe },
        CatalogEntry { pattern: "appdata/roaming/slack/cache", category: "Slack缓存", purpose: "Slack应用缓存；可安全删除", risk: Safe },
        CatalogEntry { pattern: "appdata/roaming/code/cache", category: "VS Code缓存", purpose: "VS Code编辑器缓存；可安全删除", risk: Safe },
        CatalogEntry { pattern: "appdata/roaming/zoom/data", category: "Zoom数据", purpose: "Zoom会议数据；可能含录制文件，确认后删除", risk: Caution },
        CatalogEntry { pattern: "appdata/roaming/sun/java/deployment/cache", category: "Java缓存", purpose: "Java部署缓存；可安全删除", risk: Safe },
        CatalogEntry { pattern: "appdata/roaming/vlc/art", category: "VLC缓存", purpose: "VLC播放器封面缓存；可安全删除", risk: Safe },
        CatalogEntry { pattern: "appdata/roaming/microsoft/windows/recent", category: "最近文件", purpose: "Windows最近文件快捷方式列表；可安全删除", risk: Safe },
        CatalogEntry { pattern: "appdata/local/microsoft/windows/explorer", category: "缩略图缓存", purpose: "文件资源管理器缩略图缓存；可安全删除", risk: Safe },
        CatalogEntry { pattern: "appdata/local/microsoft/media player", category: "媒体播放器缓存", purpose: "Windows Media Player缓存；可安全删除", risk: Safe },
        CatalogEntry { pattern: "appdata/roaming/tencent/logs", category: "腾讯日志", purpose: "腾讯软件日志文件；可安全删除", risk: Safe },
        CatalogEntry { pattern: "appdata/roaming/duowan/yy/log", category: "YY日志", purpose: "YY语音日志文件；可安全删除", risk: Safe },
        CatalogEntry { pattern: "appdata/roaming/baiduyunkernel/data", category: "百度网盘缓存", purpose: "百度网盘数据缓存；可安全删除", risk: Safe },
        // --- installed-software version artifacts ---
        CatalogEntry { pattern: "program files/tencent/qqnt/versions", category: "QQ旧版本", purpose: "QQNT旧版本安装目录；可清理旧版本保留最新", risk: Caution },
        CatalogEntry { pattern: "program files/tencent/wemeet", category: "腾讯会议安装", purpose: "腾讯会议安装目录；旧版本可清理但建议通过卸载程序操作", risk: Caution },
        // --- git repositories (Caution) ---
        CatalogEntry { pattern: ".git", category: "Git repository data", purpose: "Git version control database (objects, refs, history); removing loses commit history — re-clone from remote to restore", risk: Caution },
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

    #[test]
    fn matches_mft_and_new_entries() {
        assert_eq!(match_path(Path::new(r"\$MFT")).unwrap().category, "Master File Table");
        assert_eq!(match_path(Path::new(r"\$MFT")).unwrap().risk, Risk::System);
        assert_eq!(match_path(Path::new(r"\System Volume Information")).unwrap().risk, Risk::System);
        assert_eq!(match_path(Path::new(r"\Windows\Installer")).unwrap().risk, Risk::Caution);
        // SysWOW64 is core OS — both LLMs false-Safe it, so the catalog must catch it.
        assert_eq!(match_path(Path::new(r"\Windows\SysWOW64")).unwrap().risk, Risk::System);
    }

    #[test]
    fn runtime_dirs_are_system() {
        // .NET runtime + shared component dirs must never be classified Safe.
        assert_eq!(match_path(Path::new(r"\Program Files\dotnet")).unwrap().risk, Risk::System);
        assert_eq!(match_path(Path::new(r"\Program Files\Common Files")).unwrap().risk, Risk::System);
        assert_eq!(match_path(Path::new(r"\Windows\Microsoft.NET")).unwrap().risk, Risk::System);
    }

    #[test]
    fn installed_toolchains_not_safe() {
        // Installed toolchains/IDEs (and their bin/lib/plugins subdirs) must never be Safe.
        // The cut claims the whole install subtree at the root match.
        let cases = [
            r"\Program Files (x86)\Microsoft Visual Studio",
            r"\Program Files\Microsoft Visual Studio",
            r"\Xilinx",
            r"\texlive",
            r"\Program Files\Android\Android Studio",
            r"\Program Files\GNU Octave",
            r"\Program Files\Blackmagic Design",
        ];
        for c in cases {
            let e = match_path(Path::new(c)).unwrap_or_else(|| panic!("no catalog match for {c}"));
            assert_ne!(e.risk, Risk::Safe, "{c} must not be Safe (got {:?})", e.risk);
        }
    }

    #[test]
    fn matches_git_dir_anywhere() {
        let e = match_path(Path::new(r"\Users\dongm\github\myproject\.git")).unwrap();
        assert_eq!(e.category, "Git repository data");
        assert_eq!(e.risk, Risk::Caution);
    }

    #[test]
    fn matches_user_profile_root() {
        let e = match_path(Path::new(r"\Users\dongm")).unwrap();
        assert_eq!(e.category, "User profile");
        assert_eq!(e.risk, Risk::System);
    }
}
