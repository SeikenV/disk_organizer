use clap::Parser;
use disk_organizer::scan::aggregate::aggregate;
use disk_organizer::classify::cut::cut;
use disk_organizer::enrich::{self, Backend, LlmConfig, VideoConfig, VisionSession};
use disk_organizer::scan::index::build_index;
use disk_organizer::model::{Item, RawRecord, Source};
use disk_organizer::report::{self, ReportFile};
use disk_organizer::scan::snapshot;
use flexi_logger::{Duplicate, FileSpec, Logger, WriteMode};
use log::{info, warn, error};
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(name = "disk_organizer", about = "Classify disk usage via NTFS MFT. Outputs JSON.")]
struct Args {
    /// Drive letter to scan, e.g. C (omit when using --from-snapshot)
    drive: Option<String>,
    #[arg(long, default_value_t = 40)]
    top: usize,
    #[arg(long, default_value_t = 200)]
    min_size_mb: u64,
    /// Save the raw scan to a JSON snapshot
    #[arg(long)]
    save_snapshot: Option<PathBuf>,
    /// Analyze a saved snapshot instead of reading the MFT (no admin needed)
    #[arg(long)]
    from_snapshot: Option<PathBuf>,
    /// Use a local LLM (llama-server) to classify unknown directories
    #[arg(long)]
    llm: bool,
    /// Path to the GGUF model llama-server loads
    #[arg(long, default_value = "tools/models/Qwen3.5-0.8B-UD-Q4_K_XL.gguf")]
    llm_model_path: PathBuf,
    /// Directory of per-backend llama-server binaries (<dir>/<backend>/llama-server)
    #[arg(long, default_value = "tools/llamacpp")]
    tools_dir: PathBuf,
    /// Backend preference order (repeatable): cuda, vulkan, cpu.
    /// Omit to use cuda,vulkan,cpu with fallback.
    #[arg(long = "backend")]
    backend: Vec<String>,
    /// Number of llama-server slots (--parallel) and concurrency ceiling
    #[arg(long, default_value_t = 4)]
    llm_parallel: usize,
    /// Context tokens per slot (total -c = parallel × this)
    #[arg(long, default_value_t = 4096)]
    llm_per_slot_ctx: usize,
    /// GPU layers to offload (-ngl); ignored on the CPU backend
    #[arg(long, default_value_t = 999)]
    llm_ngl: u32,
    /// Port llama-server listens on
    #[arg(long, default_value_t = 8080)]
    llm_port: u16,
    /// Number of filenames to sample per unknown directory (default: 20)
    #[arg(long, default_value_t = 20)]
    llm_samples: usize,
    /// Translate results into this language via a second LLM pass (e.g. en, zh, ja).
    /// Omit to keep the model's default output language. NOTE: needs a capable
    /// text model (--llm-model-path); the small default (Qwen3.5-0.8B) tends to
    /// ignore the target language and is not reliable for translation.
    #[arg(long)]
    language: Option<String>,
    /// (Reserved — not yet implemented) Augment analysis with web search.
    #[arg(long)]
    web_search: bool,
    /// Look inside a video and describe what it probably contains, then exit.
    /// Repeatable; all videos share one llama-server.
    #[arg(long)]
    describe_video: Vec<PathBuf>,
    /// Describe every video file found in an enrichment-result JSON (the engine's
    /// item array), reusing one llama-server. Prints a JSON array of guesses.
    #[arg(long)]
    describe_videos_from: Option<PathBuf>,
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
    /// Shrink each frame's longest side to N px before montage (0 = off).
    /// Default 512 keeps the montage under llama-server's payload limit; full
    /// 4K frames otherwise produce a base64 body the server rejects (413).
    #[arg(long, default_value_t = 512)]
    vlm_downscale: u32,
    /// Enable debug mode: verbose logs written to disk (disk_organizer.log)
    #[arg(long)]
    debug: bool,
    /// Diagnostic: audit MFT size sources vs OS used space, then exit
    #[arg(long)]
    size_audit: bool,
}

