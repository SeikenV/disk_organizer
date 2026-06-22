mod backend;
mod content;
mod lifecycle;
mod llm;
mod orchestrator;

use crate::scan::index::Index;
use crate::model::{Item, Source};
use crate::report::ReportFile;
use log::{debug, info, warn, error};
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::thread;
use std::time::Instant;

use orchestrator::{run_supervisor, CwndCtl};

pub use content::analyze_directory_contents;
pub use lifecycle::health_check as is_ollama_running;
pub use lifecycle::preload_model;
pub use llm::summarize_report;
pub use llm::{DirSummary, FinalReport, parse_risk};

// ---- Re-export centralized constants (convenience for intra-module use) ----
use crate::consts::{
    cwnd_init,
    MAX_RETRIES, RETRY_BASE_DELAY,
};

// ---- Configuration ----

/// Configuration for LLM enrichment.
///
/// Supports up to two inference backends:
/// - `endpoint` (primary, typically dGPU Ollama)
/// - `igpu_endpoint` (optional secondary, e.g. iGPU llama-server)
///
/// When both are set, `igpu_weight` fraction of worker threads are assigned to
/// the iGPU backend. Each backend gets its own congestion-control state.
pub struct LlmConfig {
    /// dGPU / primary endpoint.
    pub endpoint: String,
    /// Optional iGPU / secondary endpoint.  `None` disables dual-backend mode.
    pub igpu_endpoint: Option<String>,
    /// Fraction of worker threads assigned to the iGPU backend (0.0–1.0).
    /// Default 0.3 = 30 %.
    pub igpu_weight: f64,
    pub model: String,
    /// How many child filenames to sample per directory.
    pub sample_count: usize,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            endpoint: "http://localhost:11434".into(),
            igpu_endpoint: None,
            igpu_weight: 0.3,
            model: "qwen35-q4ud:0.8b".into(),
            sample_count: 20,
        }
    }
}

// ---- Work unit (pre-extracted so threads don't borrow items/index) ----

enum WorkKind {
    Dir {
        samples: Vec<String>,
        /// Nearest meaningful ancestor (project-level context), e.g. "myproject (git repo)".
        ancestor_context: Option<String>,
        /// Physical size of the directory in MB.
        size_mb: u64,
        /// Content summary: file count, ext distribution, subdir stats.
        content_summary: String,
    },
    File {
        ext: String,
        /// Parent directory path (for context).
        parent_dir: String,
        /// Sibling file/dir names in the same parent.
        siblings: Vec<String>,
        /// Nearest meaningful ancestor (project-level context), e.g. "myproject (git repo)".
        ancestor_context: Option<String>,
        /// Physical size of the file in MB.
        size_mb: u64,
    },
}

struct WorkItem {
    /// Index into the original `items` slice.
    idx: usize,
    path: PathBuf,
    kind: WorkKind,
}

// ---- Public API ----

