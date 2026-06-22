# Sub-project A — Backend Unification (Ollama → llama.cpp) Design

> 2026-06-22 · Status: design for review · Prerequisite for M3 (sub-project B, vision). Supersedes the Ollama-based enrichment backend.

## Goal

Replace Ollama with **llama.cpp `llama-server`** as the single inference backend for LLM enrichment, with **runtime backend selection (CUDA → Vulkan → CPU)** and graceful fallback. This collapses the project to one backend (no Ollama dependency), gives full model control (custom GGUFs, and the vision model SmolVLM2 in sub-project B), and removes the multi-backend VRAM-coordination risk by making backend choice explicit and testable.

## Why (validated)

- llama.cpp runs our text model (`Qwen3.5-0.8B-UD-Q4_K_XL.gguf`) with **structured JSON output** when thinking is disabled (`chat_template_kwargs:{enable_thinking:false}` — the llama.cpp equivalent of Ollama's `think:false`). Verified: dotnet→system, Pantum→caution, npm-cache→safe, identical quality to the Ollama Q4 we shipped.
- CPU is viable: ~0.6–1.1 s/item warm, ~8.5 min for 300 items (vs ~2 min on GPU). So a CPU floor is acceptable for this deferred, cached stage; an accelerator is a speedup, not a requirement.
- Ollama cannot run SmolVLM2 (sub-project B), so we need llama.cpp regardless — unifying text onto it too avoids two backends.

## Current state

`engine/src/enrich/`: `mod.rs` (`enrich_items` orchestration), `orchestrator.rs` (throughput/cwnd concurrency control), `lifecycle.rs` (model preload/health — currently Ollama), `llm.rs` (ollama-rs client + prompts), `content.rs`. Public API consumed by `main.rs`: `LlmConfig`, `enrich_items`, `is_ollama_running`, `preload_model`, `summarize_report`, `analyze_directory_contents`, `parse_risk`, `setup_hint`, `DirSummary`, `FinalReport`.

## Target architecture

```
engine/src/enrich/
├── mod.rs          # enrich_items orchestration (largely unchanged)
├── orchestrator.rs # cwnd throughput control (unchanged; now drives llama-server)
├── backend.rs      # NEW: accelerator probe + backend selection (CUDA→Vulkan→CPU)
├── server.rs       # NEW: llama-server process lifecycle (spawn/health/shutdown)
├── client.rs       # NEW: OpenAI-compatible HTTP client (replaces ollama-rs)
├── lifecycle.rs    # adapts to server.rs (warm-up = ensure server running)
├── llm.rs          # prompts + request building (ollama-rs calls → client.rs)
└── content.rs      # unchanged
engine/src/consts.rs   # backend/server tuning constants
tools/                 # NEW (gitignored): pinned llama-server builds + (later) ffmpeg
scripts/setup_tools.ps1# NEW: fetch-on-setup for pinned binaries
```

### C-A1. Backend selector (`backend.rs`)
- **Responsibility:** decide which llama.cpp backend to use, preferring CUDA → Vulkan → CPU, and which `llama-server` binary + flags that implies.
- **Interface:**
  ```rust
  pub enum Backend { Cuda, Vulkan, Cpu }
  pub trait AcceleratorProbe { fn cuda_available(&self) -> bool; fn vulkan_available(&self) -> bool; }
  pub fn select_backend(probe: &dyn AcceleratorProbe, prefs: &[Backend]) -> Backend; // first available in prefs, else Cpu
  ```
- **Probe (real impl):** CUDA = an NVIDIA GPU + a CUDA-capable `llama-server` build present; Vulkan = a Vulkan-capable build + Vulkan loader/GPU present; CPU = always true (floor).
- **Testability (required):** `select_backend` takes the probe as a trait object, so unit tests inject a mock that reports CUDA/Vulkan **unavailable** and assert the result is `Cpu` (and other combinations). No real GPU needed in tests.
- **Graceful fallback at runtime:** if the selected backend's server fails to start (e.g., CUDA build but driver missing), `server.rs` retries with the next backend down to CPU.
- **Depends on:** the tools dir (which builds are installed).

### C-A2. Server lifecycle (`server.rs`)
- **Responsibility:** own the `llama-server` child process for a given model.
- **Interface:** `LlamaServer::start(model_path, backend, cfg) -> Result<LlamaServer>`, `.endpoint()`, `.shutdown()`. `Drop` kills the child.
- **Config & the per-slot-context rule:** launch with `--jinja -c <ctx> --parallel <n>` and, for GPU backends, `-ngl <layers>`. **Critical:** llama-server splits `-c` across slots, so per-slot context = `ctx / parallel` must be ≥ `max_prompt_tokens + max_output` (~2048). The launcher computes `ctx = parallel × per_slot_ctx` (per_slot_ctx default 4096). (This is the cause of the 400 "exceeds context size" we hit at `-c 8192 --parallel 8` → 1024/slot.)
- **Health:** poll `GET /health` until `{"status":"ok"}` before use; bounded retries.
- **Lifecycle policy:** start before the enrichment run, shut down after (the "unload after use" guidance, [[llm-ollama-operation]]). One server per model; sub-project B may run a second (vision) server.

### C-A3. OpenAI-compatible client (`client.rs`)
- **Responsibility:** the HTTP calls to `llama-server`, replacing `ollama-rs`.
- **Interface:** `chat_json(endpoint, system, user, schema) -> Result<DirSummary>` for classification; a `chat_text(...)` for the free-form report. Internally POSTs `/v1/chat/completions` with:
  - `response_format: {type:"json_schema", json_schema:{name, schema}}` to constrain output to `{category, purpose, risk∈enum}`.
  - `chat_template_kwargs: {enable_thinking:false}` (disables the reasoning phase that otherwise empties `content`).
  - `temperature` + `max_tokens` from config.
  - (For sub-project B) message `content` arrays with `image_url` data-URIs — the client supports image parts now so B needs no client change.
- **Impl:** `reqwest` blocking + `serde` (drop the `ollama-rs` dependency). Tokio runtime reused from existing code.
- **Parsing:** same `parse_summary`/`parse_risk` logic; the schema constraint makes output clean JSON.

### C-A4. Dependency management (`tools/` + `scripts/setup_tools.ps1`)
- **Responsibility:** pinned, project-managed binaries — not stray Downloads/home copies (per the wotagei-toolchain pattern).
- `tools/` is gitignored; `setup_tools.ps1` downloads a **pinned llama.cpp release** (CPU always; CUDA/Vulkan optional) into `tools/llamacpp/<backend>/` and verifies checksums. (Sub-project B adds a pinned ffmpeg here.)
- The engine resolves binary paths via config/env (`DISK_ORG_LLAMACPP_DIR`) with a sensible default of `tools/llamacpp`.
- The GGUF model path is config (default points at a known model file); the model itself isn't committed.

### C-A5. Config (`LlmConfig` extended)
Replace Ollama-endpoint fields with: `model_path`, `backend_prefs: Vec<Backend>` (default `[Cuda, Vulkan, Cpu]`), `parallel`, `per_slot_ctx`, `ngl`, `tools_dir`. Keep `sample_count`. CLI flags mirror these (`--llm-model-path`, `--backend`, …).

## Data flow (unchanged shape)

`collect_work` → orchestrator schedules work across cwnd → each worker calls `client::chat_json` against the running `llama-server` → results applied to Items (`source = LLM`) → final report via `client::chat_text`. The orchestrator's throughput control is preserved; it now drives `llama-server` (cap cwnd ≤ `--parallel`, since excess just queues server-side).

## Error handling

- Backend launch fails → fall back to next backend → CPU.
- No usable backend / model missing / server won't start → **skip enrichment, keep rule+heuristic results** (same graceful degradation as today's "Ollama not running"). `setup_hint` updated to point at `setup_tools.ps1`.
- Per-request failures → existing retry/selective-repeat in the orchestrator.

