# Sub-project A — Backend Unification (Ollama → llama.cpp) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.
>
> Read the design first: [2026-06-22-backend-unification-llamacpp-design.md](../specs/2026-06-22-backend-unification-llamacpp-design.md).

**Goal:** Replace Ollama (`ollama-rs`) with llama.cpp `llama-server` as the single enrichment backend, with runtime backend selection (CUDA → Vulkan → CPU) and graceful CPU fallback, preserving classification quality and the public `enrich` API.

**Architecture:** A pure `backend` selector (probe behind a trait, unit-tested) picks an accelerator; `server` spawns/owns the `llama-server` child process (per-slot-context-aware); `client` does OpenAI-compatible `/v1/chat/completions` with `response_format` JSON-schema + `enable_thinking:false`. `llm.rs`/`lifecycle.rs` swap their `ollama-rs` calls for these; prompts, schemas, parsing, the orchestrator, and the public API are preserved.

**Tech Stack:** Rust 2021; `reqwest` (blocking+json, already a dep), `serde`/`serde_json`, `schemars` (already used for schemas); `std::process` for the server child. Drops `ollama-rs`. `llama-server` binaries pinned under `tools/`. Cargo: `& "$env:USERPROFILE\.cargo\bin\cargo.exe"` (not on PATH). Baseline: 73 tests pass.

---

### Task 1: Backend selector (`backend.rs`) — the tested fallback

**Files:** Create `engine/src/enrich/backend.rs`; add `mod backend;` to `engine/src/enrich/mod.rs`.

- [ ] **Step 1: Write the failing tests + implementation**

```rust
//! Runtime backend selection for llama.cpp: prefer CUDA, then Vulkan, then CPU.
//! The probe is a trait so tests can simulate accelerators being unavailable.

/// A llama.cpp compute backend, in descending preference order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Cuda,
    Vulkan,
    Cpu,
}

impl Backend {
    /// Subdirectory under the tools dir holding this backend's `llama-server`.
    pub fn dir_name(self) -> &'static str {
        match self {
            Backend::Cuda => "cuda",
            Backend::Vulkan => "vulkan",
            Backend::Cpu => "cpu",
        }
    }
}

/// Detects which accelerators are usable on this machine. Real impl probes the
/// system; tests inject a mock.
pub trait AcceleratorProbe {
    fn cuda_available(&self) -> bool;
    fn vulkan_available(&self) -> bool;
}

/// Pick the first backend in `prefs` whose accelerator is available; CPU is the
/// always-available floor and is returned if nothing else qualifies.
pub fn select_backend(probe: &dyn AcceleratorProbe, prefs: &[Backend]) -> Backend {
    for &b in prefs {
        let ok = match b {
            Backend::Cuda => probe.cuda_available(),
            Backend::Vulkan => probe.vulkan_available(),
            Backend::Cpu => true,
        };
        if ok {
            return b;
        }
    }
    Backend::Cpu
}

/// Default preference order.
pub fn default_prefs() -> Vec<Backend> {
    vec![Backend::Cuda, Backend::Vulkan, Backend::Cpu]
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Mock {
        cuda: bool,
        vulkan: bool,
    }
    impl AcceleratorProbe for Mock {
        fn cuda_available(&self) -> bool { self.cuda }
        fn vulkan_available(&self) -> bool { self.vulkan }
    }

    #[test]
    fn prefers_cuda_when_available() {
        let p = Mock { cuda: true, vulkan: true };
        assert_eq!(select_backend(&p, &default_prefs()), Backend::Cuda);
    }

    #[test]
    fn falls_back_to_vulkan_when_no_cuda() {
        let p = Mock { cuda: false, vulkan: true };
        assert_eq!(select_backend(&p, &default_prefs()), Backend::Vulkan);
    }

    #[test]
    fn falls_back_to_cpu_when_no_accelerator() {
        // The required case: simulate CUDA and Vulkan unavailable.
        let p = Mock { cuda: false, vulkan: false };
        assert_eq!(select_backend(&p, &default_prefs()), Backend::Cpu);
    }

    #[test]
    fn respects_custom_prefs_order() {
        let p = Mock { cuda: true, vulkan: true };
        assert_eq!(select_backend(&p, &[Backend::Vulkan, Backend::Cuda]), Backend::Vulkan);
        assert_eq!(select_backend(&p, &[Backend::Cpu]), Backend::Cpu);
    }
}
```