/// Attempt to enrich items with LLM summaries.
///
/// Processes two groups:
/// - `Source::Unknown` directories → analyzed by sampling children.
/// - `Source::Heuristic` files → analyzed by path + parent context.
///
/// Uses TCP-style congestion control with latency feedback.
/// No hard concurrency cap — the window self-tunes based on observed RTT.
///
/// If `report` is provided, the probe log, completion stats, and LLM summary
/// are written to disk in addition to stderr.
pub fn enrich_items(config: &LlmConfig, items: &mut [Item], index: &Index, report: &mut Option<ReportFile>) {
    // ---- Phase: collect work ----
    let phase_collect = Instant::now();
    let work = collect_work(items, index, config.sample_count);
    let collect_elapsed = phase_collect.elapsed();

    if work.is_empty() {
        info!("[LLM] No items to enrich.");
        return;
    }

    let total = work.len();
    let dir_count = work.iter().filter(|w| matches!(w.kind, WorkKind::Dir { .. })).count();
    let file_count = total - dir_count;

    // ---- Decide thread allocation ----
    // Dynamic: 2× logical processors, capped by total work items.
    // Each worker makes blocking HTTP calls to Ollama, so the 2×
    // multiplier gives pipelining headroom above CPU count.
    let worker_cap = crate::consts::worker_thread_limit(total);
    let has_igpu = config.igpu_endpoint.is_some();
    let igpu_threads = if has_igpu {
        ((worker_cap as f64 * config.igpu_weight).ceil() as usize)
            .clamp(1, worker_cap.saturating_sub(1))
    } else {
        0
    };
    let gpu_threads = worker_cap - igpu_threads;

    info!(
        "[LLM] Enriching {} item(s) ({} dirs, {} files) via {} (dGPU={} threads, iGPU={} threads, cwnd_init={}) ...",
        total, dir_count, file_count, config.model, gpu_threads, igpu_threads, cwnd_init(),
    );

    let start_time = Instant::now();

    // ---- Backend-specific state ----
    // ollama-rs manages its own HTTP connection pool internally.
    // We just pass endpoint strings — each llm:: function creates an Ollama
    // instance on the fly (cheap, just wraps a URL).

    let gpu_endpoint = Arc::new(config.endpoint.clone());

    let (igpu_endpoint_arc, igpu_cwnd_ctl): (Option<Arc<String>>, Option<Arc<CwndCtl>>) = if has_igpu {
        let ep = Arc::new(config.igpu_endpoint.clone().unwrap());
        let cc = Arc::new(CwndCtl::new());
        (Some(ep), Some(cc))
    } else {
        (None, None)
    };

    // ---- Preload model into GPU memory (avoids cold-start on first request) ----
    info!("[LLM] Preloading model '{}' into memory ...", config.model);
    if let Err(e) = lifecycle::preload_model(&config.endpoint, &config.model) {
        warn!("[LLM] Model preload failed (non-fatal): {e}");
    } else {
        info!("[LLM] Model preloaded successfully.");
    }

    let gpu_cwnd_ctl = Arc::new(CwndCtl::new());
    let next_idx = Arc::new(AtomicUsize::new(0));
    let done_cnt = Arc::new(AtomicUsize::new(0));
    let model = Arc::new(config.model.clone());

    // ---- Performance counters ----
    let total_attempts_cnt = Arc::new(AtomicUsize::new(0));
    let total_retries_cnt  = Arc::new(AtomicUsize::new(0));
    let min_latency_us     = Arc::new(AtomicUsize::new(u64::MAX as usize));
    let max_latency_us     = Arc::new(AtomicUsize::new(0));
    let latency_sum_us     = Arc::new(AtomicUsize::new(0));

    // ---- Supervisor: sliding-window throughput probe + progress display ----
    let supervisor_gpu = Arc::clone(&gpu_cwnd_ctl);
    let supervisor_igpu = igpu_cwnd_ctl.as_ref().map(Arc::clone);
    let supervisor_done = Arc::clone(&done_cnt);
    let supervisor_total = total;
    let supervisor_handle = thread::spawn(move || {
        run_supervisor(
            &supervisor_gpu,
            supervisor_igpu.as_ref(),
            supervisor_total,
            &supervisor_done,
        );
    });

    // Keep a clone of iGPU controller for final stats (scope consumes the original).
    let igpu_cwnd_for_stats = igpu_cwnd_ctl.as_ref().map(Arc::clone);

    // ---- Worker threads ----
    // GPU and iGPU workers pull from the same `next_idx` counter. Each backend
    // has its own congestion controller. The iGPU cwnd naturally settles lower
    // (higher per-request latency), so neither starves the other.
    let (results, failures): (Vec<(usize, DirSummary)>, Vec<(usize, String)>) = thread::scope(|s| {
        let mut handles = Vec::with_capacity(worker_cap);

        // Shared helper to spawn one worker with a given backend.
        let spawn_workers = |handles: &mut Vec<_>, count: usize, cwnd_ctl: Arc<CwndCtl>,
                             endpoint: Arc<String>| {
            for _ in 0..count {
                let cwnd_ctl = Arc::clone(&cwnd_ctl);
                let endpoint = Arc::clone(&endpoint);
                let model = Arc::clone(&model);
                let next_idx = Arc::clone(&next_idx);
                let done_cnt = Arc::clone(&done_cnt);
                let work = &work;
                let total = total;
                let attempts_cnt = Arc::clone(&total_attempts_cnt);
                let retries_cnt  = Arc::clone(&total_retries_cnt);
                let min_lat = Arc::clone(&min_latency_us);
                let max_lat = Arc::clone(&max_latency_us);
                let sum_lat = Arc::clone(&latency_sum_us);

                let h = s.spawn(move || {
                    let mut local_results: Vec<(usize, DirSummary)> = Vec::new();
                    let mut local_failures: Vec<(usize, String)> = Vec::new();

                    loop {
                        let i = next_idx.fetch_add(1, Ordering::SeqCst);
                        if i >= total {
                            break;
                        }
                        let wi = &work[i];

                        cwnd_ctl.acquire();

                        // ---- Selective Repeat ----
                        let mut last_err = String::new();
                        let mut succeeded = false;
                        for attempt in 0..=MAX_RETRIES {
                            if attempt > 0 {
                                retries_cnt.fetch_add(1, Ordering::Relaxed);
                            }
                            attempts_cnt.fetch_add(1, Ordering::Relaxed);
                            let call_start = Instant::now();
                            match do_summarize(&endpoint, &model, wi) {
                                Ok(summary) => {
                                    let latency_ms = call_start.elapsed().as_secs_f64() * 1000.0;
                                    let latency_us = (latency_ms * 1000.0) as u64;
                                    let lat_us = latency_us as usize;
                                    // Update min latency
                                    loop {
                                        let cur = min_lat.load(Ordering::Relaxed);
                                        if lat_us >= cur { break; }
                                        if min_lat.compare_exchange_weak(cur, lat_us, Ordering::Relaxed, Ordering::Relaxed).is_ok() { break; }
                                    }
                                    // Update max latency
                                    loop {
                                        let cur = max_lat.load(Ordering::Relaxed);
                                        if lat_us <= cur { break; }
                                        if max_lat.compare_exchange_weak(cur, lat_us, Ordering::Relaxed, Ordering::Relaxed).is_ok() { break; }
                                    }
                                    sum_lat.fetch_add(lat_us, Ordering::Relaxed);
                                    local_results.push((wi.idx, summary));
                                    cwnd_ctl.release_success(latency_ms);
                                    succeeded = true;
                                    break;
                                }
                                Err(e) => {
                                    last_err = e;
                                    if attempt < MAX_RETRIES {
                                        let delay =
                                            RETRY_BASE_DELAY * 2u32.pow(attempt);
                                        // Release permit before backoff —
                                        // don't waste a concurrency slot while sleeping.
                                        cwnd_ctl.release_retry();
                                        thread::sleep(delay);
                                        cwnd_ctl.acquire(); // re-obtain permit
                                    }
                                }
                            }
                        }
                        if !succeeded {
                            local_failures.push((wi.idx, last_err));
                            cwnd_ctl.release_failure();
                        }
                        done_cnt.fetch_add(1, Ordering::Relaxed);
                    }

                    (local_results, local_failures)
                });
                handles.push(h);
            }
        };

        // Spawn dGPU group.
        spawn_workers(&mut handles, gpu_threads, Arc::clone(&gpu_cwnd_ctl), Arc::clone(&gpu_endpoint));

        // Spawn iGPU group (if configured).
        if let (Some(ep), Some(cc)) = (igpu_endpoint_arc, igpu_cwnd_ctl) {
            spawn_workers(&mut handles, igpu_threads, Arc::clone(&cc), Arc::clone(&ep));
        }

        // Merge.
        let mut merged_results = Vec::with_capacity(work.len());
        let mut merged_failures = Vec::new();
        for h in handles {
            let (r, f) = h.join().unwrap();
            merged_results.extend(r);
            merged_failures.extend(f);
        }
        (merged_results, merged_failures)
    });

    let _ = supervisor_handle.join();
    let elapsed = start_time.elapsed();

    // Apply results to items.
    for (idx, ref summary) in &results {
        items[*idx].category = summary.category.clone();
        items[*idx].purpose = summary.purpose.clone();
        let llm_risk = llm::parse_risk(summary.risk.as_deref());
        // Only override risk if it was Unknown — don't downgrade catalog risks.
        if items[*idx].risk == crate::model::Risk::Unknown {
            items[*idx].risk = llm_risk;
        }
        items[*idx].source = Source::LLM;
    }

    let ok = results.len();
    let fail = failures.len();
    let avg_ms = if ok > 0 { elapsed.as_millis() as f64 / ok as f64 } else { 0.0 };
    let gpu_final_cw = gpu_cwnd_ctl.cwnd.load(Ordering::Relaxed);
    let gpu_final_srtt = gpu_cwnd_ctl.srtt();
    // Peaks are tracked inside CwndCtl now (via acquire / supervisor).
    let gpu_peak = gpu_cwnd_ctl.peak_inflight.load(Ordering::Relaxed);
    let gpu_peak_cw = gpu_cwnd_ctl.peak_cwnd.load(Ordering::Relaxed);

    if has_igpu {
        if let Some(ref ic) = igpu_cwnd_for_stats {
            let i_final_cw = ic.cwnd.load(Ordering::Relaxed);
            let i_final_srtt = ic.srtt();
            let i_peak = ic.peak_inflight.load(Ordering::Relaxed);
            let i_peak_cw = ic.peak_cwnd.load(Ordering::Relaxed);
            info!(
                "\r[LLM] Enrichment complete. {ok}/{total} succeeded, {fail} failed. \
                 Elapsed: {elapsed:.1?}. Avg/req: {avg_ms:.0} ms.\n\
                 \x20 dGPU: srtt={gpu_final_srtt:.0}ms, peak inflight={gpu_peak}, \
                 peak cwnd={gpu_peak_cw}, final cwnd={gpu_final_cw}\n\
                 \x20 iGPU: srtt={i_final_srtt:.0}ms, peak inflight={i_peak}, \
                 peak cwnd={i_peak_cw}, final cwnd={i_final_cw}",
            );
        }
    } else {
        info!(
            "\r[LLM] Enrichment complete. {ok}/{total} succeeded, {fail} failed. \
             Elapsed: {elapsed:.1?}. \
             Avg/req: {avg_ms:.0} ms. \
             SRTT: {gpu_final_srtt:.0} ms. \
             Peak inflight: {gpu_peak}, peak cwnd: {gpu_peak_cw}, final cwnd: {gpu_final_cw}."
        );
    }

    // Counters.
    let tot_attempts = total_attempts_cnt.load(Ordering::Relaxed);
    let tot_retries  = total_retries_cnt.load(Ordering::Relaxed);
    let sum_us       = latency_sum_us.load(Ordering::Relaxed);
    let min_us       = min_latency_us.load(Ordering::Relaxed);
    let max_us       = max_latency_us.load(Ordering::Relaxed);
    if ok > 0 {
        let avg_us = sum_us / ok;
        info!(
            "\r[LLM] Latency: min={:.1}s  avg={:.1}s  max={:.1}s  \
             requests={}  retries={}  wastage={:.0}%",
            min_us as f64 / 1_000_000.0,
            avg_us as f64 / 1_000_000.0,
            max_us as f64 / 1_000_000.0,
            tot_attempts, tot_retries,
            if tot_attempts > ok { (tot_attempts - ok) as f64 / tot_attempts as f64 * 100.0 } else { 0.0 },
        );
    }
    info!(
        "[LLM] Phase timings: collect_work={:.1}s  llm_api={:.1}s",
        collect_elapsed.as_secs_f64(),
        elapsed.as_secs_f64(),
    );

    // ---- Probe log dump ----
    {
        let log = gpu_cwnd_ctl.probe_log.lock().unwrap();
        if !log.is_empty() {
            // --- file: compact table (debug level → log file only, not terminal) ---
            debug!("");
            debug!("{}", "=".repeat(80));
            debug!("  PROBE LOG (throughput-driven cwnd)");
            debug!("{}", "=".repeat(80));
            debug!(
                "  {:>7} {:>5} {:>5} {:>7} {:>8} {:>7} {:>7} {:>8} {:>5} {:>7}",
                "elapsed", "cwnd", "infl", "done/w", "rps", "srtt", "t/ms", "best_tp", "b_cw", "phase"
            );
            debug!("  {}", "-".repeat(76));
            for r in log.iter() {
                debug!(
                    "  {:>6}ms {:>4} {:>4} {:>6} {:>7.1} {:>6.0} {:>7.0} {:>7.1} {:>5} {:>7}",
                    r.elapsed_ms, r.cwnd, r.inflight, r.completed_in_window,
                    r.throughput_rps, r.srtt_ms, r.per_task_ms,
                    r.best_tp_rps, r.best_cwnd, r.phase,
                );
            }
            debug!("{}", "=".repeat(80));

            // --- Disk: detailed table ---
            if let Some(ref mut rep) = report {
                let _ = rep.section("PROBE LOG (throughput-driven cwnd)");
                let _ = rep.line(&format!(
                    "  {:>7} {:>5} {:>5} {:>7} {:>8} {:>7} {:>7} {:>8} {:>5} {:>7}",
                    "elapsed", "cwnd", "infl", "done/w", "rps", "srtt", "t/ms", "best_tp", "b_cw", "phase"
                ));
                let _ = rep.line(&format!("  {}", "-".repeat(76)));
                for r in log.iter() {
                    let _ = rep.line(&format!(
                        "  {:>6}ms {:>4} {:>4} {:>6} {:>7.1} {:>6.0} {:>7.0} {:>7.1} {:>5} {:>7}",
                        r.elapsed_ms, r.cwnd, r.inflight, r.completed_in_window,
                        r.throughput_rps, r.srtt_ms, r.per_task_ms,
                        r.best_tp_rps, r.best_cwnd, r.phase,
                    ));
                }

                let _ = rep.section("COMPLETION SUMMARY");
                let _ = rep.kv("model", &config.model);
                let _ = rep.kv("succeeded", &format!("{ok}/{total}"));
                let _ = rep.kv("failed", &format!("{fail}"));
                let _ = rep.kv("elapsed", &crate::report::fmt_dur(elapsed));
                let _ = rep.kv("avg_per_request_ms", &format!("{avg_ms:.0}"));
                let _ = rep.kv("peak_inflight", &format!("{gpu_peak}"));
                let _ = rep.kv("peak_cwnd", &format!("{gpu_peak_cw}"));
                let _ = rep.kv("final_cwnd", &format!("{gpu_final_cw}"));
                let _ = rep.kv("final_srtt_ms", &format!("{gpu_final_srtt:.0}"));
                let _ = rep.kv("collect_work", &crate::report::fmt_dur(collect_elapsed));
                let _ = rep.kv("llm_api", &crate::report::fmt_dur(elapsed));
                if ok > 0 {
                    let avg_us = latency_sum_us.load(Ordering::Relaxed) / ok;
                    let _ = rep.kv("latency_min_s", &format!("{:.1}", min_latency_us.load(Ordering::Relaxed) as f64 / 1_000_000.0));
                    let _ = rep.kv("latency_avg_s", &format!("{:.1}", avg_us as f64 / 1_000_000.0));
                    let _ = rep.kv("latency_max_s", &format!("{:.1}", max_latency_us.load(Ordering::Relaxed) as f64 / 1_000_000.0));
                    let _ = rep.kv("total_requests", &format!("{tot_attempts}"));
                    let _ = rep.kv("retries", &format!("{tot_retries}"));
                    let w = if tot_attempts > ok { (tot_attempts - ok) as f64 / tot_attempts as f64 * 100.0 } else { 0.0 };
                    let _ = rep.kv("wastage_pct", &format!("{w:.0}"));
                }
            }

            // --- Disk: JSON probe log for programmatic analysis ---
            if let Ok(json) = serde_json::to_string_pretty(&*log) {
                let _ = std::fs::write("enrichment_probe_log.json", &json);
                info!(
                    "[LLM] Probe log written to enrichment_probe_log.json ({} records).",
                    log.len()
                );
            }
        }
    }
    // (probe log lock dropped above)

    // Print failed items so user can investigate.
    if !failures.is_empty() {
        warn!("[LLM] Failed items:");
        for (idx, err) in &failures {
            let path = &items[*idx].path;
            warn!("  {} {}", path.display(), err);
        }
        if let Some(ref mut rep) = report {
            let _ = rep.section("FAILED ITEMS");
            for (idx, err) in &failures {
                let path = &items[*idx].path;
                let _ = rep.line(&format!("  {} {}", path.display(), err));
            }
        }
    }

    // ---- Final LLM summary report ----
    // Collect top-N per risk group by size (avoid huge prompts that cause 400).
    const REPORT_TOP_PER_GROUP: usize = 10;

    fn top_by_size(items: &[Item], risk: crate::model::Risk, n: usize) -> (Vec<DirSummary>, f64) {
        let mut matching: Vec<&Item> = items.iter().filter(|it| it.risk == risk).collect();
        matching.sort_by_key(|it| std::cmp::Reverse(it.physical_size));
        let mb = matching.iter().map(|it| it.physical_size).sum::<u64>() as f64 / (1024.0 * 1024.0);
        let summaries: Vec<DirSummary> = matching
            .into_iter()
            .take(n)
            .map(|it| DirSummary {
                category: it.category.clone(),
                purpose: it.purpose.clone(),
                risk: None,
            })
            .collect();
        (summaries, mb)
    }

    let (safe_items, safe_mb) = top_by_size(items, crate::model::Risk::Safe, REPORT_TOP_PER_GROUP);
    let (caution_items, caution_mb) = top_by_size(items, crate::model::Risk::Caution, REPORT_TOP_PER_GROUP);
    let (system_items, system_mb) = top_by_size(items, crate::model::Risk::System, REPORT_TOP_PER_GROUP);
    let (unknown_items, unknown_mb) = top_by_size(items, crate::model::Risk::Unknown, REPORT_TOP_PER_GROUP);
    let total_mb = items.iter().map(|it| it.physical_size).sum::<u64>() as f64 / (1024.0 * 1024.0);

    // Collect counts for the report (we need both shown and total).
    let safe_total = items.iter().filter(|it| it.risk == crate::model::Risk::Safe).count();
    let caution_total = items.iter().filter(|it| it.risk == crate::model::Risk::Caution).count();
    let system_total = items.iter().filter(|it| it.risk == crate::model::Risk::System).count();
    let unknown_total = items.iter().filter(|it| it.risk == crate::model::Risk::Unknown).count();

    // Final report uses a FRESH client — not the shared pool from workers.
    // After hundreds of concurrent requests the shared pool may hold half-closed
    // connections.  A new client gets a clean TCP connection.
    // Quick health check first to avoid 30s timeouts if Ollama is momentarily down.
    let mut report_result = Err("unreachable".to_string());
    if !lifecycle::health_check(&config.endpoint) {
        warn!("\n[LLM] Ollama not responding, skipping final report.");
        if let Some(ref mut rep) = report {
            let _ = rep.line("[LLM] Ollama not responding, skipping final report.");
        }
    } else {
        for attempt in 1..=3 {
            if attempt > 1 {
                warn!("[LLM] Retrying final report (attempt {attempt})...");
            } else {
                info!("\n[LLM] Requesting final summary and cleanup plan...");
            }
            report_result = llm::summarize_report(
                &config.endpoint,
                &config.model,
                &safe_items,
                &caution_items,
                &system_items,
                &unknown_items,
                total_mb,
                safe_mb,
                caution_mb,
                system_mb,
                unknown_mb,
                safe_total,
                caution_total,
                system_total,
                unknown_total,
            );
            if let Err(ref e) = report_result {
                warn!("[LLM] Final report attempt {attempt} failed: {e}");
            }
            if report_result.is_ok() {
                break;
            }
        }
    }

    match report_result {
        Ok(llm_report) => {
            // --- stderr ---
            info!("");
            info!("{}", "=".repeat(70));
            info!("  LLM SUMMARY REPORT");
            info!("{}", "=".repeat(70));
            info!("");
            info!("📊 OVERVIEW");
            info!("  {}", llm_report.overview);
            info!("");
            info!("🟢 SAFE ITEMS (recoverable: {:.0} MB)", safe_mb);
            info!("  {}", llm_report.safe_summary);
            info!("");
            info!("🟡 CAUTION ITEMS (potential: {:.0} MB)", caution_mb);
            info!("  {}", llm_report.caution_advice);
            info!("");
            info!("📋 CLEANUP PLAN");
            for (i, step) in llm_report.cleanup_plan.iter().enumerate() {
                info!("  {}. {step}", i + 1);
            }
            info!("");
            info!("{}", "=".repeat(70));

            // --- Disk ---
            if let Some(ref mut rep) = report {
                let _ = rep.section("LLM SUMMARY REPORT");
                let _ = rep.line(&format!("OVERVIEW: {}", llm_report.overview));
                let _ = rep.line(&format!("SAFE ITEMS ({:.0} MB): {}", safe_mb, llm_report.safe_summary));
                let _ = rep.line(&format!("CAUTION ITEMS ({:.0} MB): {}", caution_mb, llm_report.caution_advice));
                let _ = rep.line("CLEANUP PLAN:");
                for (i, step) in llm_report.cleanup_plan.iter().enumerate() {
                    let _ = rep.line(&format!("  {}. {step}", i + 1));
                }
            }
        }
        Err(e) => {
            error!("\n[LLM] Final report generation failed: {e}");
            if let Some(ref mut rep) = report {
                let _ = rep.line(&format!("[LLM] Final report generation failed: {e}"));
            }
        }
    }
}

