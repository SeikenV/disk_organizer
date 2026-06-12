// Ollama LLM client — uses ollama-rs community crate.
//
//   https://github.com/pepperoni21/ollama-rs
//
// Replaces the hand-rolled reqwest calls with the official community library.
//
// Key API features used:
//   * `think(false)` — disables reasoning phase (top-level param).
//   * `format(FormatType::Json)` / `format(schema_value)` — structured output.
//   * `.system(...)` — system prompt as proper field.
//   * `keep_alive(KeepAlive::Until { timestamp: ... })` — keep model loaded.
//   * `options(ModelOptions::default().temperature(0.1).num_predict(300))` — gen params.

use ollama_rs::generation::completion::request::GenerationRequest;
use ollama_rs::generation::completion::GenerationResponse;
use ollama_rs::generation::parameters::{FormatType, JsonStructure, KeepAlive, TimeUnit};
use ollama_rs::models::ModelOptions;
use ollama_rs::Ollama;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use std::time::Duration;

// ---- Our domain types ----

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirSummary {
    pub category: String,
    pub purpose: String,
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
// We derive schemars::JsonSchema on empty structs whose field names + types
// match the JSON shape we want Ollama to produce.  ollama-rs serializes these
// as the request's `format` parameter via FormatType::StructuredJson.
//
// Note: the derive places `"additionalProperties": false` by default.
// Ollama handles this fine, but if a model ever chokes we can relax it.
// The `allow(dead_code)` silences warnings — fields are only read by the derive macro.

#[allow(dead_code)]
#[derive(JsonSchema)]
struct DirSummarySchema {
    /// 2-6 word classification
    category: String,
    /// 1 sentence, include safe-to-delete assessment
    purpose: String,
}

#[allow(dead_code)]
#[derive(JsonSchema)]
struct ReportSchema {
    overview: String,
    safe_summary: String,
    caution_advice: String,
    cleanup_plan: Vec<String>,
}

// ---- Tokio runtime for blocking-in-async ----

fn tk_rt() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| tokio::runtime::Runtime::new().expect("tokio runtime init"))
}

// ---- Helpers ----

/// Parse endpoint URL into an `Ollama` client.
fn ollama_from_endpoint(endpoint: &str) -> Ollama {
    let without_proto = endpoint
        .trim_start_matches("http://")
        .trim_start_matches("https://");
    if let Some(colon_pos) = without_proto.rfind(':') {
        let host = format!("http://{}", &without_proto[..colon_pos]);
        let port: u16 = without_proto[colon_pos + 1..].parse().unwrap_or(11434);
        Ollama::new(host, port)
    } else {
        Ollama::default()
    }
}

/// Default model options for classification tasks.
fn default_opts() -> ModelOptions {
    ModelOptions::default()
        .temperature(0.1_f32)
        .num_predict(300)
}

/// Keep model alive for 10 minutes from now.
fn keep_10m() -> KeepAlive {
    KeepAlive::Until {
        time: 10,
        unit: TimeUnit::Minutes,
    }
}

fn dir_format() -> FormatType {
    FormatType::StructuredJson(Box::new(JsonStructure::new::<DirSummarySchema>()))
}

fn report_format() -> FormatType {
    FormatType::StructuredJson(Box::new(JsonStructure::new::<ReportSchema>()))
}

/// Build a GenerationRequest with common settings.
fn classify_request<'a>(
    model: &'a str,
    system: &'a str,
    prompt: &'a str,
    format: FormatType,
) -> GenerationRequest<'a> {
    GenerationRequest::new(model.to_string(), prompt.to_string())
        .system(system.to_string())
        .think(true)
        .format(format)
        .options(default_opts())
        .keep_alive(keep_10m())
}

/// Make a synchronous blocking call to Ollama.
fn block_generate(
    ollama: &Ollama,
    req: GenerationRequest<'_>,
) -> Result<GenerationResponse, String> {
    tk_rt()
        .block_on(async { ollama.generate(req).await })
        .map_err(|e| format!("Ollama error: {e}"))
}

// ---- Public API ----

