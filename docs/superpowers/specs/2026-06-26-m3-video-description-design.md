# Sub-project B — M3 Video Description (SmolVLM2) Design

> 2026-06-26 · Status: approved · Builds on Sub-project A (llama.cpp backend unification). Implements the "vision" capability (C6 Vision Orchestrator) at the core-engine level.

## Goal

Add a standalone capability to **look inside a video and describe what it probably contains**, so a user can decide whether a large, opaquely-named video is worth keeping. One entry point — `describe_video(path) -> VideoContentGuess` — that extracts frames with ffmpeg, sends them to a SmolVLM2 vision model via llama-server, and returns a structured content guess.

This cycle delivers the **core engine only**, exposed through a standalone CLI flag (`--describe-video <PATH>`). Wiring it into the deferred enrichment pass (the "large + meaningless-named video" trigger and risk mapping) is **out of scope** and deferred to a later cycle.

## Why (validated)

- SmolVLM2-500M runs via llama.cpp and is **purpose-built for video**: aggressive pixel-shuffle makes each frame cost far fewer tokens (~64–81) than typical VLMs, so multi-frame input is affordable even on CPU / an 8 GB GPU. (SmolVLM2-500M was validated via llama.cpp in a prior cycle.)
- Sub-project A already provides everything needed to run a second model: `server.rs` (a `ServerConfig` with `mmproj: Some(...)` for vision), `backend.rs` (CUDA→Vulkan→CPU fallback), and `client.rs` (OpenAI-compatible chat). B reuses these and adds only frame extraction + a vision orchestration module.
- ffmpeg is the industry-standard frame extractor and can build a montage in a single invocation (`select` → `tile`), so no Rust image-compositing dependency is needed.

## Decisions (locked)

- **Scope:** core engine + standalone `--describe-video <PATH>` CLI. No enrich-pass wiring, no trigger heuristic this cycle.
- **Model:** SmolVLM2-500M is the default; `--vlm-model-path` keeps it configurable. SmolVLM2-2.2B can be dropped in via the flag (documented, no code change).
- **Frame sampling:** adaptive count `clamp(round(fraction × total_frames), min, max)` with defaults `fraction=0.001`, `min=4`, `max=16`.
- **Frame delivery:** **montage** — sampled frames composited into one grid image by ffmpeg's `tile` filter, sent as a single image.
- **Downscale:** off by default (local loopback inference, so a larger base64 body adds little delay). A `--vlm-downscale <px>` knob shrinks each frame's longest side if the model rejects oversized input or guesses are mushy. Note: SmolVLM uses Idefics3-style image splitting and will re-tile a large montage at its own native resolution regardless, so "no downscale" is a first try, not a guarantee of full per-frame detail.
- **Output:** content only — no risk verdict (videos stay user-decided; risk mapping is deferred with the wiring).

## Components

### `enrich/frames.rs` — ffmpeg seam + pure math
- `fn frames_to_sample(total: u64, fraction: f64, min: u32, max: u32) -> u32` — pure: `clamp(round(fraction × total), min, max)`. Unit-tested.
- `fn montage_grid(n: u32) -> (u32, u32)` — pure: `cols = ceil(sqrt(n))`, `rows = ceil(n / cols)`. Unit-tested.
- `fn count_total_frames(ffprobe: &Path, video: &Path) -> Result<u64, String>` — runs ffprobe; prefers `nb_read_frames`/`nb_frames`, falls back to `duration × avg_frame_rate`.
- `fn build_montage(ffmpeg: &Path, video: &Path, count: u32, shrink: Option<u32>, out: &Path) -> Result<(), String>` — single ffmpeg invocation: `select` evenly-spaced `count` frames → optional `scale` → `tile=COLSxROWS` → one PNG at `out`.

### `enrich/video.rs` — orchestration + contract
- `pub struct VideoContentGuess { summary: String, category: VideoCategory, confidence: Confidence }`
- `pub enum VideoCategory { ScreenRecording, MovieOrShow, PersonalFootage, GameCapture, Other }` (serializes to readable strings: "Screen recording", "Movie or show", "Personal footage", "Game capture", "Other").
- `pub enum Confidence { Low, Medium, High }`.
- All three derive `Serialize` + `JsonSchema`; a `VideoContentGuessSchema` drives schemars JSON-schema generation.
- System prompt stresses **推测非确证 / this is a guess, not confirmation**.
- `fn parse_video_guess(raw: &str) -> Result<VideoContentGuess, String>`.
- `pub fn describe_video(path: &Path, cfg: &VideoConfig) -> Result<VideoContentGuess, String>` — counts frames, computes sample count, builds a temp montage (RAII-cleaned), starts the vision server via `start_with_fallback` (owned, Drop-killed), builds the image request, calls `client::chat`, parses.

