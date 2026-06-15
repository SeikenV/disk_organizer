use clap::Parser;
use disk_organizer::scan::aggregate::aggregate;
use disk_organizer::classify::cut::cut;
use disk_organizer::enrich::{self, is_ollama_running, LlmConfig};
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
    /// Use a local LLM (Ollama) to classify unknown directories
    #[arg(long)]
    llm: bool,
    /// Override LLM endpoint (default: http://localhost:11434)
    #[arg(long, default_value = "http://localhost:11434")]
    llm_endpoint: String,
    /// Optional iGPU/CPU inference endpoint (e.g. llama-server on port 8080)
    #[arg(long)]
    llm_igpu_endpoint: Option<String>,
    /// Fraction of threads for iGPU backend (0.0-1.0, default: 0.3)
    #[arg(long, default_value_t = 0.3)]
    llm_igpu_weight: f64,
    /// Override LLM model (default: qwen35-q4ud:0.8b)
    #[arg(long, default_value = "qwen35-q4ud:0.8b")]
    llm_model: String,
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
            endpoint: args.llm_endpoint.clone(),
            igpu_endpoint: args.llm_igpu_endpoint.clone(),
            igpu_weight: args.llm_igpu_weight,
            model: args.llm_model.clone(),
            sample_count: args.llm_samples,
        };
        if is_ollama_running(&config.endpoint) {
            match ReportFile::create() {
                Ok(rf) => { enrich_report = Some(rf); }
                Err(e) => warn!("cannot create report file: {e}"),
            }
            let t0 = Instant::now();
            enrich::enrich_items(&config, &mut items, &index, &mut enrich_report);
            timings.push(("llm_enrich", t0.elapsed()));
        } else {
            info!("{}", enrich::setup_hint());
        }
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
