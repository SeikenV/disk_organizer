// Low-level Ollama HTTP client.
use crate::consts::{HEALTH_TIMEOUT, REPORT_TIMEOUT, REQUEST_TIMEOUT};
use serde::{Deserialize, Serialize};

// ---- Ollama API types ----

#[derive(Serialize)]
struct GenerateRequest {
    model: String,
    prompt: String,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<GenerateOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    format: Option<serde_json::Value>,
}

#[derive(Serialize)]
struct GenerateOptions {
    temperature: f64,
    /// Not set: let the model output as many tokens as it wants (local model, no cost).
    #[serde(skip_serializing_if = "Option::is_none")]
    num_predict: Option<u32>,
}

#[derive(Deserialize)]
struct GenerateResponse {
    response: String,
}

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

/// True if an Ollama-compatible server is reachable at `endpoint`.
pub fn health_check(endpoint: &str) -> bool {
    reqwest::blocking::Client::new()
        .get(format!("{endpoint}/api/tags"))
        .timeout(HEALTH_TIMEOUT)
        .send()
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Ask the LLM to summarize a directory.
pub fn summarize_dir(
    client: &reqwest::blocking::Client,
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
    let system = system_prompt_dir();
    let user = format!(
        "Directory: {dir_path}{ancestor_line}\nSample contents:\n{samples_str}"
    );
    let raw = chat(client, endpoint, model, &system, &user)?;
    parse_summary(&raw)
}

/// Ask the LLM to analyze a large file.
pub fn summarize_file(
    client: &reqwest::blocking::Client,
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
    let system = system_prompt_file();
    let user = format!(
        "File: {file_path}\nExtension: .{ext}\nParent directory: {parent_dir}{ancestor_line}\nSibling files: {sibs_str}"
    );
    let raw = chat(client, endpoint, model, &system, &user)?;
    parse_summary(&raw)
}

/// Ask the LLM for a holistic summary and cleanup plan.
pub fn summarize_report(
    _client: &reqwest::blocking::Client,
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
    fn fmt_group(label: &str, items: &[DirSummary], mb: f64, shown: usize, total: usize) -> String {
        if items.is_empty() {
            return format!("{label}: (none) — {mb:.1} MB");
        }
        let trunc = if shown < total { format!(" (top {shown} of {total})") } else { String::new() };
        let mut out = format!("{label} ({mb:.1} MB, {total} items){trunc}:\n");
        for it in items {
            out.push_str(&format!("  - [{}] — {}\n", it.category, it.purpose));
        }
        out
    }
    let safe_b = fmt_group("SAFE (can delete)", safe_items, safe_mb, safe_items.len(), safe_total);
    let caution_b = fmt_group("CAUTION (review)", caution_items, caution_mb, caution_items.len(), caution_total);
    let system_b = fmt_group("SYSTEM (keep)", system_items, system_mb, system_items.len(), system_total);
    let unknown_b = fmt_group("UNKNOWN", unknown_items, unknown_mb, unknown_items.len(), unknown_total);
    let user_prompt = format!(
        "Disk scan complete. {total_size_mb:.1} MB analyzed across all items.\n\n{safe_b}\n\n{caution_b}\n\n{system_b}\n\n{unknown_b}"
    );
    // Use a separate client with shorter timeout for the report.
    let report_client = reqwest::blocking::Client::builder()
        .timeout(REPORT_TIMEOUT)
        .build()
        .map_err(|e| format!("Build client: {e}"))?;
    let raw = chat_with_schema(
        &report_client, endpoint, model,
        &system_prompt_report(), &user_prompt,
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "overview": {"type": "string"},
                "safe_summary": {"type": "string"},
                "caution_advice": {"type": "string"},
                "cleanup_plan": {"type": "array", "items": {"type": "string"}}
            },
            "required": ["overview", "safe_summary", "caution_advice", "cleanup_plan"]
        })),
    )?;
    parse_final_report(&raw)
}

// ---- Compressed prompts (enough context, minimal overthinking) ----

fn system_prompt_dir() -> String {
    "\
Classify dir. Output:
category: <2-6 words>
purpose: <1 sentence, safe to delete?>

Rules: C:\\Users\\X→keep. project(src/,Cargo.toml,.git/)→keep. cache/node_modules/venv/build/target/dist→safe delete. Be specific.".to_string()
}

fn system_prompt_file() -> String {
    "\
Classify file. Output:
category: <2-6 words>
purpose: <1 sentence, safe to delete?>

Use path context. Don't restate extension. Be concise.".to_string()
}

fn system_prompt_report() -> String {
    "\
You are a disk cleanup advisor. Given categorized scan data, produce a cleanup plan.
Output ONLY a JSON object with fields:
- overview (2-3 sentences)
- safe_summary (1-2 sentences)
- caution_advice (2-3 sentences)
- cleanup_plan (array of 5-10 steps)
Use Chinese if the paths suggest a Chinese user.".to_string()
}

// ---- Internal helpers ----

fn chat(
    client: &reqwest::blocking::Client,
    endpoint: &str,
    model: &str,
    system: &str,
    user: &str,
) -> Result<String, String> {
    // Use /api/generate with plain-text prompt (no JSON schema constraint).
    // Constrained decoding (`format`) is extremely slow on GGUF models from
    // HuggingFace/ModelScope.  We extract category/purpose client-side.
    chat_with_schema(client, endpoint, model, system, user, None)
}

