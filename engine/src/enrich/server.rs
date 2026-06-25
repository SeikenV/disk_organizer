//! llama-server process lifecycle. This file: context sizing (pure) now;
//! spawn/health/shutdown (Task 3) next.

/// Total `-c` value to pass llama-server so each of `parallel` slots gets at
/// least `per_slot` tokens. (llama-server divides total context across slots.)
pub fn total_context(parallel: usize, per_slot: usize) -> usize {
    parallel.max(1) * per_slot.max(1)
}

/// True if a request of `prompt_tokens` + `max_output` fits one slot.
pub fn fits_slot(per_slot: usize, prompt_tokens: usize, max_output: usize) -> bool {
    prompt_tokens + max_output <= per_slot
}

use crate::enrich::backend::Backend;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

pub struct ServerConfig {
    pub model_path: PathBuf,
    pub tools_dir: PathBuf,   // contains <backend>/llama-server(.exe)
    pub port: u16,
    pub parallel: usize,
    pub per_slot_ctx: usize,
    pub ngl: u32,             // GPU layers (ignored on CPU build)
    pub mmproj: Option<PathBuf>, // None for text; Some for vision (sub-project B)
}

pub struct LlamaServer {
    child: Child,
    endpoint: String,
}

impl LlamaServer {
    /// Spawn llama-server for `backend`, wait until /health is ok.
    pub fn start(backend: Backend, cfg: &ServerConfig) -> Result<LlamaServer, String> {
        let exe = cfg.tools_dir.join(backend.dir_name()).join(server_exe_name());
        if !exe.exists() {
            return Err(format!("llama-server not found: {}", exe.display()));
        }
        let ctx = super::server::total_context(cfg.parallel, cfg.per_slot_ctx);
        // Guard the per-slot-context rule: each slot must hold a typical prompt
        // plus its output, or requests fail with "exceeds context size". Our
        // real classification prompts run ~1645 tokens + ~300 output.
        const TYPICAL_PROMPT_TOKENS: usize = 1645;
        const TYPICAL_OUTPUT_TOKENS: usize = 300;
        if !fits_slot(cfg.per_slot_ctx, TYPICAL_PROMPT_TOKENS, TYPICAL_OUTPUT_TOKENS) {
            log::warn!(
                "[LLM] per_slot_ctx={} is below a typical prompt+output ({}+{} tokens); \
                 requests may fail. Raise --llm-per-slot-ctx or lower --llm-parallel.",
                cfg.per_slot_ctx, TYPICAL_PROMPT_TOKENS, TYPICAL_OUTPUT_TOKENS,
            );
        }
        let mut cmd = Command::new(&exe);
        cmd.arg("-m").arg(&cfg.model_path)
            .arg("--host").arg("127.0.0.1")
            .arg("--port").arg(cfg.port.to_string())
            .arg("--jinja")
            .arg("-c").arg(ctx.to_string())
            .arg("--parallel").arg(cfg.parallel.to_string());
        if matches!(backend, Backend::Cuda | Backend::Vulkan) {
            cmd.arg("-ngl").arg(cfg.ngl.to_string());
        }
        if let Some(mm) = &cfg.mmproj {
            cmd.arg("--mmproj").arg(mm);
        }
        let child = cmd.spawn().map_err(|e| format!("spawn llama-server: {e}"))?;
        let endpoint = format!("http://127.0.0.1:{}", cfg.port);
        let server = LlamaServer { child, endpoint };
        server.wait_healthy(Duration::from_secs(60))?;
        Ok(server)
    }

    pub fn endpoint(&self) -> &str { &self.endpoint }

    fn wait_healthy(&self, timeout: Duration) -> Result<(), String> {
        let url = format!("{}/health", self.endpoint);
        let deadline = Instant::now() + timeout;
        let http = reqwest::blocking::Client::new();
        while Instant::now() < deadline {
            if let Ok(r) = http.get(&url).timeout(Duration::from_secs(2)).send() {
                if r.status().is_success() {
                    return Ok(());
                }
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        Err("llama-server did not become healthy in time".into())
    }
}

impl Drop for LlamaServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn server_exe_name() -> &'static str {
    if cfg!(windows) { "llama-server.exe" } else { "llama-server" }
}

/// Start `backend_prefs` in order, falling back if a backend fails to launch.
pub fn start_with_fallback(prefs: &[Backend], cfg: &ServerConfig) -> Result<(Backend, LlamaServer), String> {
    let mut last = String::from("no backends tried");
    for &b in prefs {
        match LlamaServer::start(b, cfg) {
            Ok(s) => return Ok((b, s)),
            Err(e) => last = format!("{b:?}: {e}"),
        }
    }
    Err(format!("all backends failed; last: {last}"))
}

#[cfg(test)]
mod ctx_tests {
    use super::*;

    #[test]
    fn total_context_multiplies() {
        assert_eq!(total_context(4, 4096), 16384);
        assert_eq!(total_context(8, 2048), 16384);
        assert_eq!(total_context(0, 4096), 4096); // guards against 0
    }

    #[test]
    fn fits_slot_checks_budget() {
        assert!(fits_slot(4096, 1645, 256)); // our real prompt fits 4096
        assert!(!fits_slot(1024, 1645, 256)); // the bug case: 1645 > 1024
    }
}
