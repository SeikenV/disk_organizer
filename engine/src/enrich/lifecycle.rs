//! Model lifecycle + backend reachability for LLM enrichment.
//!
//! These functions probe the inference backend itself (is it up, is the model
//! warm) as opposed to the per-item prompting in `llm.rs`. With the llama-server
//! backend the model is loaded when the server starts (see `server.rs`), so
//! "health" is a `GET /health` poll and "preload" is a tiny priming request.
//! Splitting them out keeps a clean seam for pooling multiple LLM backends.

/// True if a llama-server is reachable and ready at `endpoint`.
///
/// llama-server answers `GET /health` with `200 {"status":"ok"}` once the model
/// is loaded (and `503` while still loading), so a 2xx here means ready to serve.
pub fn health_check(endpoint: &str) -> bool {
    let http = crate::enrich::client::local_client();
    http.get(format!("{endpoint}/health"))
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Prime the model so the first real request isn't a cold start.
///
/// The model is already resident once the server is healthy; this sends a tiny
/// generation to warm the first-token path. Failures are surfaced so callers can
/// fall back to rule/heuristic-only enrichment.
pub fn preload_model(endpoint: &str, _model: &str) -> Result<(), String> {
    let body = serde_json::json!({
        "messages": [{"role": "user", "content": "."}],
        "max_tokens": 8,
        "temperature": 0.0,
        "chat_template_kwargs": {"enable_thinking": false}
    });
    crate::enrich::client::chat(endpoint, &body)?;
    Ok(())
}
