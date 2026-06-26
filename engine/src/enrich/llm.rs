// LLM prompts + request building for the llama-server backend.
//
// The actual HTTP transport lives in `client.rs` (OpenAI-compatible
// `/v1/chat/completions`).  This module owns the domain types, the JSON
// schemas, the system prompts/few-shots, and the parsing of model output.
//
// Structured output is constrained via `response_format: json_schema` and the
// reasoning phase is disabled via `chat_template_kwargs.enable_thinking=false`
// (both set by `client::build_json_body`).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---- Our domain types ----

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirSummary {
    pub category: String,
    pub purpose: String,
    /// LLM-assessed risk: "safe", "caution", "system", or "unknown".
    pub risk: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FinalReport {
    pub overview: String,
    pub safe_summary: String,
    pub caution_advice: String,
    pub cleanup_plan: Vec<String>,
}

// ---- JSON Schemas for structured output ----
//
// We derive schemars::JsonSchema on structs whose field names + types match the
// JSON shape we want the model to produce.  `dir_schema()`/`report_schema()`
// serialize these to a `serde_json::Value` passed as the request's
// `response_format.json_schema.schema` (see client.rs).
//
// Note: the derive places `"additionalProperties": false` by default, which
// llama-server's grammar-constrained sampling handles fine.
// The `allow(dead_code)` silences warnings — fields are only read by the derive macro.

#[allow(dead_code)]
#[derive(JsonSchema)]
struct DirSummarySchema {
    /// 2-6 word classification
    category: String,
    /// 1 sentence, include safe-to-delete assessment
    purpose: String,
    /// Risk level: "safe" (can delete), "caution" (review first), "system" (never delete), "unknown" (uncertain)
    risk: String,
}

#[allow(dead_code)]
#[derive(JsonSchema)]
struct ReportSchema {
    overview: String,
    safe_summary: String,
    caution_advice: String,
    cleanup_plan: Vec<String>,
}

// ---- JSON schema helpers ----

/// JSON Schema for a directory/file classification (`{category, purpose, risk}`).
fn dir_schema() -> Value {
    serde_json::to_value(schemars::schema_for!(DirSummarySchema))
        .expect("DirSummarySchema serializes")
}

/// JSON Schema for the holistic final report.
fn report_schema() -> Value {
    serde_json::to_value(schemars::schema_for!(ReportSchema))
        .expect("ReportSchema serializes")
}

// ---- Public API ----

/// Ask the LLM to summarize a directory.
pub fn summarize_dir(
    endpoint: &str,
    _model: &str,
    dir_path: &str,
    sample_entries: &[String],
    ancestor_context: Option<&str>,
    size_mb: u64,
    content_summary: &str,
) -> Result<DirSummary, String> {
    let samples_str = if sample_entries.is_empty() {
        "(empty)".to_string()
    } else {
        sample_entries.join(", ")
    };
    let ancestor_line = ancestor_context
        .map(|c| format!("\nAncestor context: {c}"))
        .unwrap_or_default();
    let size_line = format!("\nSize: {size_mb} MB");
    let content_line = if content_summary.is_empty() {
        String::new()
    } else {
        format!("\nContent summary: {content_summary}")
    };
    let user = format!(
        "Directory: {dir_path}{ancestor_line}{size_line}{content_line}\nSample contents:\n{samples_str}"
    );
    let system = system_prompt_dir();

    let body = crate::enrich::client::build_json_body(&system, &user, dir_schema(), 300);
    let raw = crate::enrich::client::chat(endpoint, &body)?;
    parse_summary(&raw)
}

/// Ask the LLM to analyze a large file.
pub fn summarize_file(
    endpoint: &str,
    _model: &str,
    file_path: &str,
    parent_dir: &str,
    sibling_files: &[String],
    ext: &str,
    ancestor_context: Option<&str>,
    size_mb: u64,
) -> Result<DirSummary, String> {
    let sibs_str = if sibling_files.is_empty() {
        "(none)".to_string()
    } else {
        sibling_files.join(", ")
    };
    let ancestor_line = ancestor_context
        .map(|c| format!("\nAncestor context: {c}"))
        .unwrap_or_default();
    let user = format!(
        "File: {file_path}\nSize: {size_mb} MB\nExtension: .{ext}\nParent directory: {parent_dir}{ancestor_line}\nSibling files: {sibs_str}"
    );
    let system = system_prompt_file();

    let body = crate::enrich::client::build_json_body(&system, &user, dir_schema(), 300);
    let raw = crate::enrich::client::chat(endpoint, &body)?;
    parse_summary(&raw)
}

// ---- Second-round translation (language option) ----

#[allow(dead_code)]
#[derive(JsonSchema)]
struct TranslationBatchSchema {
    items: Vec<TranslationItemSchema>,
}
#[allow(dead_code)]
#[derive(JsonSchema)]
struct TranslationItemSchema {
    category: String,
    purpose: String,
}

#[derive(Deserialize)]
struct TranslationBatchOut {
    items: Vec<TranslationItemOut>,
}
#[derive(Deserialize)]
struct TranslationItemOut {
    category: String,
    purpose: String,
}

fn translation_schema() -> Value {
    serde_json::to_value(schemars::schema_for!(TranslationBatchSchema))
        .expect("TranslationBatchSchema serializes")
}

/// Translate a batch of (category, purpose) pairs into `language` in ONE call.
/// Returns Err if the model's item count doesn't match the input (so the caller
/// can fall back to per-item translation or keep the originals).
pub fn translate_batch(
    endpoint: &str,
    pairs: &[(String, String)],
    language: &str,
) -> Result<Vec<(String, String)>, String> {
    if pairs.is_empty() {
        return Ok(Vec::new());
    }
    let input = serde_json::to_string(
        &pairs
            .iter()
            .map(|(c, p)| serde_json::json!({"category": c, "purpose": p}))
            .collect::<Vec<_>>(),
    )
    .map_err(|e| e.to_string())?;
    let system = format!(
        "You are a professional translator. Rewrite each item's 'category' and 'purpose' fields \
         ENTIRELY in {language}. You MUST output {language} only — never copy the source words, \
         and never leave any text in the original language. Preserve meaning and technical terms \
         (proper nouns like product names may stay). Return a JSON object {{\"items\": [...]}} with \
         EXACTLY the same number of items, in the same order."
    );
    let user = format!(
        "Translate every category and purpose below into {language}:\n{input}"
    );
    let max_tokens = ((pairs.len() as u32) * 90).clamp(256, 1500);
    let body = crate::enrich::client::build_json_body(&system, &user, translation_schema(), max_tokens);
    let raw = crate::enrich::client::chat(endpoint, &body)?;
    parse_translation(&raw, pairs.len())
}

/// Parse a translation batch, requiring exactly `expected` items.
fn parse_translation(raw: &str, expected: usize) -> Result<Vec<(String, String)>, String> {
    let trimmed = raw.trim();
    let try_parse = |s: &str| serde_json::from_str::<TranslationBatchOut>(s).ok();
    let out = try_parse(trimmed)
        .or_else(|| {
            let start = trimmed.find('{')?;
            let end = trimmed.rfind('}')?;
            try_parse(&trimmed[start..=end])
        })
        .ok_or_else(|| format!("translation: no valid JSON.\nRaw:\n{raw}"))?;
    if out.items.len() != expected {
        return Err(format!(
            "translation count mismatch: got {}, expected {}",
            out.items.len(),
            expected
        ));
    }
    Ok(out
        .items
        .into_iter()
        .map(|i| (i.category, i.purpose))
        .collect())
}

/// Ask the LLM for a holistic summary and cleanup plan.
pub fn summarize_report(
    endpoint: &str,
    _model: &str,
    safe_items: &[DirSummary],
    caution_items: &[DirSummary],
    system_items: &[DirSummary],
    unknown_items: &[DirSummary],
    total_size_mb: f64,
    safe_mb: f64,
    caution_mb: f64,
    system_mb: f64,
    unknown_mb: f64,
    safe_total: usize,
    caution_total: usize,
    system_total: usize,
    unknown_total: usize,
    language: Option<&str>,
) -> Result<FinalReport, String> {
    fn fmt_group(
        label: &str,
        items: &[DirSummary],
        mb: f64,
        shown: usize,
        total: usize,
    ) -> String {
        if items.is_empty() {
            return format!("{label}: (none) — {mb:.1} MB");
        }
        let trunc = if shown < total {
            format!(" (top {shown} of {total})")
        } else {
            String::new()
        };
        let mut out = format!("{label} ({mb:.1} MB, {total} items){trunc}:\n");
        for it in items {
            out.push_str(&format!("  - [{}] — {}\n", it.category, it.purpose));
        }
        out
    }
    let safe_b = fmt_group(
        "SAFE (can delete)", safe_items, safe_mb, safe_items.len(), safe_total,
    );
    let caution_b = fmt_group(
        "CAUTION (review)", caution_items, caution_mb, caution_items.len(), caution_total,
    );
    let system_b = fmt_group(
        "SYSTEM (keep)", system_items, system_mb, system_items.len(), system_total,
    );
    let unknown_b = fmt_group(
        "UNKNOWN", unknown_items, unknown_mb, unknown_items.len(), unknown_total,
    );
    let user_prompt = format!(
        "Disk scan complete. {total_size_mb:.1} MB analyzed across all items.\n\n{safe_b}\n\n{caution_b}\n\n{system_b}\n\n{unknown_b}"
    );

    // The report prompt is bounded (mod.rs caps items to the top-N per group),
    // so it fits the per-slot context; `enable_thinking=false` keeps the answer
    // in `content` (no reasoning phase to drain into `thinking`).  Output is
    // capped at 1024 tokens — enough for overview + summaries + cleanup plan
    // while leaving headroom in a 4096-token slot.
    let mut system = system_prompt_report();
    match language.filter(|l| !l.is_empty()) {
        Some(lang) => system.push_str(&format!(
            "\n\nIMPORTANT: Write every field ENTIRELY in {lang}. Do not use any other language."
        )),
        // Preserve prior behavior when no language is requested.
        None => system.push_str("\n\nUse Chinese if the paths suggest a Chinese user, otherwise English."),
    }
    let body = crate::enrich::client::build_json_body(
        &system,
        &user_prompt,
        report_schema(),
        1024,
    );
    let raw = crate::enrich::client::chat(endpoint, &body)?;
    log::info!(
        "[LLM] Report response: len={} preview={:.80}",
        raw.len(),
        raw.chars().take(80).collect::<String>(),
    );
    parse_final_report(&raw)
}

// ---- System prompts ----

fn system_prompt_dir() -> String {
    r#"You are a Windows disk cleanup analyzer. Given a directory path, its size,
content summary, and sample child names, classify it and describe its purpose.
Output valid JSON matching the schema: {"category": "...", "purpose": "...", "risk": "..."}.
Use Chinese for labels.

对用户目录（Downloads/Videos/Desktop/Documents 及个人项目目录），目录名本身意义不大；
当 Content summary 列出 "largest files:" 时，purpose 必须点出其中最占空间的具体大文件
（文件名 + 大小），帮助用户判断这些大文件是否要清理，而不是只复述目录用途。

IMPORTANT: Do NOT assume every CAUTION item is "game data" or "Steam". Consider
the full variety of reasons a directory may require review: drivers, professional
software, old projects, hardware utilities, SDK toolchains, installers.

关键反模式（最常被搞错）：不要因为"不认识这个厂商"就把目录当成缓存/可安全删除(safe)。
位于 Program Files\<厂商>\ 或 Program Files (x86)\<厂商>\ 之下、且目录名本身不是
cache/temp/log/crashpad 的目录，都是【已安装的软件】，绝不能标 safe：
- 运行时/框架/驱动核心 (.NET、dotnet、Microsoft Shared、VC++ Redist、打印机/扫描仪驱动) → system
- 可选厂商工具/外设软件 (ASUS Armoury、MSI Center、Razer Synapse、奔图Pantum 打印/扫描软件) → caution
只有当目录名明确含 cache/temp/log，或路径位于已知缓存位置时，才考虑 safe。

## Risk guidelines

### SAFE to delete
- Build/cache artifacts: target/ build/ dist/ out/ obj/ generated/ __pycache__/
- Package manager dirs: node_modules/ .venv/ venv/ vendor/ packages/ .npm/
- Temp & cache: .cache/ cache/ tmp/ temp/ Temp/ .temp/
- Log dirs: logs/ _logs/ log/ — directories dominated by .log files
- IDE caches (NOT config): .idea/ .vs/ .vscode-server/ — transient data
- Crash dumps, stale download caches, browser temp data
- Driver extraction directories: C:\AMD\ C:\NVIDIA\ C:\Intel\ — left by driver installers
- Shader/precompiled caches: dxcache/ glcache/ compiled/
- Windows maintenance caches: Prefetch, WinSxS\ManifestCache

### CAUTION — review before deleting
- Large downloaded archives/dirs (>500 MB total)
- Old backup dirs: backup_... old_... archive_... Copy of ...
- Old git repos or abandoned side projects you may no longer need
- Versioned SDKs/tools with multiple versions (keep the latest)
- Game data — often large but re-downloadable
- OEM driver installation packages (e.g. eSupport\eDriver) — only delete after confirming drivers work
- GPU driver installer caches (e.g. AMD_Graphic..., NVIDIA DisplayDriver) — large, re-downloadable
- Professional software data: Xilinx/Vivado IP cores, MATLAB toolboxes, old Altium versions
- Hardware utility install data: ASUS ROG, MSI Dragon, Razer Synapse backup packages
- Content creation caches: DaVinci Resolve render cache, Adobe Media Cache
- MSI/WIX installer source caches (ProgramData\Package Cache)
- Named-version application install dirs (e.g. app\2.3.4\) — keep latest, delete old

### SYSTEM — do NOT delete
- User profile root: C:\Users\X\ — the profile itself
- Project source: dirs containing Cargo.toml package.json .git/ CMakeLists.txt
- IDE user config: .codebuddy/ .cursor/ .vscode/ — user settings, not cache
- Windows system: C:\Windows\ C:\Program Files\ C:\Program Files (x86)\ C:\ProgramData\
- Active application data: AppData\Local\ (if belonging to software you use)
- Development toolchains: Python/Rust/Go/Node.js installations
- LaTeX distros (identified by texmf-dist/)
- FPGA/EDA tool installations: Xilinx/ Altera/ Vivado/
- Installed software under Program Files (unless clearly a versioned SDK with old copies)

## Few-shot examples
Path: C:\Users\dongm\AppData\Local\Temp\
→ {"category": "临时文件", "purpose": "系统和应用临时文件，可安全删除", "risk": "safe"}

Path: C:\Users\dongm\github\disk_organizer\target\
→ {"category": "Rust构建产物", "purpose": "Cargo build输出，可安全删除的target目录", "risk": "safe"}

Path: C:\Users\dongm\.codebuddy\
→ {"category": "IDE用户数据", "purpose": "CodeBuddy AI助手的用户配置和数据，应保留", "risk": "system"}

Path: D:\Steam\steamapps\common\OldGame\
→ {"category": "游戏数据", "purpose": "Steam游戏安装目录，需确认是否仍玩后再决定删除", "risk": "caution"}

Path: C:\eSupport\eDriver\Software\Driver\DCH\Online\Graphic\AMD\AMD_Graphic_DriverOnly_ROG\
→ {"category": "AMD驱动安装包", "purpose": "ASUS笔记本AMD显卡驱动安装文件；确认驱动正常工作后可删除", "risk": "caution"}

Path: C:\NVIDIA\DisplayDriver\528.02\
→ {"category": "NVIDIA驱动解压目录", "purpose": "NVIDIA显卡驱动安装时留下的解压目录，可安全删除", "risk": "safe"}

Path: C:\ProgramData\Package Cache\
→ {"category": "MSI安装源缓存", "purpose": "WIX/MSI软件安装包缓存；已安装的软件对应的缓存可安全删除", "risk": "safe"}

Path: C:\Program Files\NVIDIA Corporation\Installer2\
→ {"category": "NVIDIA驱动缓存", "purpose": "NVIDIA驱动安装源缓存，驱动安装后可安全清理", "risk": "safe"}

Path: D:\OldProjects\abandoned_web_demo_2022\
→ {"category": "废弃项目", "purpose": "旧项目目录，确认不再需要后可删除备份后清理", "risk": "caution"}

Path: C:\Program Files\Tencent\WeMeet\3.36.1.445\
→ {"category": "腾讯会议旧版本", "purpose": "腾讯会议旧版安装目录，可保留最新版本删除旧版", "risk": "caution"}

Path: C:\Program Files\dotnet\shared\Microsoft.NETCore.App\
→ {"category": ".NET运行时", "purpose": ".NET Core共享运行时，已安装的应用程序依赖它运行，删除会导致程序无法启动", "risk": "system"}

Path: C:\Program Files\Pantum\OCR\Bin\
→ {"category": "奔图打印机软件", "purpose": "Pantum打印机OCR组件，属于已安装的打印机驱动软件，删除会影响打印/扫描功能", "risk": "caution"}

Path: C:\Program Files (x86)\ASUS\ArmouryDevice\
→ {"category": "华硕硬件工具", "purpose": "ASUS Armoury Crate设备驱动与控制软件；如不使用应通过卸载程序移除，而非直接删目录", "risk": "caution"}

Path: C:\SCANNER\ptm7100\
→ {"category": "扫描仪驱动软件", "purpose": "奔图(Pantum)扫描仪驱动程序目录，删除会导致扫描功能不可用", "risk": "caution"}""#
        .to_string()
}

fn system_prompt_file() -> String {
    r#"You are a Windows disk cleanup analyzer. Given a file path, size, extension,
parent directory, and sibling files, classify it and describe its purpose.
Output valid JSON matching the schema: {"category": "...", "purpose": "...", "risk": "..."}.
Use Chinese for labels.
Do NOT restate the extension. Be concise.

IMPORTANT: Do NOT assume large .exe/.msi files are "game installers" by default.
Consider driver installers, SDK bundles, hardware utilities, and professional software.

关键反模式：不要因为"不确定"就把文件标成"临时日志/临时文件 safe"。只有当扩展名确实是
.log/.tmp/.dmp/.etl 等、或文件位于 Temp/缓存目录时，才可标 safe。位于 Program Files\、
软件安装目录、Steam\...\_CommonRedist 等位置下的 .exe/.dll/.dat/.bin/.emb 等，是【已安装
软件或可再发行运行时的组成部分】→ caution（可重装）或 system（核心依赖），绝不是"临时日志"。

## Risk guidelines

### SAFE to delete
- Build intermediates: .o .obj .pdb .ilk .exp .lib (intermediate only)
- Cache: .pyc .pyo .class
- Logs: .log .etl .dump .dmp .mdmp
- Temp: .tmp .temp .bak .swp .swo ~ (backup) files
- Old download leftovers in Downloads/ or Temp/
- Driver installation packages in root folders (C:\AMD\ C:\NVIDIA\ C:\Intel\) — left by driver installers

### CAUTION
- Large installers (>500 MB) — you may need them
- .zip .rar .7z .tar .gz archives — check contents
- .iso .vhd .vhdx disk images
- .pst .ost Outlook data
- Named backups: "Copy of ...", "... (backup)", "... (old)"
- GPU driver installer .exe (e.g. *-desktop-win10-win11-64bit*.exe) — large but re-downloadable
- SDK / toolchain installers (.exe .msi > 1GB) — check if the installed version is current
- FPGA bitstream / IP core archive files
- Professional software installer packages

### SYSTEM
- Source code: .rs .py .js .ts .c .cpp .h .java .go
- Config: .json .yaml .yml .toml .ini .cfg .conf .env
- Documents: .docx .xlsx .pptx .pdf (unless clearly outdated)
- Project files: Cargo.toml package.json CMakeLists.txt Makefile
- Database: .db .sqlite .sqlite3
- Executables in Program Files/ or toolchain paths
- System files in C:\Windows\ C:\ProgramData\

## Few-shot examples
Path: C:\Users\dongm\AppData\Local\Temp\DumpStack.log | 2MB | .log
→ {"category": "临时日志", "purpose": "系统临时崩溃日志，可安全删除", "risk": "safe"}

Path: C:\Users\dongm\Downloads\setup.exe | 150MB | .exe
→ {"category": "下载的安装程序", "purpose": "下载目录中的安装包，安装后即可删除", "risk": "caution"}

Path: D:\Projects\data\large_dataset.h5 | 2GB | .h5
→ {"category": "大型数据文件", "purpose": "HDF5科学数据文件，需确认是否仍在使用后再决定", "risk": "caution"}

Path: C:\AMD\AMD_Software_22.11.2.exe | 600MB | .exe
→ {"category": "AMD驱动安装包", "purpose": "AMD显卡驱动安装包，安装完成后即可删除", "risk": "safe"}

Path: C:\NVIDIA\528.02-desktop-win10-win11-64bit.exe | 800MB | .exe
→ {"category": "NVIDIA驱动安装包", "purpose": "NVIDIA显卡驱动安装包，安装后可删除", "risk": "safe"}

Path: C:\eSupport\eDriver\...\nvoptix.bi_ | 20MB | .bi_
→ {"category": "NVIDIA组件文件", "purpose": "NVIDIA Optix光线追踪驱动组件，属于驱动安装包，可随驱动安装包删除", "risk": "safe"}

Path: C:\Program Files (x86)\Steam\steamapps\common\...\_CommonRedist\DotNet\4.8\ndp48-x86-x64-allos-enu.exe | 117MB | .exe
→ {"category": ".NET可再发行运行时", "purpose": "游戏附带的.NET 4.8运行时安装包，删除后需要时要重新安装；不是临时日志", "risk": "caution"}

Path: C:\Program Files\Pantum\OCR\Data\bcr_embeddings.emb | 89MB | .emb
→ {"category": "打印机OCR数据文件", "purpose": "奔图打印机OCR软件的模型/字典数据，属于已安装软件组成部分，删除会影响OCR功能", "risk": "caution"}""#
        .to_string()
}

fn system_prompt_report() -> String {
    "\
You are a disk cleanup advisor. Given categorized scan data, produce a cleanup plan.
Output ONLY the JSON object with fields: overview, safe_summary, caution_advice, cleanup_plan.

IMPORTANT: safe_summary must ONLY describe SAFE items (things that can be deleted).
caution_advice must ONLY describe CAUTION items (things that need review).
Do NOT repeat the same categories in both sections — each category belongs to exactly one risk group.
Focus on the DISTINCTIVE characteristics of each group:
- SAFE: what is clearly deletable and why (caches, temps, build outputs)
- CAUTION: what requires human judgment and what to look for (versioned installs, old projects, backups)
- Output at most 2-3 sentences per section. Be concrete and actionable."
        .to_string()
}

// ---- Parsing ----

/// Parse a risk string from LLM output into our Risk enum.
pub fn parse_risk(llm_risk: Option<&str>) -> crate::model::Risk {
    match llm_risk.map(|s| s.trim().to_lowercase()).as_deref() {
        Some("safe") => crate::model::Risk::Safe,
        Some("caution") => crate::model::Risk::Caution,
        Some("system") => crate::model::Risk::System,
        _ => crate::model::Risk::Unknown,
    }
}

/// Zero-误删 redline: the LLM may guess a category/purpose, but it must NEVER
/// mark an item "safe to delete" — only deterministic rules may declare Safe.
/// A guessed Safe is demoted to Unknown (e.g. the model called a source repo
/// containing a `target/` dir "safe"); conservative Caution/System guesses,
/// which lean toward keeping, pass through unchanged.
pub fn clamp_llm_risk(r: crate::model::Risk) -> crate::model::Risk {
    match r {
        crate::model::Risk::Safe => crate::model::Risk::Unknown,
        other => other,
    }
}

/// Parse DirSummary from JSON.  With `format` JSON Schema constraint,
/// the model outputs clean JSON directly.  The extraction fallback handles
/// rare cases where the model wraps JSON in surrounding text.
fn parse_summary(raw: &str) -> Result<DirSummary, String> {
    let trimmed = raw.trim();

    // 1) Direct JSON parse.
    if let Ok(s) = serde_json::from_str::<DirSummary>(trimmed) {
        return Ok(s);
    }

    // 2) Find the first { ... } JSON block (safety net).
    if let Some(start) = trimmed.find('{') {
        if let Some(end) = trimmed.rfind('}') {
            let slice = &trimmed[start..=end];
            if let Ok(s) = serde_json::from_str::<DirSummary>(slice) {
                return Ok(s);
            }
        }
    }

    Err(format!(
        "LLM did not produce valid JSON.\nRaw output:\n{raw}"
    ))
}

/// Try to parse the LLM response as FinalReport, with fallback.
fn parse_final_report(raw: &str) -> Result<FinalReport, String> {
    let trimmed = raw.trim();

    if let Ok(r) = serde_json::from_str::<FinalReport>(trimmed) {
        return Ok(r);
    }

    if let Some(start) = trimmed.find('{') {
        if let Some(end) = trimmed.rfind('}') {
            let slice = &trimmed[start..=end];
            if let Ok(r) = serde_json::from_str::<FinalReport>(slice) {
                return Ok(r);
            }
        }
    }

    Err(format!(
        "LLM did not produce a valid final report.\nRaw output:\n{trimmed}"
    ))
}

// ---- Tests ----

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_direct_json() {
        let s =
            parse_summary(r#"{"category": "cache", "purpose": "temp files"}"#).unwrap();
        assert_eq!(s.category, "cache");
    }

    #[test]
    fn parse_json_inside_noise() {
        let raw =
            "Here is:\n```json\n{\"category\": \"logs\", \"purpose\": \"app logs\"}\n```";
        let s = parse_summary(raw).unwrap();
        assert_eq!(s.category, "logs");
    }

    #[test]
    fn parse_garbled_fails() {
        assert!(parse_summary("not json at all").is_err());
    }

    #[test]
    fn translation_parses_and_validates_count() {
        let raw = r#"{"items":[{"category":"Cache","purpose":"safe to delete"},{"category":"Logs","purpose":"app logs"}]}"#;
        let out = parse_translation(raw, 2).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].0, "Cache");
        assert_eq!(out[1].1, "app logs");
    }

    #[test]
    fn translation_count_mismatch_errors() {
        let raw = r#"{"items":[{"category":"Cache","purpose":"x"}]}"#;
        assert!(parse_translation(raw, 3).is_err());
    }

    #[test]
    fn translation_garbage_errors() {
        assert!(parse_translation("not json", 1).is_err());
    }

    #[test]
    fn llm_safe_is_demoted_to_unknown() {
        use crate::model::Risk;
        // The redline: a guessed Safe must never stick.
        assert_eq!(clamp_llm_risk(Risk::Safe), Risk::Unknown);
        // Conservative guesses pass through.
        assert_eq!(clamp_llm_risk(Risk::Caution), Risk::Caution);
        assert_eq!(clamp_llm_risk(Risk::System), Risk::System);
        assert_eq!(clamp_llm_risk(Risk::Unknown), Risk::Unknown);
    }
}