fn main() -> std::io::Result<()> {
    let args = Args::parse();
    init_logger(args.debug)?;

    // Reserved seam: --web-search is claimed for a future module that fetches
    // vendor/product context to feed the LLM (see docs/ARCHITECTURE.md). It is
    // intentionally a no-op until after the GUI milestone.
    if args.web_search {
        warn!("--web-search is reserved and not yet implemented; ignoring.");
    }

    // Diagnostic short-circuit: measure where on-disk size lives.
    if args.size_audit {
        let drive = args.drive.clone().unwrap_or_else(|| {
            error!("--size-audit needs a drive letter");
            std::process::exit(2);
        });
        let drive = drive.trim_end_matches([':', '\\', '/']).to_ascii_uppercase();
        info!("Reading MFT for {drive}: (size audit, requires Administrator) ...");
        let image = disk_organizer::scan::volume::read_mft(&drive)?;
        disk_organizer::scan::mft::size_audit(image.bytes);
        return Ok(());
    }

    // Diagnostic short-circuit: describe video(s) via ONE vision server, exit.
    if !args.describe_video.is_empty() || args.describe_videos_from.is_some() {
        let videos = collect_videos(&args.describe_video, &args.describe_videos_from)?;
        if videos.is_empty() {
            error!("describe-video: no video files to describe");
            std::process::exit(1);
        }
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
            shrink: if args.vlm_downscale == 0 { None } else { Some(args.vlm_downscale) },
        };
        // Lifecycle lives here: start the server once, describe many, drop.
        let session = match VisionSession::start(cfg) {
            Ok(s) => s,
            Err(e) => {
                error!("describe-video failed: {e}");
                std::process::exit(1);
            }
        };

        // One explicit video and no batch source → print just the guess object.
        let single = args.describe_videos_from.is_none() && videos.len() == 1;
        if single {
            match session.describe(&videos[0]) {
                Ok(guess) => println!(
                    "{}",
                    serde_json::to_string_pretty(&guess)
                        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?
                ),
                Err(e) => {
                    error!("describe-video failed: {e}");
                    std::process::exit(1);
                }
            }
            return Ok(());
        }

        // Otherwise print a JSON array of {path, ...guess} (or {path, error}).
        let mut out: Vec<serde_json::Value> = Vec::with_capacity(videos.len());
        for (i, video) in videos.iter().enumerate() {
            let path_str = video.display().to_string();
            info!("[VLM] ({}/{}) {}", i + 1, videos.len(), path_str);
            match session.describe(video) {
                Ok(guess) => {
                    let mut v = serde_json::to_value(&guess)
                        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
                    if let Some(m) = v.as_object_mut() {
                        m.insert("path".into(), serde_json::Value::String(path_str));
                    }
                    out.push(v);
                }
                Err(e) => {
                    warn!("describe-video failed for {path_str}: {e}");
                    out.push(serde_json::json!({"path": path_str, "error": e}));
                }
            }
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&out)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?
        );
        return Ok(());
    }

    let min = args.min_size_mb.saturating_mul(1024 * 1024);
    let program_start = Instant::now();

    // ---- Phase timing ----
    let mut timings: Vec<(&str, Duration)> = Vec::new();

    // 1. Obtain records: from snapshot, or by reading the MFT.
    let t0 = Instant::now();
    let (drive, records): (String, Vec<RawRecord>) = match &args.from_snapshot {
        Some(path) => {
            info!("Loading snapshot {} ...", path.display());
            let snap = snapshot::load(path)?;
            (snap.drive, snap.records)
        }
        None => {
            let drive = args.drive.clone().unwrap_or_else(|| {
                error!("provide a drive letter or --from-snapshot");
                std::process::exit(2);
            });
            info!("Reading MFT for {drive}: (requires Administrator) ...");
            let image = disk_organizer::scan::volume::read_mft(&drive)?;
            (drive, disk_organizer::scan::mft::parse_records(image.bytes))
        }
    };
    let drive = drive.trim_end_matches([':', '\\', '/']).to_ascii_uppercase();
    info!("{} records.", records.len());
    timings.push(("load_records", t0.elapsed()));

    if let Some(path) = &args.save_snapshot {
        snapshot::save(path, &drive, &records)?;
        info!("Saved snapshot to {}", path.display());
    }

    // 2. Classify.
    let t0 = Instant::now();
    let index = build_index(records);
    let totals = aggregate(&index);
    let used = totals
        .get(&disk_organizer::model::ROOT_FRN)
        .map(|a| (a.physical_size, a.file_count))
        .unwrap_or((0, 0));
    info!(
        "Total accounted (hardlink-deduped): {} across {} files",
        disk_organizer::format::human(used.0),
        used.1
    );
    let mut items = cut(&index, &totals, min);
    // Truncate by LLM-eligible count: items that need LLM analysis
    // (Source::Unknown dirs + Source::Heuristic files) count toward `top`;
    // Catalog/Rule-matched items are included in output but don't consume quota.
    {
        let mut llm_eligible = 0usize;
        let mut take = 0usize;
        for it in &items {
            take += 1;
            let needs_llm = if it.is_dir {
                it.source == Source::Unknown
            } else {
                it.source == Source::Heuristic
            };
            if needs_llm {
                llm_eligible += 1;
            }
            if llm_eligible >= args.top {
                break;
            }
        }
        items.truncate(take);
    }
    timings.push(("classify", t0.elapsed()));

    // 2.1 Make all paths absolute (prepend drive letter).
    let drive_prefix = format!("{drive}:");
    for it in &mut items {
        let rel = it.path.clone();
        it.path = PathBuf::from(format!("{drive_prefix}{}", rel.display()));
    }

    // 2.3 Content analysis: for known directories, inspect actual contents.
    let t0 = Instant::now();
    enrich::analyze_directory_contents(&mut items, &index);
    timings.push(("content_analysis", t0.elapsed()));

    // 2.5 LLM enrichment (if requested and available).
    let mut enrich_report: Option<ReportFile> = None;
    if args.llm {
        let config = LlmConfig {
            model_path: args.llm_model_path.clone(),
            tools_dir: args.tools_dir.clone(),
            backend_prefs: parse_backends(&args.backend),
            parallel: args.llm_parallel,
            per_slot_ctx: args.llm_per_slot_ctx,
            ngl: args.llm_ngl,
            port: args.llm_port,
            sample_count: args.llm_samples,
            language: args.language.clone(),
        };
        // enrich_items owns the llama-server lifecycle: it starts the backend
        // (with CUDA→Vulkan→CPU fallback), enriches, and shuts it down. If no
        // backend can start it logs a hint and leaves rule/heuristic results.
        match ReportFile::create() {
            Ok(rf) => { enrich_report = Some(rf); }
            Err(e) => warn!("cannot create report file: {e}"),
        }
        let t0 = Instant::now();
        enrich::enrich_items(&config, &mut items, &index, &mut enrich_report);
        timings.push(("llm_enrich", t0.elapsed()));
    }

    // 3. Output JSON to stdout.
    let t0 = Instant::now();
    let json = serde_json::to_string_pretty(&items)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    println!("{json}");
    timings.push(("output_json", t0.elapsed()));

    // ---- Timing report ----
    let total = program_start.elapsed();
    info!("");
    info!("{}", "=".repeat(70));
    info!("  BENCHMARK TIMING REPORT");
    info!("{}", "=".repeat(70));
    for (name, dur) in &timings {
        let pct = dur.as_secs_f64() / total.as_secs_f64() * 100.0;
        let bar = "█".repeat((pct / 2.0) as usize);
        info!("  {:>20}: {:>7.1}s ({:>5.1}%) {}",
            name, dur.as_secs_f64(), pct, bar);
    }
    info!("  {:>20}: {:>7.1}s", "TOTAL", total.as_secs_f64());
    info!("{}", "=".repeat(70));

    // Write timing to report file if we have one.
    if let Some(ref mut rep) = enrich_report {
        let _ = rep.section("BENCHMARK TIMING REPORT");
        for (name, dur) in &timings {
            let _ = rep.kv(name, &report::fmt_dur(*dur));
        }
        let _ = rep.kv("TOTAL", &report::fmt_dur(total));
        let _ = rep.flush();
        info!("Full report written to {}", rep.path().display());
    }

    Ok(())
}