/// True if an Ollama-compatible server is reachable at `endpoint`.
pub fn health_check(endpoint: &str) -> bool {
    let ollama = ollama_from_endpoint(endpoint);
    tk_rt()
        .block_on(async { ollama.list_local_models().await })
        .is_ok()
}

/// Preload a model into GPU memory so the first real request isn't cold-start.
///
/// Sends a tiny generation request and sets `keep_alive` to 10 minutes.
/// Subsequent requests with `keep_alive` will extend the lifetime.
pub fn preload_model(endpoint: &str, model: &str) -> Result<(), String> {
    let ollama = ollama_from_endpoint(endpoint);
    let req = GenerationRequest::new(model.to_string(), ".")
        .think(true)
        .keep_alive(keep_10m());
    block_generate(&ollama, req)?;
    Ok(())
}

/// Ask the LLM to summarize a directory.
pub fn summarize_dir(
    endpoint: &str,
    model: &str,
    dir_path: &str,
    sample_entries: &[String],
    ancestor_context: Option<&str>,
) -> Result<DirSummary, String> {
    let samples_str = if sample_entries.is_empty() {
        "(empty)".to_string()
    } else {
        sample_entries.join(", ")
    };
    let ancestor_line = ancestor_context
        .map(|c| format!("\nAncestor context: {c}"))
        .unwrap_or_default();
    let user = format!(
        "Directory: {dir_path}{ancestor_line}\nSample contents:\n{samples_str}"
    );
    let system = system_prompt_dir();

    let ollama = ollama_from_endpoint(endpoint);
    let req = classify_request(model, &system, &user, dir_format());
    let raw = block_generate(&ollama, req)?.response;
    parse_summary(&raw)
}

/// Ask the LLM to analyze a large file.
pub fn summarize_file(
    endpoint: &str,
    model: &str,
    file_path: &str,
    parent_dir: &str,
    sibling_files: &[String],
    ext: &str,
    ancestor_context: Option<&str>,
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
        "File: {file_path}\nExtension: .{ext}\nParent directory: {parent_dir}{ancestor_line}\nSibling files: {sibs_str}"
    );
    let system = system_prompt_file();

    let ollama = ollama_from_endpoint(endpoint);
    let req = classify_request(model, &system, &user, dir_format());
    let raw = block_generate(&ollama, req)?.response;
    parse_summary(&raw)
}

/// Ask the LLM for a holistic summary and cleanup plan.
pub fn summarize_report(
    endpoint: &str,
    model: &str,
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

    let ollama = ollama_from_endpoint(endpoint);
    let req = GenerationRequest::new(model.to_string(), user_prompt)
        .system(system_prompt_report())
        .think(true)
        .format(report_format())
        .options(
            ModelOptions::default()
                .temperature(0.1_f32)
                .num_predict(600),
        )
        .keep_alive(keep_10m());

    // The report may take longer; wrap in a timeout.
    let result = tk_rt().block_on(async {
        tokio::time::timeout(
            Duration::from_secs(crate::consts::REPORT_TIMEOUT.as_secs()),
            ollama.generate(req),
        )
        .await
    });

    match result {
        Ok(Ok(resp)) => parse_final_report(&resp.response),
        Ok(Err(e)) => Err(format!("Ollama error: {e}")),
        Err(_elapsed) => Err("Report generation timed out".to_string()),
    }
}

// ---- System prompts ----

fn system_prompt_dir() -> String {
    "\
Classify the directory. You MUST output valid JSON matching the schema.
Rules: C:\\Users\\X→keep. project(src/,Cargo.toml,.git/)→keep. cache/node_modules/venv/build/target/dist→safe delete. Be specific."
        .to_string()
}

fn system_prompt_file() -> String {
    "\
Classify the file. You MUST output valid JSON matching the schema.
Use path context. Don't restate extension. Be concise."
        .to_string()
}

fn system_prompt_report() -> String {
    "\
You are a disk cleanup advisor. Given categorized scan data, produce a cleanup plan.
Output ONLY the JSON object with fields: overview, safe_summary, caution_advice, cleanup_plan.
Use Chinese if the paths suggest a Chinese user."
        .to_string()
}

// ---- Parsing ----

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
}
