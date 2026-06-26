# M3 Video Description Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a standalone `--describe-video <PATH>` capability that extracts frames from a video with ffmpeg, sends a montage to a SmolVLM2 vision model via llama-server, and prints a structured content guess.

**Architecture:** A new `enrich/frames.rs` (ffmpeg seam + pure math) and `enrich/video.rs` (orchestration + the `VideoContentGuess` contract). `client.rs` gains an image-capable request builder. `main.rs` gets a `--size-audit`-style short-circuit. Reuses sub-project A's `server.rs` (a second llama-server with `mmproj: Some`), `backend.rs` (CUDA→Vulkan→CPU fallback), and `client.rs` (`chat`, `local_client`).

**Tech Stack:** Rust, llama.cpp `llama-server`, SmolVLM2-500M GGUF + mmproj, ffmpeg/ffprobe, `schemars` (JSON schema), `serde`/`serde_json`, `reqwest` (blocking), `base64`.

## Global Constraints

- Spec: `docs/superpowers/specs/2026-06-26-m3-video-description-design.md`.
- VLM CLI flags keep the `--vlm-*` prefix (user decision). The action flag is `--describe-video`.
- All loopback HTTP must use `client::local_client()` (`.no_proxy()`) — never `reqwest::blocking::Client::new()`.
- Disable model reasoning on every request: `chat_template_kwargs: {enable_thinking: false}`.
- New code lives inside the `enrich` module; `frames.rs`/`video.rs` use `super::server`, `super::client` internally.
- Run all commands from `c:\Users\dongm\github\disk_organizer`. Tests: `cargo test -p disk_organizer`.
- Frame sampling: `count = clamp(round(fraction × total_frames), min, max)`, defaults `fraction=0.001`, `min=4`, `max=16`.
- Content-only output: no risk verdict. No enrich-pass wiring, no trigger heuristic this cycle.

---

### Task 1: frames.rs pure math (frames_to_sample, montage_grid)

**Files:**
- Create: `engine/src/enrich/frames.rs`
- Modify: `engine/src/enrich/mod.rs:7` (add `mod frames;` after `mod content;`)
- Test: in `engine/src/enrich/frames.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Produces: `frames_to_sample(total: u64, fraction: f64, min: u32, max: u32) -> u32`; `montage_grid(n: u32) -> (u32, u32)`

- [ ] **Step 1: Declare the module**

In `engine/src/enrich/mod.rs`, add after `mod content;`:
```rust
mod frames;
```

- [ ] **Step 2: Write the failing tests**

Create `engine/src/enrich/frames.rs`:
```rust
//! Frame extraction for video description: ffmpeg/ffprobe subprocess calls
//! plus the pure sampling/layout math they depend on.

/// How many frames to sample: `clamp(round(fraction * total), min, max)`.
pub fn frames_to_sample(total: u64, fraction: f64, min: u32, max: u32) -> u32 {
    let raw = (total as f64 * fraction).round() as i64;
    raw.clamp(min as i64, max as i64) as u32
}

