//! Video description: sample frames, ask a SmolVLM2 vision model what the video
//! most likely contains, return a structured guess. Standalone (not wired into
//! the enrichment pass this cycle).

use super::backend::Backend;
use super::frames;
use super::server::{start_with_fallback, LlamaServer, ServerConfig};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Inputs for a `VisionSession`. VLM-prefixed CLI flags map onto these fields.
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

/// File extensions treated as video for `--describe-videos-from` filtering.
const VIDEO_EXTS: &[&str] = &[
    "mp4", "mkv", "mov", "avi", "webm", "m4v", "flv", "wmv", "mpg", "mpeg", "ts", "m2ts",
];

/// True if `path` has a known video extension (case-insensitive).
pub fn is_video_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            let e = e.to_ascii_lowercase();
            VIDEO_EXTS.contains(&e.as_str())
        })
        .unwrap_or(false)
}

/// Unique-per-call montage filename suffix, so concurrent describes don't clash.
static MONTAGE_SEQ: AtomicUsize = AtomicUsize::new(0);

/// A running vision model: owns the `llama-server` lifecycle (start on
/// `start`, load model + mmproj, shut down on `Drop`). Reuse one session to
/// `describe` many videos — describing only writes prompts to this server.
pub struct VisionSession {
    server: LlamaServer,
    cfg: VideoConfig,
}

impl VisionSession {
    /// Start the vision `llama-server` (loads the model + mmproj). The server
    /// stays up until the session is dropped, so callers reuse it across videos.
    pub fn start(cfg: VideoConfig) -> Result<VisionSession, String> {
        let ffmpeg = tool_exe(&cfg.ffmpeg_dir, "ffmpeg");
        let ffprobe = tool_exe(&cfg.ffmpeg_dir, "ffprobe");
        if !ffmpeg.exists() || !ffprobe.exists() {
            return Err(format!(
                "ffmpeg/ffprobe not found in {} — run scripts/setup_tools.ps1",
                cfg.ffmpeg_dir.display()
            ));
        }
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
        log::info!("[VLM] video model up on {backend:?}");
        Ok(VisionSession { server, cfg })
    }

    /// Describe one video using the already-running server (prompting only:
    /// sample frames → montage → ask → parse). Does not touch the server's
    /// lifecycle.
    pub fn describe(&self, path: &Path) -> Result<VideoContentGuess, String> {
        if !path.exists() {
            return Err(format!("video not found: {}", path.display()));
        }
        let ffprobe = tool_exe(&self.cfg.ffmpeg_dir, "ffprobe");
        let ffmpeg = tool_exe(&self.cfg.ffmpeg_dir, "ffmpeg");

        // 1. Decide how many frames, build the montage.
        let meta = frames::probe_video(&ffprobe, path)?;
        let count = frames::frames_to_sample(
            meta.total_frames,
            self.cfg.frame_fraction,
            self.cfg.min_frames,
            self.cfg.max_frames,
        );
        let seq = MONTAGE_SEQ.fetch_add(1, Ordering::Relaxed);
        let montage_path = std::env::temp_dir()
            .join(format!("disk_org_montage_{}_{}.png", std::process::id(), seq));
        let montage = TempPng(montage_path);
        frames::build_montage(&ffmpeg, path, meta.duration_secs, count, self.cfg.shrink, &montage.0)?;
        let png = std::fs::read(&montage.0).map_err(|e| format!("read montage: {e}"))?;
        log::info!("[VLM] {count} frames sampled from {}", path.display());

        // 2. Ask, parse. Deliberately omit the filename: the goal is to describe
        // opaquely-named videos from their frames, and including the name makes a
        // small model echo it instead of describing what it sees.
        let user = format!(
            "Here is a montage of {count} frames sampled evenly from one video. \
             Describe what the video most likely contains, based only on the frames."
        );
        let body =
            super::client::build_image_request(VISION_SYSTEM, &user, &png, guess_schema(), 256);
        let raw = super::client::chat(self.server.endpoint(), &body)?;
        parse_video_guess(&raw)
    }
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

    #[test]
    fn detects_video_paths() {
        assert!(is_video_path(Path::new("C:/x/DSC_1.MP4")));
        assert!(is_video_path(Path::new("/a/b/clip.mkv")));
        assert!(is_video_path(Path::new("movie.webm")));
        assert!(!is_video_path(Path::new("notes.txt")));
        assert!(!is_video_path(Path::new("C:/dir/no_ext")));
        assert!(!is_video_path(Path::new("song.mp3")));
    }
}