// ---- Collect work items ----

fn collect_work(items: &[Item], index: &Index, sample_count: usize) -> Vec<WorkItem> {
    let mut work = Vec::new();

    for (i, it) in items.iter().enumerate() {
        if it.is_dir {
            // Only Unknown dirs (catalog-matched dirs are already classified).
            if it.source == Source::Unknown {
                let ancestor_context = find_ancestor_context(it.frn, index);
                let content_summary = content::summarize_children(it.frn, index);
                work.push(WorkItem {
                    idx: i,
                    path: it.path.clone(),
                    kind: WorkKind::Dir {
                        samples: sample_children(it.frn, index, sample_count),
                        ancestor_context,
                        size_mb: it.physical_size / (1024 * 1024),
                        content_summary,
                    },
                });
            }
        } else {
            // Heuristic files get LLM re-analysis for deeper understanding.
            if it.source == Source::Heuristic {
                let (parent_dir, siblings) = parent_context(it.frn, index);
                let ancestor_context = find_ancestor_context(it.frn, index);
                let ext = it
                    .path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_string();
                work.push(WorkItem {
                    idx: i,
                    path: it.path.clone(),
                    kind: WorkKind::File {
                        ext,
                        parent_dir,
                        siblings,
                        ancestor_context,
                        size_mb: it.physical_size / (1024 * 1024),
                    },
                });
            }
        }
    }

    work
}