## Testing

- **Backend selection (the required one):** mock `AcceleratorProbe` → assert CUDA preferred when available; Vulkan when only Vulkan; **CPU when both unavailable**; respects custom `backend_prefs`.
- **Per-slot context math:** `ctx = parallel × per_slot_ctx`; assert a prompt > per-slot ctx is rejected/avoided by config.
- **Client request building:** the JSON body contains `response_format` json_schema, `enable_thinking:false`, correct messages (pure, no network).
- **Parsing:** `parse_summary`/`parse_risk` unchanged tests.
- **Integration (manual/gated):** against a real `llama-server` (CPU build) — one classification returns valid JSON; matches `superpowers:verification-before-completion`.

## Migration / preservation

- **Remove:** `ollama-rs` dependency and Ollama-specific code paths.
- **Preserve unchanged:** all prompts (`llm.rs` system prompts + few-shots), the catalog, cut/classify, the orchestrator throughput algorithm, the public `enrich` API surface (rename `is_ollama_running`→`is_backend_ready` with a deprecated alias if cheap), and `main.rs`'s flow (only flag names change).
- **Re-validate:** text quality on the snapshot matches the shipped Q4 results; throughput acceptable on the selected backend.

## Out of scope (later sub-projects)

- **M3 vision (sub-project B):** SmolVLM2 server, ffmpeg frame extraction, video trigger/content-detection, vision→text two-stage. B reuses A's `server.rs`/`client.rs`/`backend.rs` and adds a pinned ffmpeg to `tools/`.
- **M4 GUI:** external app over the engine contract.
- Bundling/redistributing llama.cpp or GGUFs in git.

## Success criteria

1. Enrichment runs end-to-end via `llama-server` with no Ollama installed.
2. Backend auto-selects CUDA/Vulkan when present, CPU otherwise; unit tests prove the CPU-fallback path with accelerators simulated unavailable.
3. Text classification quality matches the shipped Q4 (spot-check the known cases: dotnet/Pantum/npm-cache/SysWOW64).
4. `cargo test` green; clean graceful-degradation when the backend/model is absent.