fn chat_with_schema(
    client: &reqwest::blocking::Client,
    endpoint: &str,
    model: &str,
    system: &str,
    user: &str,
    format: Option<serde_json::Value>,
) -> Result<String, String> {
    let prompt = format!("System: {system}\n\nUser: {user}\n\nReply:");
    let request = GenerateRequest {
        model: model.to_string(),
        prompt,
        stream: false,
        options: Some(GenerateOptions {
            temperature: 0.1,
            num_predict: None,  // no token cap — let REQUEST_TIMEOUT be the only limit
        }),
        format,
    };
    send_generate(client, endpoint, &request)
}

fn send_generate(client: &reqwest::blocking::Client, endpoint: &str, request: &GenerateRequest) -> Result<String, String> {
    let resp = client
        .post(format!("{endpoint}/api/generate"))
        .json(request)
        .timeout(REQUEST_TIMEOUT)
        .send()
        .map_err(|e| format!("HTTP error: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().unwrap_or_else(|_| "(no body)".into());
        return Err(format!("Ollama returned {status}: {body}"));
    }

    let body: GenerateResponse = resp.json().map_err(|e| format!("JSON parse error: {e}"))?;
    Ok(body.response)
}

/// Try to parse the LLM response as DirSummary, with fallbacks.
fn parse_summary(raw: &str) -> Result<DirSummary, String> {
    // Strip <think>...</think> blocks (reasoning-distilled models emit these
    // regardless of prompt; the useful answer comes after </think>).
    let trimmed = strip_think(raw).trim().to_string();
    let trimmed = if trimmed.is_empty() { raw.trim() } else { trimmed.as_str() };

    // 1) Direct JSON parse.
    if let Ok(s) = serde_json::from_str::<DirSummary>(trimmed) {
        return Ok(s);
    }

    // 2) Find the first { ... } JSON block.
    if let Some(start) = trimmed.find('{') {
        if let Some(end) = trimmed.rfind('}') {
            let slice = &trimmed[start..=end];
            if let Ok(s) = serde_json::from_str::<DirSummary>(slice) {
                return Ok(s);
            }
        }
    }

    // 3) Plain text fallback: "category: ..., purpose: ..."
    if let Some((cat, pur)) = extract_category_purpose(trimmed) {
        return Ok(DirSummary { category: cat, purpose: pur });
    }

    // 4) Last resort: try parsing the original raw text (with think block).
    if !trimmed.is_empty() && trimmed != raw.trim() {
        if let Some((cat, pur)) = extract_category_purpose(raw) {
            return Ok(DirSummary { category: cat, purpose: pur });
        }
    }

    Err(format!("LLM did not produce parseable output.\nRaw output:\n{raw}"))
}

/// Remove `<think>...</think>` (and similar variants) from text.
fn strip_think(text: &str) -> String {
    // Find all <think>...</think> blocks and remove them.
    let mut result = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<think") {
        result.push_str(&rest[..start]);
        // Find the matching </think>
        let after_tag = &rest[start..];
        if let Some(inner_start) = after_tag.find('>') {
            let after_open = &after_tag[inner_start + 1..];
            if let Some(close) = after_open.find("</think>") {
                rest = &after_open[close + 8..];
                continue;
            }
        }
        // Malformed: push "<think" and continue
        result.push_str("<think");
        rest = &rest[start + 6..];
    }
    result.push_str(rest);
    result
}

/// Extract "category: X, purpose: Y" from plain text (case-insensitive).
fn extract_category_purpose(text: &str) -> Option<(String, String)> {
    let lower = text.to_lowercase();

    let cat_pos = lower.find("category")?;
    let cat_rest = &text[cat_pos + 8..]; // after "category"
    let cat_val = cat_rest
        .trim_start_matches(|c: char| c == ':' || c == '：' || c.is_whitespace());
    // Category ends at the next known marker, newline, or comma
    let cat_end = cat_val.find(|c: char| c == '\n' || c == '\r' || c == ',' || c == ';')
        .unwrap_or(cat_val.len());
    let cat_val = cat_val[..cat_end].trim().trim_matches(|c: char| c == '"' || c == '\'').trim();
    if cat_val.is_empty() || cat_val.len() > 100 {
        return None;
    }

    let purp_pos = lower.find("purpose")?;
    let purp_rest = &text[purp_pos + 7..]; // after "purpose"
    let purp_val = purp_rest
        .trim_start_matches(|c: char| c == ':' || c == '：' || c.is_whitespace());
    // Purpose ends at newline or end
    let purp_end = purp_val.find(|c: char| c == '\n' || c == '\r').unwrap_or(purp_val.len());
    let purp_val = purp_val[..purp_end].trim().trim_matches(|c: char| c == ',' || c == ';' || c == '.' || c == '"' || c == '\'').trim();
    if purp_val.is_empty() {
        return None;
    }

    Some((cat_val.to_string(), purp_val.to_string()))
}

/// Try to parse the LLM response as FinalReport, with fallbacks.
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

    Err(format!("LLM did not produce a valid final report.\nRaw output:\n{trimmed}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_direct_json() {
        let s = parse_summary(r#"{"category": "cache", "purpose": "temp files"}"#).unwrap();
        assert_eq!(s.category, "cache");
    }

    #[test]
    fn parse_json_inside_noise() {
        let raw = "Here is:\n```json\n{\"category\": \"logs\", \"purpose\": \"app logs\"}\n```";
        let s = parse_summary(raw).unwrap();
        assert_eq!(s.category, "logs");
    }

    #[test]
    fn parse_garbled_fails() {
        assert!(parse_summary("not json at all").is_err());
    }
}