/// Get the parent directory path and sibling names for a file.
fn parent_context(frn: u64, index: &Index) -> (String, Vec<String>) {
    let rec = match index.by_frn.get(&frn) {
        Some(r) => r,
        None => return (String::new(), vec![]),
    };

    let parent_frn = rec.parent_frn;

    // Try to get parent path.
    let parent_path = get_name(parent_frn, index).unwrap_or_else(|| "?".to_string());

    // Get sibling names (up to 15).
    let siblings: Vec<String> = index
        .children
        .get(&parent_frn)
        .map(|kids| {
            kids.iter()
                .filter(|&&c| c != frn)
                .filter_map(|&c| get_name(c, index))
                .take(15)
                .collect()
        })
        .unwrap_or_default();

    (parent_path, siblings)
}

/// Well-known generic subdirectory names that don't provide project-level
/// context when tracing ancestry. These are common build/config/data dirs.
const GENERIC_SUBDIR_NAMES: &[&str] = &[
    "src", "target", "build", "generated", "dist", "output",
    "node_modules", "tests", "test", "docs", "examples", "assets",
    "static", "lib", "lib64", "bin", "bin64", "include", "obj",
    "Debug", "Release", "x64", "x86", "win64.o", "nt64",
    "artifacts", "temp", "tmp", "cache", "data", "config", "vendor",
    "packages", ".venv", "venv", "__pycache__", ".github", "test_data",
    "fixtures", "samples", "input", "public", "resources", "external",
    "third_party", "submodules", "engine", "source", "modules", "logs",
    "_logs", "results", "run", "out", "outputs",
    // LaTeX / TeX internal directories
    "texmf-dist", "tex", "latex", "fonts", "doc", "generic", "type1",
    "tfm", "truetype", "opentype", "public",
    // Xilinx / FPGA internal directories
    "secureip", "devint", "vault", "versal", "timingdata", "arch", "dst",
    "parts", "xsim", "verilog", "ip", "hls", "impl", "syn", "sim",
    "rtl", "xdc", "bd", "hw", "sw",
];

