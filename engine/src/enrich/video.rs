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