### `VideoConfig` (in `video.rs`)
`model_path`, `mmproj_path`, `tools_dir`, `ffmpeg_dir`, `backend_prefs`, `port`, `ngl`, `frame_fraction` (0.001), `min_frames` (4), `max_frames` (16), `shrink: Option<u32>` (None).

### `enrich/client.rs` — extended
- `fn build_image_request(system: &str, user: &str, image_png: &[u8], schema: Value, max_tokens: u32) -> Value` — message `content` array with a text part + an `image_url` part (`data:image/png;base64,…`), `response_format` JSON-schema, `chat_template_kwargs:{enable_thinking:false}`. Reuses existing `chat()` and `local_client()` (the no-proxy loopback client).

### `main.rs` — standalone entry
- `--describe-video <PATH>` short-circuit (mirrors the existing `--size-audit` pattern): builds `VideoConfig` from flags, runs `describe_video`, prints the `VideoContentGuess` as JSON to stdout, exits.
- Flags: `--vlm-model-path` (default `tools/models/SmolVLM2-500M-Video-Instruct-Q8_0.gguf`), `--vlm-mmproj-path` (default the matching mmproj GGUF), `--ffmpeg-dir` (default `tools/ffmpeg`), `--vlm-port` (default 8090), `--vlm-frame-rate` (default 0.001), `--vlm-min-frames` (4), `--vlm-max-frames` (16), `--vlm-downscale <px>` (optional, default off). Reuses `--tools-dir`, `--backend`, `--llm-ngl`.

### `scripts/setup_tools.ps1` — tools
- Add a pinned **ffmpeg** download (ffmpeg.exe + ffprobe.exe) into `tools/ffmpeg/`.
- Document fetching the SmolVLM2-500M GGUF + its mmproj into `tools/models/` (model itself not committed).

## Data flow

```
--describe-video video.mp4
  → count_total_frames (ffprobe)
  → frames_to_sample(total, 0.001, 4, 16)
  → montage_grid(n)
  → build_montage (ffmpeg select+tile → temp PNG)
  → base64 → client::build_image_request (schema-constrained, thinking off)
  → SmolVLM2 llama-server /v1/chat/completions
  → parse_video_guess
  → VideoContentGuess as JSON on stdout
  → temp montage + llama-server cleaned up (Drop)
```

## Error handling

`describe_video` returns `Err(String)`; the CLI prints a clear message and exits non-zero (vision is opt-in and standalone — no silent degradation needed here):
- ffmpeg/ffprobe missing → "ffmpeg not found at `<dir>` — run scripts/setup_tools.ps1".
- unreadable video / 0 frames → "could not read any frames from `<path>`".
- no backend starts (reuses A's fallback) → "could not start the video model" + setup hint.
- model returns non-JSON / schema miss → surfaced from `parse_video_guess`.
- Temp montage file and the llama-server are RAII-cleaned (Drop) even on error.

## Testing

- **Pure unit tests** (no ffmpeg/model):
  - `frames_to_sample` — tiny video → 4, huge → 16, mid → fraction; clamp/round boundaries.
  - `montage_grid` — 4→2×2, 9→3×3, 16→4×4, non-square counts.
  - `parse_video_guess` — valid JSON, enum mapping, malformed input.
  - `build_image_request` — shape: text + image parts present, schema present, thinking off.
- **Manual end-to-end validation** (like A's Task 10): run `--describe-video` on a couple of real videos (e.g. a screen recording vs. other footage), confirm category/summary are sane, note CPU timing.

## Out of scope (later cycles)

- Enrich-pass wiring: the "large + meaningless-named video" trigger (`is_candidate_video`), the deferred lowest-priority video queue, and risk/category mapping into the item list.
- Still-image (photo) description.
- M4 GUI.
- Bundling/redistributing ffmpeg or GGUFs in git.