/// Walk up the parent chain from `frn` looking for the nearest meaningful
/// ancestor directory — one that represents a project/application, not a
/// generic subdirectory like "src/", "build/", etc.
///
/// Also checks whether any ancestor contains a `.git/` subdirectory.
///
/// Returns a context description string, e.g.:
/// - `"disk_organizer (git repository)"`
/// - `"SuperWeb-Cluster"`
fn find_ancestor_context(frn: u64, index: &Index) -> Option<String> {
    let mut current_frn = frn;
    let mut git_repo: Option<String> = None;
    let mut project_name: Option<String> = None;

    // Skip the item itself — go directly to its parent.
    let start = index.by_frn.get(&current_frn)?;
    current_frn = start.parent_frn;

    loop {
        // Stop at the NTFS root (FRN 5) first — test data may not include a root record.
        if current_frn == crate::model::ROOT_FRN || current_frn == 0 {
            break;
        }

        let rec = match index.by_frn.get(&current_frn) {
            Some(r) => r,
            None => break,
        };

        // Check whether this ancestor contains a .git/ subdirectory.
        if git_repo.is_none() {
            if let Some(kids) = index.children.get(&current_frn) {
                for &kid_frn in kids {
                    if let Some(kid) = index.by_frn.get(&kid_frn) {
                        if kid.is_dir && kid.name.eq_ignore_ascii_case(".git") {
                            git_repo = Some(rec.name.clone());
                            break;
                        }
                    }
                }
            }
        }

        // If we haven't found a project-level ancestor yet, check this one.
        if project_name.is_none() {
            let name_lower = rec.name.to_lowercase();
            if !GENERIC_SUBDIR_NAMES.iter().any(|g| g.eq_ignore_ascii_case(&name_lower)) {
                project_name = Some(rec.name.clone());
            }
        }

        current_frn = rec.parent_frn;
    }

    // Build the context description string.
    match (project_name, git_repo) {
        (Some(proj), Some(git)) if proj == git => {
            Some(format!("project '{proj}' (git repository)"))
        }
        (Some(proj), Some(git)) => {
            Some(format!("project '{proj}' (inside git repo '{git}')"))
        }
        (Some(proj), None) => {
            Some(format!("project '{proj}'"))
        }
        (None, Some(git)) => {
            Some(format!("git repository '{git}'"))
        }
        (None, None) => None,
    }
}