/// Grid dimensions (cols, rows) for an n-frame montage: near-square, cols-major.
pub fn montage_grid(n: u32) -> (u32, u32) {
    let cols = ((n as f64).sqrt().ceil() as u32).max(1);
    let rows = (n + cols - 1) / cols;
    (cols, rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_count_floors_at_min() {
        // 1000 * 0.001 = 1 -> clamped up to 4
        assert_eq!(frames_to_sample(1000, 0.001, 4, 16), 4);
    }

    #[test]
    fn sample_count_caps_at_max() {
        // 200_000 * 0.001 = 200 -> clamped down to 16
        assert_eq!(frames_to_sample(200_000, 0.001, 4, 16), 16);
    }

    #[test]
    fn sample_count_scales_in_band() {
        // 8000 * 0.001 = 8 -> in [4,16]
        assert_eq!(frames_to_sample(8000, 0.001, 4, 16), 8);
    }

    #[test]
    fn grid_is_near_square() {
        assert_eq!(montage_grid(4), (2, 2));
        assert_eq!(montage_grid(9), (3, 3));
        assert_eq!(montage_grid(16), (4, 4));
        assert_eq!(montage_grid(5), (3, 2));
        assert_eq!(montage_grid(7), (3, 3));
        assert_eq!(montage_grid(1), (1, 1));
    }
}
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test -p disk_organizer frames::`
Expected: PASS (4 tests). Build is clean (`mod frames;` declared).

- [ ] **Step 4: Commit**

```bash
git add engine/src/enrich/frames.rs engine/src/enrich/mod.rs
git commit -m "feat(video): frame-sampling and montage-grid math"
```

---

### Task 2: frames.rs ffmpeg seam (count_total_frames, build_montage, parse_fps)

**Files:**
- Modify: `engine/src/enrich/frames.rs`
- Test: `engine/src/enrich/frames.rs` (`tests`)

**Interfaces:**
- Consumes: `montage_grid` (Task 1)
- Produces: `count_total_frames(ffprobe: &Path, video: &Path) -> Result<u64, String>`; `build_montage(ffmpeg: &Path, video: &Path, total_frames: u64, count: u32, shrink: Option<u32>, out: &Path) -> Result<(), String>`

- [ ] **Step 1: Write the failing tests** (pure `parse_fps` + missing-binary error paths)

Append to `engine/src/enrich/frames.rs`, above `#[cfg(test)]` add the imports and functions, and add tests inside `mod tests`:

At the top of the file (after the module doc comment):
```rust
use serde_json::Value;
use std::path::Path;
use std::process::Command;
```

Before `#[cfg(test)]`:
```rust
/// Parse an ffprobe frame-rate string like "30000/1001" or "25/1" into fps.
fn parse_fps(s: &str) -> f64 {
    match s.split_once('/') {
        Some((n, d)) => {
            let n: f64 = n.parse().unwrap_or(0.0);
            let d: f64 = d.parse().unwrap_or(1.0);
            if d == 0.0 { 0.0 } else { n / d }
        }
        None => s.parse().unwrap_or(0.0),
    }
}

/// Estimate a video's total frame count via ffprobe (duration × avg_frame_rate).
/// Approximate by design — the count only feeds `frames_to_sample`'s clamp.
pub fn count_total_frames(ffprobe: &Path, video: &Path) -> Result<u64, String> {
    let out = Command::new(ffprobe)
        .args([
            "-v", "error",
            "-select_streams", "v:0",
            "-show_entries", "stream=duration,avg_frame_rate",
            "-of", "json",
        ])
        .arg(video)
        .output()
        .map_err(|e| format!("run ffprobe ({}): {e}", ffprobe.display()))?;
    if !out.status.success() {
        return Err(format!(
            "ffprobe failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let v: Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| format!("parse ffprobe json: {e}"))?;
    let stream = &v["streams"][0];
    let duration: f64 = stream["duration"]
        .as_str()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| "ffprobe: no stream duration".to_string())?;
    let fps = parse_fps(stream["avg_frame_rate"].as_str().unwrap_or("0/1"));
    let total = (duration * fps).round() as u64;
    if total == 0 {
        return Err("video has 0 frames".into());
    }
    Ok(total)
}

/// Composite `count` evenly-spaced frames from `video` into one montage PNG at
/// `out`, in a single ffmpeg invocation (`select` → optional `scale` → `tile`).
pub fn build_montage(
    ffmpeg: &Path,
    video: &Path,
    total_frames: u64,
    count: u32,
    shrink: Option<u32>,
    out: &Path,
) -> Result<(), String> {
    let count = count.max(1);
    let (cols, rows) = montage_grid(count);
    let step = (total_frames / count as u64).max(1);
    // Pick every `step`-th decoded frame, then tile into a cols×rows grid.
    let select = format!("select='not(mod(n\\,{step}))'");
    let scale = match shrink {
        // Shrink the longest side to `s`, preserving aspect (per frame, pre-tile).
        Some(s) => format!(
            ",scale='if(gt(iw,ih),{s},-1)':'if(gt(iw,ih),-1,{s})'"
        ),
        None => String::new(),
    };
    let vf = format!("{select}{scale},tile={cols}x{rows}");
    let status = Command::new(ffmpeg)
        .args(["-y", "-i"])
        .arg(video)
        .args(["-vf", &vf, "-frames:v", "1", "-fps_mode", "vfr"])
        .arg(out)
        .status()
        .map_err(|e| format!("run ffmpeg ({}): {e}", ffmpeg.display()))?;
    if !status.success() {
        return Err("ffmpeg montage failed".into());
    }
    if !out.exists() {
        return Err("ffmpeg produced no montage file".into());
    }
    Ok(())
}
```

Add to `mod tests`:
```rust
    #[test]
    fn fps_parses_ratio_and_plain() {
        assert!((parse_fps("30000/1001") - 29.97).abs() < 0.01);
        assert_eq!(parse_fps("25/1"), 25.0);
        assert_eq!(parse_fps("0/0"), 0.0);
        assert_eq!(parse_fps("24"), 24.0);
    }

    #[test]
    fn count_frames_errors_when_ffprobe_missing() {
        let r = count_total_frames(
            std::path::Path::new("definitely_not_ffprobe_xyz"),
            std::path::Path::new("nope.mp4"),
        );
        assert!(r.is_err());
    }

    #[test]
    fn build_montage_errors_when_ffmpeg_missing() {
        let r = build_montage(
            std::path::Path::new("definitely_not_ffmpeg_xyz"),
            std::path::Path::new("nope.mp4"),
            1000,
            4,
            None,
            std::path::Path::new("out.png"),
        );
        assert!(r.is_err());
    }
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test -p disk_organizer frames::`
Expected: PASS (7 tests total). No clippy/dead-code warnings (`parse_fps` is used by `count_total_frames`).

- [ ] **Step 3: Commit**

```bash
git add engine/src/enrich/frames.rs
git commit -m "feat(video): ffprobe frame count + ffmpeg montage extraction"
```

---

### Task 3: base64 dependency + client::build_image_request

**Files:**
- Modify: `engine/Cargo.toml:6-16` (add `base64 = "0.22"`)
- Modify: `engine/src/enrich/client.rs`
- Test: `engine/src/enrich/client.rs` (`tests`)

**Interfaces:**
- Consumes: existing `client::chat`, `client::local_client`, `client::build_json_body`
- Produces: `build_image_request(system: &str, user: &str, image_png: &[u8], schema: Value, max_tokens: u32) -> Value`

- [ ] **Step 1: Add the base64 dependency**

In `engine/Cargo.toml` `[dependencies]`, add (alphabetical, before `chrono`):
```toml
base64 = "0.22"
```

- [ ] **Step 2: Write the failing test**

Add to `engine/src/enrich/client.rs` `mod tests`:
```rust
    #[test]
    fn image_request_has_text_and_image_parts() {
        let schema = json!({"type":"object"});
        let b = build_image_request("sys", "describe", &[1u8, 2, 3], schema, 256);
        assert_eq!(b["messages"][0]["role"], "system");
        let content = &b["messages"][1]["content"];
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "describe");
        assert_eq!(content[1]["type"], "image_url");
        let url = content[1]["image_url"]["url"].as_str().unwrap();
        assert!(url.starts_with("data:image/png;base64,"));
        assert_eq!(b["response_format"]["type"], "json_schema");
        assert_eq!(b["chat_template_kwargs"]["enable_thinking"], false);
        assert_eq!(b["max_tokens"], 256);
    }
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p disk_organizer client:: 2>&1 | head -20`
Expected: FAIL — `build_image_request` not found.

- [ ] **Step 4: Implement build_image_request**

In `engine/src/enrich/client.rs`, after `build_json_body`:
```rust
/// Build a vision chat-completions body: a text instruction plus one PNG image
/// (base64 data-URI), JSON-schema-constrained, reasoning disabled.
pub fn build_image_request(
    system: &str,
    user: &str,
    image_png: &[u8],
    schema: Value,
    max_tokens: u32,
) -> Value {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(image_png);
    let data_uri = format!("data:image/png;base64,{b64}");
    json!({
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": [
                {"type": "text", "text": user},
                {"type": "image_url", "image_url": {"url": data_uri}}
            ]}
        ],
        "temperature": 0.1,
        "max_tokens": max_tokens,
        "response_format": {"type": "json_schema", "json_schema": {"name": "out", "schema": schema}},
        "chat_template_kwargs": {"enable_thinking": false}
    })
}
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p disk_organizer client::`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add engine/Cargo.toml engine/Cargo.lock engine/src/enrich/client.rs
git commit -m "feat(video): image-capable chat request builder (base64 data-URI)"
```

---

### Task 4: video.rs contract + parse_video_guess

**Files:**
- Create: `engine/src/enrich/video.rs`
- Modify: `engine/src/enrich/mod.rs` (add `mod video;` after `mod server;`)
- Test: `engine/src/enrich/video.rs` (`tests`)

**Interfaces:**
- Produces: `VideoContentGuess { summary: String, category: VideoCategory, confidence: Confidence }`; enums `VideoCategory`, `Confidence`; `parse_video_guess(raw: &str) -> Result<VideoContentGuess, String>`; `fn guess_schema() -> Value`

- [ ] **Step 1: Declare the module**

In `engine/src/enrich/mod.rs`, add after `mod server;`:
```rust
mod video;
```

- [ ] **Step 2: Write the contract + failing tests**

Create `engine/src/enrich/video.rs`:
```rust
//! Video description: sample frames, ask a SmolVLM2 vision model what the video
//! most likely contains, return a structured guess. Standalone (not wired into
//! the enrichment pass this cycle).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Coarse video category. Serializes to human-readable strings the model emits.
#[derive(Serialize, Deserialize, JsonSchema, Debug, PartialEq, Clone)]
pub enum VideoCategory {
    #[serde(rename = "Screen recording")]
    ScreenRecording,
    #[serde(rename = "Movie or show")]
    MovieOrShow,
    #[serde(rename = "Personal footage")]
    PersonalFootage,
    #[serde(rename = "Game capture")]
    GameCapture,
    Other,
}

#[derive(Serialize, Deserialize, JsonSchema, Debug, PartialEq, Clone)]
pub enum Confidence {
    Low,
    Medium,
    High,
}

/// A best-effort guess of a video's content. A guess, not confirmation.
#[derive(Serialize, Deserialize, JsonSchema, Debug, PartialEq, Clone)]
pub struct VideoContentGuess {
    /// One sentence describing what the video most likely contains.
    pub summary: String,
    pub category: VideoCategory,
    pub confidence: Confidence,
}

/// JSON Schema for a `VideoContentGuess`, for response_format constraint.
fn guess_schema() -> Value {
    serde_json::to_value(schemars::schema_for!(VideoContentGuess))
        .expect("VideoContentGuess serializes")
}

/// Parse the model reply into a `VideoContentGuess` (direct JSON, then a
/// first-`{`..last-`}` safety net).
fn parse_video_guess(raw: &str) -> Result<VideoContentGuess, String> {
    let trimmed = raw.trim();
    if let Ok(g) = serde_json::from_str::<VideoContentGuess>(trimmed) {
        return Ok(g);
    }
    if let Some(start) = trimmed.find('{') {
        if let Some(end) = trimmed.rfind('}') {
            let slice = &trimmed[start..=end];
            if let Ok(g) = serde_json::from_str::<VideoContentGuess>(slice) {
                return Ok(g);
            }
        }
    }
    Err(format!("VLM did not produce a valid guess.\nRaw output:\n{raw}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_direct_json() {
        let g = parse_video_guess(
            r#"{"summary":"A screen recording of a code editor","category":"Screen recording","confidence":"Medium"}"#,
        )
        .unwrap();
        assert_eq!(g.category, VideoCategory::ScreenRecording);
        assert_eq!(g.confidence, Confidence::Medium);
        assert!(g.summary.contains("code editor"));
    }

    #[test]
    fn parses_json_inside_noise() {
        let raw = "Sure:\n```json\n{\"summary\":\"home video\",\"category\":\"Personal footage\",\"confidence\":\"High\"}\n```";
        let g = parse_video_guess(raw).unwrap();
        assert_eq!(g.category, VideoCategory::PersonalFootage);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_video_guess("not json").is_err());
    }

    #[test]
    fn schema_lists_categories() {
        let s = guess_schema().to_string();
        assert!(s.contains("Screen recording"));
        assert!(s.contains("Game capture"));
    }
}
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test -p disk_organizer video::`
Expected: PASS (4 tests).

- [ ] **Step 4: Commit**

```bash
git add engine/src/enrich/video.rs engine/src/enrich/mod.rs
git commit -m "feat(video): VideoContentGuess contract + parser + schema"
```

---

### Task 5: video.rs orchestration (describe_video, VideoConfig) + re-exports

**Files:**
- Modify: `engine/src/enrich/video.rs`
- Modify: `engine/src/enrich/mod.rs` (re-export the public API)
- Test: manual (needs ffmpeg + model) — covered in Task 8

**Interfaces:**
- Consumes: `frames::{count_total_frames, frames_to_sample, build_montage}` (Tasks 1–2); `client::{build_image_request, chat}` (Task 3); `super::server::{ServerConfig, start_with_fallback}` and `super::backend::Backend` (sub-project A); `guess_schema`, `parse_video_guess` (Task 4)
- Produces: `pub struct VideoConfig {...}`; `pub fn describe_video(path: &Path, cfg: &VideoConfig) -> Result<VideoContentGuess, String>`

- [ ] **Step 1: Add imports, VideoConfig, temp-file guard, and describe_video**

At the top of `engine/src/enrich/video.rs`, extend imports:
```rust
use super::backend::Backend;
use super::frames;
use super::server::{start_with_fallback, ServerConfig};
use std::path::{Path, PathBuf};
```

Add the config struct (after the imports):
```rust
/// Inputs for `describe_video`. VLM-prefixed CLI flags map onto these fields.
pub struct VideoConfig {
    pub model_path: PathBuf,
    pub mmproj_path: PathBuf,
    pub tools_dir: PathBuf,
    pub ffmpeg_dir: PathBuf,
    pub backend_prefs: Vec<Backend>,
    pub port: u16,
    pub ngl: u32,
    pub frame_fraction: f64,
    pub min_frames: u32,
    pub max_frames: u32,
    pub shrink: Option<u32>,
}
```

Add a RAII temp-file guard and the orchestration, before `#[cfg(test)]`:
```rust
/// Deletes its path on drop (best-effort) so a montage never lingers.
struct TempPng(PathBuf);
impl Drop for TempPng {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn tool_exe(dir: &Path, name: &str) -> PathBuf {
    let file = if cfg!(windows) { format!("{name}.exe") } else { name.to_string() };
    dir.join(file)
}

const VISION_CTX: usize = 8192;
const VISION_SYSTEM: &str = "You are a video content classifier. You are shown a \
montage grid of frames sampled evenly from one video file. Guess what the video \
most likely contains. This is a GUESS, not confirmation (推测，非确证). Reply ONLY \
with JSON matching the schema: a one-sentence summary, a category, and confidence.";

/// Sample frames from `path`, ask the SmolVLM2 server what it contains.
pub fn describe_video(path: &Path, cfg: &VideoConfig) -> Result<VideoContentGuess, String> {
    if !path.exists() {
        return Err(format!("video not found: {}", path.display()));
    }
    let ffprobe = tool_exe(&cfg.ffmpeg_dir, "ffprobe");
    let ffmpeg = tool_exe(&cfg.ffmpeg_dir, "ffmpeg");
    if !ffmpeg.exists() || !ffprobe.exists() {
        return Err(format!(
            "ffmpeg/ffprobe not found in {} — run scripts/setup_tools.ps1",
            cfg.ffmpeg_dir.display()
        ));
    }

    // 1. Decide how many frames, build the montage.
    let total = frames::count_total_frames(&ffprobe, path)?;
    let count = frames::frames_to_sample(total, cfg.frame_fraction, cfg.min_frames, cfg.max_frames);
    let montage_path = std::env::temp_dir()
        .join(format!("disk_org_montage_{}.png", std::process::id()));
    let montage = TempPng(montage_path.clone());
    frames::build_montage(&ffmpeg, path, total, count, cfg.shrink, &montage.0)?;
    let png = std::fs::read(&montage.0).map_err(|e| format!("read montage: {e}"))?;

    // 2. Start the vision server (owned; Drop kills it).
    let server_cfg = ServerConfig {
        model_path: cfg.model_path.clone(),
        tools_dir: cfg.tools_dir.clone(),
        port: cfg.port,
        parallel: 1,
        per_slot_ctx: VISION_CTX,
        ngl: cfg.ngl,
        mmproj: Some(cfg.mmproj_path.clone()),
    };
    let (backend, server) = start_with_fallback(&cfg.backend_prefs, &server_cfg)
        .map_err(|e| format!("could not start the video model: {e}"))?;
    log::info!("[VLM] video model up on {backend:?}; {count} frames sampled from {}", path.display());

    // 3. Ask, parse.
    let filename = path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let user = format!(
        "Here is a montage of {count} frames from the video '{filename}'. \
         What does this video most likely contain?"
    );
    let body = super::client::build_image_request(VISION_SYSTEM, &user, &png, guess_schema(), 256);
    let raw = super::client::chat(server.endpoint(), &body)?;
    parse_video_guess(&raw)
}
```

- [ ] **Step 2: Re-export the public API**

In `engine/src/enrich/mod.rs`, after `pub use llm::{DirSummary, FinalReport, parse_risk};`, add:
```rust
pub use video::{describe_video, Confidence, VideoCategory, VideoConfig, VideoContentGuess};
```

- [ ] **Step 3: Build to verify it compiles cleanly**

Run: `cargo build -p disk_organizer 2>&1 | tail -15`
Expected: `Finished` with no warnings (all new items are used or re-exported).

- [ ] **Step 4: Run the test suite (no regressions)**

Run: `cargo test -p disk_organizer 2>&1 | tail -5`
Expected: all tests pass (existing 80 + 15 new).

- [ ] **Step 5: Commit**

```bash
git add engine/src/enrich/video.rs engine/src/enrich/mod.rs
git commit -m "feat(video): describe_video orchestration + VideoConfig"
```

---

### Task 6: main.rs --describe-video flag + short-circuit

**Files:**
- Modify: `engine/src/main.rs:4` (use line), `engine/src/main.rs:14-63` (Args), `engine/src/main.rs:65-80` (short-circuit region)
- Test: `cargo run` smoke (missing tools → clean error) — verified here; full run in Task 8

**Interfaces:**
- Consumes: `enrich::{VideoConfig, describe_video}` (Task 5); existing `parse_backends` helper

- [ ] **Step 1: Extend the use line**

`engine/src/main.rs:4`, change:
```rust
use disk_organizer::enrich::{self, Backend, LlmConfig};
```
to:
```rust
use disk_organizer::enrich::{self, Backend, LlmConfig, VideoConfig};
```

- [ ] **Step 2: Add the CLI flags**

In `struct Args` (after the `llm_samples` field, before `debug`), add:
```rust
    /// Look inside a video and describe what it probably contains, then exit
    #[arg(long)]
    describe_video: Option<PathBuf>,
    /// GGUF vision model llama-server loads for --describe-video
    #[arg(long, default_value = "tools/models/SmolVLM2-500M-Video-Instruct-Q8_0.gguf")]
    vlm_model_path: PathBuf,
    /// Multimodal projector (mmproj) GGUF that pairs with the vision model
    #[arg(long, default_value = "tools/models/mmproj-SmolVLM2-500M-Video-Instruct-Q8_0.gguf")]
    vlm_mmproj_path: PathBuf,
    /// Folder containing ffmpeg and ffprobe
    #[arg(long, default_value = "tools/ffmpeg")]
    ffmpeg_dir: PathBuf,
    /// Port the vision llama-server listens on
    #[arg(long, default_value_t = 8090)]
    vlm_port: u16,
    /// Fraction of a video's frames to look at
    #[arg(long, default_value_t = 0.001)]
    vlm_frame_rate: f64,
    /// Fewest frames to sample
    #[arg(long, default_value_t = 4)]
    vlm_min_frames: u32,
    /// Most frames to sample
    #[arg(long, default_value_t = 16)]
    vlm_max_frames: u32,
    /// Shrink each frame's longest side to N px before analysis (default: off)
    #[arg(long)]
    vlm_downscale: Option<u32>,
```

- [ ] **Step 3: Add the short-circuit**

In `fn main`, immediately after the `size_audit` short-circuit block (after its closing `}` near `engine/src/main.rs:80`), add:
```rust
    // Diagnostic short-circuit: describe a single video, print JSON, exit.
    if let Some(video) = args.describe_video.clone() {
        let cfg = VideoConfig {
            model_path: args.vlm_model_path.clone(),
            mmproj_path: args.vlm_mmproj_path.clone(),
            tools_dir: args.tools_dir.clone(),
            ffmpeg_dir: args.ffmpeg_dir.clone(),
            backend_prefs: parse_backends(&args.backend),
            port: args.vlm_port,
            ngl: args.llm_ngl,
            frame_fraction: args.vlm_frame_rate,
            min_frames: args.vlm_min_frames,
            max_frames: args.vlm_max_frames,
            shrink: args.vlm_downscale,
        };
        match enrich::describe_video(&video, &cfg) {
            Ok(guess) => {
                let json = serde_json::to_string_pretty(&guess)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
                println!("{json}");
            }
            Err(e) => {
                error!("describe-video failed: {e}");
                std::process::exit(1);
            }
        }
        return Ok(());
    }
```

- [ ] **Step 4: Build and smoke-test the error path**

Run: `cargo build -p disk_organizer 2>&1 | tail -5`
Expected: `Finished`, no warnings.

Run: `cargo run -p disk_organizer -- --describe-video does_not_exist.mp4 2>&1 | tail -3`
Expected: logs `describe-video failed: video not found: does_not_exist.mp4`, exit code 1.

- [ ] **Step 5: Commit**

```bash
git add engine/src/main.rs
git commit -m "feat(video): --describe-video CLI flag + short-circuit"
```

---

### Task 7: setup_tools.ps1 — pinned ffmpeg + SmolVLM2 model

**Files:**
- Modify: `scripts/setup_tools.ps1`

**Interfaces:** none (build/tooling)

- [ ] **Step 1: Verify exact asset URLs**

Use WebFetch on the BtbN FFmpeg-Builds releases API and the ggml-org SmolVLM2-500M HF repo to confirm the exact asset filenames before hardcoding (mirrors how sub-project A pinned b9754):
- ffmpeg (win64, gpl, includes ffmpeg.exe + ffprobe.exe), e.g. a pinned tag from `https://github.com/BtbN/FFmpeg-Builds/releases`.
- `https://huggingface.co/ggml-org/SmolVLM2-500M-Video-Instruct-GGUF` → confirm the model GGUF filename and the matching `mmproj-*.gguf`.

Record the chosen pinned ffmpeg release tag in a `$FfmpegVersion` param.

- [ ] **Step 2: Add an ffmpeg installer**

In `scripts/setup_tools.ps1`, add a param `[string]$FfmpegVersion = "<pinned-tag>"` and, after the `Install-Archive` function, an installer that downloads the ffmpeg zip, locates the dir containing `ffmpeg.exe`, and copies `ffmpeg.exe` + `ffprobe.exe` into `tools/ffmpeg/`:
```powershell
function Install-Ffmpeg {
    $dest = Join-Path $ToolsDir "ffmpeg"
    New-Item -ItemType Directory -Path $dest -Force | Out-Null
    $zip = "ffmpeg-$FfmpegVersion-win64-gpl.zip"   # adjust to the verified asset name
    $url = "https://github.com/BtbN/FFmpeg-Builds/releases/download/$FfmpegVersion/$zip"
    $zipPath = Join-Path $env:TEMP $zip
    $tmp = Join-Path $env:TEMP "disk_org_ffmpeg"
    Write-Host "[ffmpeg] Downloading $zip ..." -ForegroundColor Yellow
    Invoke-WebRequest -Uri $url -OutFile $zipPath -ErrorAction Stop
    if (Test-Path $tmp) { Remove-Item -Recurse -Force $tmp }
    Expand-Archive -Path $zipPath -DestinationPath $tmp -Force
    $exe = Get-ChildItem -Path $tmp -Recurse -Filter "ffmpeg.exe" | Select-Object -First 1
    $bin = $exe.Directory.FullName
    Copy-Item -Path (Join-Path $bin "ffmpeg.exe")  -Destination $dest -Force
    Copy-Item -Path (Join-Path $bin "ffprobe.exe") -Destination $dest -Force
    Remove-Item -Force $zipPath; Remove-Item -Recurse -Force $tmp
}
```
Call it after the backend installs (gate behind a `-Video` switch param so the default CPU setup stays lean):
```powershell
if ($Video) { Write-Host "[ffmpeg]" -ForegroundColor Cyan; Install-Ffmpeg }
```
Add `[switch]$Video` to the `param(...)` block.

- [ ] **Step 3: Add SmolVLM2 model download**

In the model section, when `-Video` is set, download the SmolVLM2 GGUF + mmproj (verified filenames) into `$ModelDir` if absent:
```powershell
if ($Video) {
    $hf = "https://huggingface.co/ggml-org/SmolVLM2-500M-Video-Instruct-GGUF/resolve/main"
    foreach ($f in @("SmolVLM2-500M-Video-Instruct-Q8_0.gguf", "mmproj-SmolVLM2-500M-Video-Instruct-Q8_0.gguf")) {
        $d = Join-Path $ModelDir $f
        if (-not (Test-Path $d)) {
            Write-Host "Downloading $f ..." -ForegroundColor Yellow
            Invoke-WebRequest -Uri "$hf/$f" -OutFile $d -ErrorAction Stop
        }
    }
}
```

- [ ] **Step 4: Update the verify/usage footer**

Add an ffmpeg presence check and a `--describe-video` example to the script's final "Done" output:
```powershell
$ff = Join-Path $ToolsDir "ffmpeg\ffmpeg.exe"
if (Test-Path $ff) { Write-Host "  [ok]   $ff" -ForegroundColor Green }
Write-Host "  cargo run -p disk_organizer -- --describe-video C:\path\to\video.mp4 --backend cpu" -ForegroundColor Gray
```

- [ ] **Step 5: Run the video setup**

Run: `./scripts/setup_tools.ps1 -Video`
Expected: `tools/ffmpeg/ffmpeg.exe`, `tools/ffmpeg/ffprobe.exe`, and the two SmolVLM2 GGUFs under `tools/models/`.

Verify:
```bash
ls tools/ffmpeg/ && ls tools/models/ | grep -i smolvlm
tools/ffmpeg/ffmpeg.exe -version | head -1
```

- [ ] **Step 6: Commit**

```bash
git add scripts/setup_tools.ps1
git commit -m "build(tools): fetch pinned ffmpeg + SmolVLM2-500M via -Video switch"
```

---

### Task 8: End-to-end validation

**Files:** none (validation only)

**Interfaces:** exercises the whole `--describe-video` path

- [ ] **Step 1: Pick two real videos**

Choose a screen recording and a different kind (e.g. a downloaded clip or personal footage) already on disk. Note their paths.

- [ ] **Step 2: Run describe-video on each (CPU backend)**

Run:
```bash
cargo run -p disk_organizer --release -- --describe-video "C:\path\to\screen_recording.mp4" --backend cpu
cargo run -p disk_organizer --release -- --describe-video "C:\path\to\other_video.mp4" --backend cpu
```
Expected: each prints a `VideoContentGuess` JSON (`summary`, `category`, `confidence`); server starts and is killed afterward; temp montage removed.

- [ ] **Step 3: Sanity-check the guesses**

Confirm `category` is plausible (screen recording → "Screen recording"; movie clip → "Movie or show", etc.) and `summary` references on-screen content. Note CPU timing from logs. If guesses are mushy, re-run one with `--vlm-downscale 768` and compare.

- [ ] **Step 4: Verify cleanup**

Run: `ls $TEMP/disk_org_montage_*.png 2>/dev/null || echo "no leftover montages"`
Expected: no leftover montage files. No `llama-server.exe` left running (`tasklist | grep llama-server` empty).

- [ ] **Step 5: Final test suite + record outcome**

Run: `cargo test -p disk_organizer 2>&1 | tail -5`
Expected: all green. Validation complete — ready for the finishing-a-development-branch flow.

---

## Self-Review

**Spec coverage:**
- describe_video entry point → Tasks 4–5. ✓
- ffmpeg montage + frame sampling (clamp 0.1%/4/16) → Tasks 1–2. ✓
- montage delivery, no downscale default + knob → Tasks 2 (`shrink`), 6 (`--vlm-downscale`). ✓
- SmolVLM2 default + configurable, mmproj → Task 5 (ServerConfig mmproj), Task 6 (flags), Task 7 (download). ✓
- VideoContentGuess content-only (summary/category/confidence) → Task 4. ✓
- image-capable client (data-URI) → Task 3. ✓
- standalone CLI, no enrich wiring, no trigger → Task 6 short-circuit; trigger absent by design. ✓
- error handling (missing ffmpeg, 0 frames, no backend, bad JSON; RAII cleanup) → Tasks 2, 4, 5. ✓
- pinned ffmpeg in setup_tools.ps1 → Task 7. ✓
- tests (frames_to_sample, montage_grid, parse_video_guess, build_image_request; manual e2e) → Tasks 1–4, 8. ✓

**Placeholder scan:** Task 7 leaves the exact pinned ffmpeg tag / asset name to verify-at-implementation (Step 1) — intentional, mirrors A's b9754 verification; everything else is concrete.

**Type consistency:** `VideoConfig` fields match Task 5 definition and Task 6 construction; `frames::*` signatures match Task 5 calls; `build_image_request` signature matches Task 3 and Task 5 call; re-exports in Task 5 match `main.rs` use in Task 6.