/// Gather video paths to describe: the explicit `--describe-video` paths plus
/// every video file found in a `--describe-videos-from` enrichment-result JSON.
/// De-duplicates while preserving order.
fn collect_videos(explicit: &[PathBuf], from: &Option<PathBuf>) -> std::io::Result<Vec<PathBuf>> {
    let mut videos: Vec<PathBuf> = explicit.to_vec();
    if let Some(path) = from {
        // Read leniently: PowerShell `>` / Out-File default to UTF-16 LE (with
        // BOM) on Windows, so a redirected result.json isn't plain UTF-8.
        let text = read_text_lenient(path)?;
        let items: Vec<Item> = serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("parsing {}: {e}", path.display()),
            )
        })?;
        for it in items {
            if !it.is_dir && enrich::is_video_path(&it.path) {
                videos.push(it.path);
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    videos.retain(|p| seen.insert(p.clone()));
    Ok(videos)
}

/// Read a text file, decoding the common Windows encodings produced by shell
/// redirection: UTF-8, UTF-8-with-BOM, and UTF-16 LE/BE (with BOM). PowerShell's
/// `>` and `Out-File` default to UTF-16 LE, which plain `read_to_string` rejects.
fn read_text_lenient(path: &std::path::Path) -> std::io::Result<String> {
    let bytes = std::fs::read(path)?;
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return decode_utf16(rest, true)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e));
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return decode_utf16(rest, false)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e));
    }
    // UTF-8 (BOM, if any, is stripped by the caller before JSON parsing).
    String::from_utf8(bytes).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// Decode raw bytes (BOM already stripped) as UTF-16 in the given endianness.