- [ ] **Step 2:** `& "$env:USERPROFILE\.cargo\bin\cargo.exe" test backend` → 4 tests pass.
- [ ] **Step 3: Commit** — `git add engine/src/enrich/backend.rs engine/src/enrich/mod.rs && git commit -m "feat(backend): accelerator selection with CPU fallback (tested)"`

---

### Task 2: Per-slot context math (`server.rs` part 1)

Prevents the `-c 8192 --parallel 8` → 1024-tok-per-slot 400 error. `llama-server` splits `-c` across slots, so total `-c` must be `parallel × per_slot`.

**Files:** Create `engine/src/enrich/server.rs`; add `mod server;` to `engine/src/enrich/mod.rs`.

- [ ] **Step 1: Write the failing test + impl**

```rust
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
```

- [ ] **Step 2:** `& "$env:USERPROFILE\.cargo\bin\cargo.exe" test ctx_tests` → 2 pass.
- [ ] **Step 3: Commit** — `git add engine/src/enrich/server.rs engine/src/enrich/mod.rs && git commit -m "feat(server): per-slot context sizing"`

---

### Task 3: llama-server process lifecycle (`server.rs` part 2) — IO spike

Spike: implement, `cargo build` to compile-check, verify by running a real `llama-server` in Task 8/10. Resolve exact std::process details against build errors.

**Files:** Modify `engine/src/enrich/server.rs`.

- [ ] **Step 1: Implement `LlamaServer`**

```rust
use crate::enrich::backend::Backend;
use std::path::{Path, PathBuf};
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
```

- [ ] **Step 2: Compile** — `& "$env:USERPROFILE\.cargo\bin\cargo.exe" build`. Resolve any `reqwest::blocking` / process API issues until clean. (Runtime verification happens in Task 10.)
- [ ] **Step 3: Commit** — `git add engine/src/enrich/server.rs && git commit -m "feat(server): spawn/health/shutdown llama-server with backend fallback"`

---

### Task 4: OpenAI-compatible client (`client.rs`)

Replaces `ollama-rs`. Request-building is unit-tested (pure JSON assertions); the POST is exercised in Task 10.

**Files:** Create `engine/src/enrich/client.rs`; add `mod client;` to `engine/src/enrich/mod.rs`.

- [ ] **Step 1: Implement + test the request body builder**

```rust
//! OpenAI-compatible client for llama-server (/v1/chat/completions).

use serde_json::{json, Value};

/// Build the chat-completions request body for a JSON-schema-constrained reply
/// with the model's reasoning phase disabled.
pub fn build_json_body(system: &str, user: &str, schema: Value, max_tokens: u32) -> Value {
    json!({
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user}
        ],
        "temperature": 0.1,
        "max_tokens": max_tokens,
        "response_format": {"type": "json_schema", "json_schema": {"name": "out", "schema": schema}},
        "chat_template_kwargs": {"enable_thinking": false}
    })
}

/// POST a chat request and return the assistant message `content`.
pub fn chat(endpoint: &str, body: &Value) -> Result<String, String> {
    let http = reqwest::blocking::Client::new();
    let resp = http
        .post(format!("{endpoint}/v1/chat/completions"))
        .json(body)
        .timeout(std::time::Duration::from_secs(240))
        .send()
        .map_err(|e| format!("request: {e}"))?;
    if !resp.status().is_success() {
        let code = resp.status();
        let detail = resp.text().unwrap_or_default();
        return Err(format!("llama-server {code}: {detail}"));
    }
    let v: Value = resp.json().map_err(|e| format!("decode: {e}"))?;
    v["choices"][0]["message"]["content"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "no message content".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_has_schema_and_thinking_off() {
        let schema = json!({"type":"object"});
        let b = build_json_body("sys", "usr", schema, 256);
        assert_eq!(b["response_format"]["type"], "json_schema");
        assert_eq!(b["chat_template_kwargs"]["enable_thinking"], false);
        assert_eq!(b["messages"][0]["role"], "system");
        assert_eq!(b["messages"][1]["content"], "usr");
        assert_eq!(b["max_tokens"], 256);
    }
}
```

