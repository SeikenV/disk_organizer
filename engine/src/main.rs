use clap::Parser;
use disk_organizer::scan::aggregate::aggregate;
use disk_organizer::classify::cut::cut;
use disk_organizer::enrich::{self, Backend, LlmConfig};
use disk_organizer::scan::index::build_index;
use disk_organizer::model::{RawRecord, Source};
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
