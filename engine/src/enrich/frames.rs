//! Frame extraction for video description: ffmpeg/ffprobe subprocess calls
//! plus the pure sampling/layout math they depend on.

use serde_json::Value;
use std::path::Path;
use std::process::Command;

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

/// Video metadata needed to sample frames.
pub struct VideoMeta {
    pub duration_secs: f64,
    pub total_frames: u64,
}

/// Probe a video's duration and (approximate) total frame count via ffprobe
/// (`duration × avg_frame_rate`). Approximate is fine — `total_frames` only
/// feeds `frames_to_sample`'s clamp, and `duration_secs` drives seek timestamps.
pub fn probe_video(ffprobe: &Path, video: &Path) -> Result<VideoMeta, String> {
    let out = Command::new(ffprobe)
        .args([
            "-v", "error",
            "-select_streams", "v:0",
            // Container (format) duration is reliable across mp4/mkv; some
            // streams omit a per-stream duration, so read format first.
            "-show_entries", "format=duration:stream=avg_frame_rate",
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
    let duration: f64 = v["format"]["duration"]
        .as_str()
        .and_then(|s| s.parse().ok())
        .or_else(|| v["streams"][0]["duration"].as_str().and_then(|s| s.parse().ok()))
        .ok_or_else(|| "ffprobe: no duration".to_string())?;
    let fps = parse_fps(v["streams"][0]["avg_frame_rate"].as_str().unwrap_or("0/1"));
    let total = (duration * fps).round() as u64;
    if total == 0 {
        return Err("video has 0 frames".into());
    }
    Ok(VideoMeta { duration_secs: duration, total_frames: total })
}

/// Removes its directory (recursively) on drop, so extracted frames never linger.
struct DirGuard(std::path::PathBuf);
impl Drop for DirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Composite `count` evenly-spaced frames from `video` into one montage PNG at
/// `out`. Frames are pulled by fast input-seek (`-ss` before `-i`, a keyframe
/// seek that avoids decoding the whole file), each optionally shrunk to `shrink`
/// px on its longest side, then tiled into a near-square grid.
pub fn build_montage(
    ffmpeg: &Path,
    video: &Path,
    duration_secs: f64,
    count: u32,
    shrink: Option<u32>,
    out: &Path,
) -> Result<(), String> {
    let count = count.max(1);
    let (cols, rows) = montage_grid(count);

    // 1. Extract `count` frames at evenly-spaced timestamps into a temp dir.
    let dir = std::env::temp_dir().join(format!("disk_org_frames_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("mk temp frame dir: {e}"))?;
    let guard = DirGuard(dir.clone());
    for i in 0..count {
        // Center each sample in its slice; keeps off the exact start/end.
        let ts = duration_secs * (i as f64 + 0.5) / count as f64;
        let frame = dir.join(format!("f_{:04}.png", i + 1));
        let status = Command::new(ffmpeg)
            .args(["-y", "-ss"])
            .arg(format!("{ts:.3}"))
            .arg("-i")
            .arg(video)
            .args(["-frames:v", "1"])
            .arg(&frame)
            .status()
            .map_err(|e| format!("run ffmpeg ({}): {e}", ffmpeg.display()))?;
        if !status.success() || !frame.exists() {
            return Err(format!("ffmpeg failed to extract frame at {ts:.1}s"));
        }
    }

    // 2. Tile the extracted frames into one montage (optionally shrinking each).
    let scale = match shrink {
        Some(s) => format!("scale='if(gt(iw,ih),{s},-1)':'if(gt(iw,ih),-1,{s})',"),
        None => String::new(),
    };
    let vf = format!("{scale}tile={cols}x{rows}");
    let pattern = dir.join("f_%04d.png");
    let status = Command::new(ffmpeg)
        .args(["-y", "-framerate", "1", "-i"])
        .arg(&pattern)
        .args(["-vf", &vf, "-frames:v", "1", "-update", "1"])
        .arg(out)
        .status()
        .map_err(|e| format!("run ffmpeg ({}): {e}", ffmpeg.display()))?;
    drop(guard);
    if !status.success() || !out.exists() {
        return Err("ffmpeg montage tiling failed".into());
    }
    Ok(())
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

    #[test]
    fn fps_parses_ratio_and_plain() {
        assert!((parse_fps("30000/1001") - 29.97).abs() < 0.01);
        assert_eq!(parse_fps("25/1"), 25.0);
        assert_eq!(parse_fps("0/0"), 0.0);
        assert_eq!(parse_fps("24"), 24.0);
    }

    #[test]
    fn probe_errors_when_ffprobe_missing() {
        let r = probe_video(
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
            120.0,
            4,
            None,
            std::path::Path::new("out.png"),
        );
        assert!(r.is_err());
    }
}