- [ ] **Step 2:** `& "$env:USERPROFILE\.cargo\bin\cargo.exe" test client` → 1 pass.
- [ ] **Step 3: Commit** — `git add engine/src/enrich/client.rs engine/src/enrich/mod.rs && git commit -m "feat(client): OpenAI-compatible llama-server client"`

---

### Task 5: Migrate `llm.rs` off ollama-rs

Keep prompts (`system_prompt_dir/file/report`), `DirSummary`, `FinalReport`, `DirSummarySchema`/`ReportSchema`, `parse_summary`, `parse_risk`. Replace the request path.

**Files:** Modify `engine/src/enrich/llm.rs`.

- [ ] **Step 1: Replace the client internals**
  - Delete the `ollama-rs` imports (lines ~14–18), `ollama_from_endpoint`, `classify_request`, `block_generate`, `keep_10m`, and (if now unused) `tk_rt`/`ModelOptions`. Keep `schemars::JsonSchema`, `serde`.
  - Add a schema helper using the existing schemars structs:
    ```rust
    fn dir_schema() -> serde_json::Value {
        serde_json::to_value(schemars::schema_for!(DirSummarySchema)).unwrap()
    }
    fn report_schema() -> serde_json::Value {
        serde_json::to_value(schemars::schema_for!(ReportSchema)).unwrap()
    }
    ```
  - Change the three `summarize_*` functions to build the prompt as today, then call the client. Keep their **signatures identical** (`endpoint: &str, model: &str, ...`); `model` is now unused per-call (the server is model-bound) — keep the param for API stability, prefix `_model` if the compiler warns. Example for `summarize_dir`:
    ```rust
    let user = format!("Directory: {dir_path}{ancestor_line}{size_line}{content_line}\nSample contents:\n{samples_str}");
    let system = system_prompt_dir();
    let body = crate::enrich::client::build_json_body(&system, &user, dir_schema(), 300);
    let raw = crate::enrich::client::chat(endpoint, &body)?;
    parse_summary(&raw)
    ```
  - `summarize_file` likewise with `system_prompt_file()`. `summarize_report` builds its report user message + `report_schema()` and parses into `FinalReport` (reuse existing parsing; it can parse from the returned content string).
- [ ] **Step 2:** `& "$env:USERPROFILE\.cargo\bin\cargo.exe" build` — fix references. `cargo test llm` (prompt/parse tests still pass).
- [ ] **Step 3: Commit** — `git add engine/src/enrich/llm.rs && git commit -m "refactor(llm): build requests for llama-server client (drop ollama-rs calls)"`

---

### Task 6: Migrate `lifecycle.rs`

**Files:** Modify `engine/src/enrich/lifecycle.rs`.

- [ ] **Step 1: Reachability + warm-up via the server/client**
  ```rust
  //! Backend reachability + warm-up.
  use serde_json::json;

  /// True if a llama-server is reachable at `endpoint`.
  pub fn health_check(endpoint: &str) -> bool {
      reqwest::blocking::Client::new()
          .get(format!("{endpoint}/health"))
          .timeout(std::time::Duration::from_secs(2))
          .send()
          .map(|r| r.status().is_success())
          .unwrap_or(false)
  }

  /// Warm the model with a tiny request so the first real call isn't cold.
  pub fn preload_model(endpoint: &str, _model: &str) -> Result<(), String> {
      let body = json!({"messages":[{"role":"user","content":"ok"}],"max_tokens":1,
                        "chat_template_kwargs":{"enable_thinking":false}});
      crate::enrich::client::chat(endpoint, &body).map(|_| ())
  }
  ```
  Remove the `ollama_rs` / `super::llm::{...}` imports.
- [ ] **Step 2:** `& "$env:USERPROFILE\.cargo\bin\cargo.exe" build`.
- [ ] **Step 3: Commit** — `git add engine/src/enrich/lifecycle.rs && git commit -m "refactor(lifecycle): health/warm-up via llama-server"`