/// Best-effort name for an FRN.
fn get_name(frn: u64, index: &Index) -> Option<String> {
    index.by_frn.get(&frn).map(|r| {
        if r.is_dir {
            format!("{}/", r.name)
        } else {
            r.name.clone()
        }
    })
}

/// Sample up to `max` child names from a directory's index entry.
fn sample_children(frn: u64, index: &Index, max: usize) -> Vec<String> {
    let children = match index.children.get(&frn) {
        Some(c) => c,
        None => return vec![],
    };

    children
        .iter()
        .filter_map(|&c| get_name(c, index))
        .take(max)
        .collect()
}

// ---- Dispatch LLM call ----

fn do_summarize(endpoint: &str, model: &str, wi: &WorkItem) -> Result<DirSummary, String> {
    let path_str = wi.path.to_string_lossy();
    match &wi.kind {
        WorkKind::Dir { samples, ancestor_context, size_mb, content_summary } => {
            llm::summarize_dir(endpoint, model, &path_str, samples, ancestor_context.as_deref(), *size_mb, content_summary)
        }
        WorkKind::File {
            ext,
            parent_dir,
            siblings,
            ancestor_context,
            size_mb,
        } => llm::summarize_file(endpoint, model, &path_str, parent_dir, siblings, ext, ancestor_context.as_deref(), *size_mb),
    }
}

