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