---

### Task 7: Config + wiring (`mod.rs`, `main.rs`)

**Files:** Modify `engine/src/enrich/mod.rs`, `engine/src/main.rs`, `engine/src/consts.rs`.

- [ ] **Step 1: Replace `LlmConfig` endpoint fields** with the new backend config:
  ```rust
  pub struct LlmConfig {
      pub model_path: std::path::PathBuf,
      pub tools_dir: std::path::PathBuf,
      pub backend_prefs: Vec<crate::enrich::backend::Backend>,
      pub parallel: usize,
      pub per_slot_ctx: usize,
      pub ngl: u32,
      pub port: u16,
      pub sample_count: usize,
  }
  ```
  `Default`: `parallel: 4, per_slot_ctx: 4096, ngl: 999, port: 8080, backend_prefs: backend::default_prefs(), tools_dir: "tools/llamacpp".into(), model_path: <config default>`.
- [ ] **Step 2: In `enrich_items`**, replace the Ollama endpoint usage: build a `ServerConfig` from `LlmConfig`, call `server::start_with_fallback(&config.backend_prefs, &server_cfg)`; on `Err`, `info!` a setup hint and return (graceful skip). Use the returned `endpoint` for all `summarize_*`/health calls. The `LlamaServer` is dropped (shut down) at function end. Keep the orchestrator/cwnd loop; cap cwnd ≤ `parallel`.
- [ ] **Step 3: Update re-exports** in `mod.rs`: `pub use lifecycle::{health_check as is_backend_ready, preload_model};` keep `pub use lifecycle::health_check as is_ollama_running;` as a deprecated alias if `main.rs` still references it (else update `main.rs`). Keep the rest.
- [ ] **Step 4: `main.rs` flags** — replace `--llm-endpoint`/`--llm-model` etc. with `--llm-model-path <PATH>`, `--backend <cuda|vulkan|cpu>` (repeatable → `backend_prefs`), `--llm-parallel`, `--tools-dir`. Build `LlmConfig` from them. Update `is_ollama_running` call site to `is_backend_ready` (it now health-checks after the server is up — or move the check inside `enrich_items`).
- [ ] **Step 5:** `& "$env:USERPROFILE\.cargo\bin\cargo.exe" build` + `cargo run -- --help` (flags present). `cargo test`.
- [ ] **Step 6: Commit** — `git add engine/src/enrich/mod.rs engine/src/main.rs engine/src/consts.rs && git commit -m "feat: wire llama-server backend config + lifecycle into enrich pipeline"`

---

### Task 8: Drop ollama-rs

**Files:** Modify `engine/Cargo.toml`.

- [ ] **Step 1:** Remove the `ollama-rs = "0.3"` line.
- [ ] **Step 2:** `& "$env:USERPROFILE\.cargo\bin\cargo.exe" build` (must compile with no ollama-rs) and `cargo test` (all green) and `cargo clippy --quiet`.
- [ ] **Step 3: Commit** — `git add engine/Cargo.toml Cargo.lock && git commit -m "chore: drop ollama-rs dependency"`

---

### Task 9: Pinned tools (`scripts/setup_tools.ps1`)

**Files:** Create `scripts/setup_tools.ps1`; modify root `.gitignore`.

- [ ] **Step 1: Add `/tools` to `.gitignore`.**
- [ ] **Step 2: Write `scripts/setup_tools.ps1`** that downloads the pinned llama.cpp release (build `b9754`) into `tools/llamacpp/<backend>/`:
  ```powershell
  param([string]$Tag = "b9754", [string[]]$Backends = @("cpu"))
  $root = Join-Path $PSScriptRoot "..\tools\llamacpp"
  New-Item -ItemType Directory -Force $root | Out-Null
  $map = @{ cpu="llama-$Tag-bin-win-cpu-x64.zip"; vulkan="llama-$Tag-bin-win-vulkan-x64.zip"; cuda="llama-$Tag-bin-win-cuda-12.4-x64.zip" }
  foreach ($b in $Backends) {
    $zip = Join-Path $env:TEMP $map[$b]
    gh release download $Tag -R ggml-org/llama.cpp -p $map[$b] -D $env:TEMP --clobber
    $dst = Join-Path $root $b
    New-Item -ItemType Directory -Force $dst | Out-Null
    Expand-Archive $zip -DestinationPath $dst -Force
    if ($b -eq "cuda") { gh release download $Tag -R ggml-org/llama.cpp -p "cudart-llama-bin-win-cuda-12.4-x64.zip" -D $env:TEMP --clobber; Expand-Archive (Join-Path $env:TEMP "cudart-llama-bin-win-cuda-12.4-x64.zip") -DestinationPath $dst -Force }
    Write-Host "installed $b -> $dst"
  }
  ```
  (`llama-server.exe` lands in `tools/llamacpp/<backend>/`; matches `ServerConfig.tools_dir`.)