// ---- Setup hint ----

/// Tell the user what to do when Ollama is not available.
pub fn setup_hint() -> &'static str {
    "LLM enrichment not available (Ollama not running at http://localhost:11434).\n\
     To install: .\\scripts\\setup_ollama.ps1\n\
     Falling back to rule-only mode."
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::index::build_index;
    use crate::model::{RawRecord, Risk};

    // ---- Helpers for work-item / index tests ----
    fn dir(frn: u64, parent: u64, name: &str) -> RawRecord {
        RawRecord {
            frn,
            parent_frn: parent,
            name: name.into(),
            is_dir: true,
            is_reparse: false,
            logical_size: 0,
            physical_size: 0,
            hard_link_count: 1,
            in_use: true,
        }
    }
    fn file(frn: u64, parent: u64, name: &str, size: u64) -> RawRecord {
        RawRecord {
            frn,
            parent_frn: parent,
            name: name.into(),
            is_dir: false,
            is_reparse: false,
            logical_size: size,
            physical_size: size,
            hard_link_count: 1,
            in_use: true,
        }
    }

    // ===================================================================
    //  Existing work-item / index tests (unchanged)
    // ===================================================================

    #[test]
    fn parent_context_returns_siblings() {
        let records = vec![
            dir(10, 5, "myproject"),
            file(20, 10, "video.mp4", 1000),
            file(21, 10, "README.md", 100),
            file(22, 10, "script.py", 200),
        ];
        let index = build_index(records);
        let (parent, sibs) = parent_context(20, &index);
        assert_eq!(parent, "myproject/");
        assert!(sibs.contains(&"README.md".to_string()));
        assert!(sibs.contains(&"script.py".to_string()));
        assert!(!sibs.contains(&"video.mp4".to_string()));
    }

    #[test]
    fn collect_work_finds_heuristic_files() {
        let index = build_index(vec![
            dir(10, 5, "stuff"),
            file(20, 10, "big.bin", 5000),
        ]);
        let items = vec![Item {
            frn: 20,
            path: PathBuf::from(r"C:\stuff\big.bin"),
            is_dir: false,
            physical_size: 5000,
            file_count: 1,
            category: "Data/cache".into(),
            purpose: "Binary data".into(),
            risk: Risk::Unknown,
            source: Source::Heuristic,
        }];
        let work = collect_work(&items, &index, 5);
        assert_eq!(work.len(), 1);
        assert!(matches!(work[0].kind, WorkKind::File { .. }));
    }

    #[test]
    fn find_ancestor_context_detects_project_and_git() {
        let records = vec![
            dir(10, 5, "github"),
            dir(11, 10, "myrepo"),
            dir(12, 11, ".git"),
            dir(13, 11, "src"),
            file(20, 13, "main.rs", 5000),
        ];
        let index = build_index(records);
        let ctx = find_ancestor_context(20, &index);
        assert_eq!(ctx, Some("project 'myrepo' (git repository)".to_string()));
    }

    #[test]
    fn find_ancestor_context_project_without_git() {
        let records = vec![
            dir(10, 5, "ee451"),
            dir(11, 10, "SuperWeb-Cluster"),
            dir(12, 11, "generated"),
            dir(13, 12, "artifacts"),
            file(20, 13, "matrix.bin", 5000),
        ];
        let index = build_index(records);
        let ctx = find_ancestor_context(20, &index);
        assert_eq!(ctx, Some("project 'SuperWeb-Cluster'".to_string()));
    }

    #[test]
    fn find_ancestor_context_returns_none_for_shallow_tree() {
        let records = vec![
            dir(10, 5, "Downloads"),
            file(20, 10, "test.zip", 5000),
        ];
        let index = build_index(records);
        assert_eq!(
            find_ancestor_context(20, &index),
            Some("project 'Downloads'".to_string())
        );
    }

    #[test]
    fn find_ancestor_context_none_at_root() {
        let records = vec![file(20, 5, "bigfile.bin", 5000)];
        let index = build_index(records);
        assert_eq!(find_ancestor_context(20, &index), None);
    }
}

