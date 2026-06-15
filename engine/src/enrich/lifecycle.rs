//! Model lifecycle + backend reachability for LLM enrichment.
//!
//! These functions manage the inference backend itself (is it up, is the model
//! warm) as opposed to the per-item prompting in `llm.rs`. Splitting them out
//! gives a clean seam for swapping or pooling multiple LLM backends.

use ollama_rs::generation::completion::request::GenerationRequest;

use super::llm::{block_generate, keep_10m, ollama_from_endpoint, tk_rt};

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