- [ ] **Step 3: Verify** — run `scripts/setup_tools.ps1` (downloads cpu build); `Test-Path tools/llamacpp/cpu/llama-server.exe` → True.
- [ ] **Step 4: Commit** — `git add scripts/setup_tools.ps1 .gitignore && git commit -m "build: pinned llama.cpp tool fetch (setup_tools.ps1)"`

---

### Task 10: End-to-end re-validation

- [ ] **Step 1: Run enrichment via llama-server** (no Ollama). With a saved snapshot:
  `& "$env:USERPROFILE\.cargo\bin\cargo.exe" run --release -- --from-snapshot scan.snapshot.json --llm --llm-model-path C:\Users\dongm\Downloads\Qwen3.5-0.8B-UD-Q4_K_XL.gguf --backend cpu --top 40`
  Expected: it spawns `llama-server` (cpu), classifies, prints results, shuts the server down.
- [ ] **Step 2: Quality spot-check** — confirm known cases still correct: `Program Files\dotnet` → System, `Pantum\OCR\Bin` → Caution, `npm-cache` → Safe, `Windows\SysWOW64` → System (catalog), no false-Safe on installed toolchains. Matches the shipped Q4 quality.
- [ ] **Step 3: Fallback check** — with no `tools/llamacpp/cuda` present, `--backend cuda --backend cpu` must fall back to cpu and still run (graceful). With the backend missing entirely, enrichment is skipped and rule/heuristic output still prints.
- [ ] **Step 4: Commit** any fixes; final `cargo test` + `cargo clippy --quiet` green.

---

## Self-Review

**Spec coverage:** C-A1 backend selector → Task 1 (with the required CPU-fallback test). C-A2 server lifecycle → Tasks 2–3 (incl. per-slot-ctx rule). C-A3 OpenAI client → Task 4. C-A4 tools mgmt → Task 9. C-A5 config → Task 7. Migration/preserve → Tasks 5–8. Error handling/fallback → Task 3 (`start_with_fallback`) + Task 7 (graceful skip). Testing → Tasks 1,2,4 (unit) + Task 10 (integration). Success criteria 1–4 → Task 10. ✓

**Placeholder scan:** Pure tasks (1,2,4) have complete code+tests. IO tasks (3) and migration (5–7) give exact signatures + the precise request/launch params (response_format, enable_thinking, `-c`/`--parallel`/`-ngl`, `/health`) verified by Task 10 — a deliberate spike, not vague. No TBD/TODO.

**Type consistency:** `Backend`/`AcceleratorProbe`/`select_backend`/`default_prefs` (Task 1) used by `server.rs` (Task 3) and `LlmConfig` (Task 7). `total_context`/`fits_slot` (Task 2) used in `LlamaServer::start` (Task 3). `build_json_body`/`chat` (Task 4) used in `llm.rs` (Task 5) and `lifecycle.rs` (Task 6). `ServerConfig`/`LlamaServer`/`start_with_fallback` (Task 3) used in `enrich_items` (Task 7). `summarize_*` signatures preserved (Task 5). `DirSummary`/`FinalReport`/schemas preserved.

**Known notes:** `model` param kept in `summarize_*` for API stability though the server is model-bound (prefix `_` if unused). `mmproj` field added to `ServerConfig` now (unused until sub-project B) — small forward seam, not dead weight.