fn decode_utf16(bytes: &[u8], little_endian: bool) -> Result<String, String> {
    if bytes.len() % 2 != 0 {
        return Err("odd-length UTF-16 stream".into());
    }
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| {
            if little_endian {
                u16::from_le_bytes([c[0], c[1]])
            } else {
                u16::from_be_bytes([c[0], c[1]])
            }
        })
        .collect();
    String::from_utf16(&units).map_err(|e| e.to_string())
}

/// Map repeatable `--backend` strings to a preference list, falling back to the
/// default order (cuda → vulkan → cpu) when none are given or all are invalid.
fn parse_backends(names: &[String]) -> Vec<Backend> {
    let prefs: Vec<Backend> = names
        .iter()
        .filter_map(|n| match n.to_ascii_lowercase().as_str() {
            "cuda" => Some(Backend::Cuda),
            "vulkan" => Some(Backend::Vulkan),
            "cpu" => Some(Backend::Cpu),
            other => {
                warn!("ignoring unknown --backend '{other}' (expected cuda|vulkan|cpu)");
                None
            }
        })
        .collect();
    if prefs.is_empty() {
        enrich::default_prefs()
    } else {
        prefs
    }
}

fn init_logger(debug: bool) -> std::io::Result<()> {
    if debug {
        // Debug mode (script default):
        // - Primary: file with debug level → all detail for post-run analysis
        // - Duplicate: stderr with info level → operator sees same messages as normal mode
        // Log file lands in logs/ with a timestamp in the name.
        let now = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S");
        Logger::try_with_str("debug")
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?
            .log_to_file(
                FileSpec::default()
                    .directory("logs")
                    .basename("disk_organizer")
                    .discriminant(now.to_string()),
            )
            .duplicate_to_stderr(Duplicate::Info)
            .write_mode(WriteMode::BufferAndFlush)
            .start()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    } else {
        // Normal mode: info level to stderr, no file.
        Logger::try_with_env_or_str("info")
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?
            .log_to_stderr()
            .start()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_utf16_little_and_big_endian() {
        // "[]" in UTF-16, no BOM.
        let le = [0x5B, 0x00, 0x5D, 0x00];
        let be = [0x00, 0x5B, 0x00, 0x5D];
        assert_eq!(decode_utf16(&le, true).unwrap(), "[]");
        assert_eq!(decode_utf16(&be, false).unwrap(), "[]");
    }

    #[test]
    fn decode_utf16_rejects_odd_length() {
        assert!(decode_utf16(&[0x5B, 0x00, 0x5D], true).is_err());
    }

    #[test]
    fn read_text_lenient_handles_encodings() {
        let dir = std::env::temp_dir();
        let content = "[\"ok\"]";

        // UTF-8 (no BOM)
        let p8 = dir.join("disk_org_test_utf8.json");
        std::fs::write(&p8, content.as_bytes()).unwrap();
        assert_eq!(read_text_lenient(&p8).unwrap(), content);

        // UTF-16 LE with BOM (what PowerShell `>` writes)
        let p16 = dir.join("disk_org_test_utf16le.json");
        let mut bytes = vec![0xFF, 0xFE];
        for u in content.encode_utf16() {
            bytes.extend_from_slice(&u.to_le_bytes());
        }
        std::fs::write(&p16, &bytes).unwrap();
        assert_eq!(read_text_lenient(&p16).unwrap(), content);

        let _ = std::fs::remove_file(&p8);
        let _ = std::fs::remove_file(&p16);
    }
}
