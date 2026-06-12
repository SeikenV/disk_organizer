mod content;
mod llm;

use crate::index::Index;
use crate::model::{Item, Source};
use crate::report::ReportFile;
use log::{info, warn, error};
use std::path::PathBuf;
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::thread;
use std::time::Instant;
#[cfg(test)]
use std::time::Duration;

pub use content::analyze_directory_contents;
pub use llm::health_check as is_ollama_running;
pub use llm::preload_model;
pub use llm::summarize_report;
pub use llm::{DirSummary, FinalReport};

// ---- Re-export centralized constants (convenience for intra-module use) ----
use crate::consts::{
    CWND_INIT, CWND_MIN,
    MAX_RETRIES, MAX_SAFETY_CWND,
    PROBE_INTERVAL, RETRY_BASE_DELAY,
    VEGAS_ALPHA, VEGAS_BETA,
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
            model: "qwen3.5:0.8b".into(),
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
    },
    File {
        ext: String,
        /// Parent directory path (for context).
        parent_dir: String,
        /// Sibling file/dir names in the same parent.
        siblings: Vec<String>,
        /// Nearest meaningful ancestor (project-level context), e.g. "myproject (git repo)".
        ancestor_context: Option<String>,
    },
}

struct WorkItem {
    /// Index into the original `items` slice.
    idx: usize,
    path: PathBuf,
    kind: WorkKind,
}

// ---- Probe record types ----

/// One row in the probe log, written every PROBE_INTERVAL by the supervisor.
#[derive(Debug, Clone, serde::Serialize)]
struct ProbeRecord {
    elapsed_ms: u64,
    cwnd: usize,
    inflight: usize,
    completed_in_window: usize,
    throughput_rps: f64,
    srtt_ms: f64,
    /// Vegas base_rtt — current minimum observed latency (ms).
    base_rtt_ms: f64,
    /// Effective per-task completion pacing in this window (ms).
    per_task_ms: f64,
    /// Vegas estimated queue depth (jobs).
    est_queue: f64,
    /// Vegas decision: "grow", "shrink", or "steady".
    vegas_action: String,
}

// ---- Cwnd: Vegas concurrency control ----
//
// Design (TCP Vegas, adapted for LLM workload):
//   Workers acquire/release permits via blocking Condvar (unchanged).
//   Every successful request feeds a latency sample to the Vegas estimator
//   which continuously tracks base_rtt (minimum observed latency) and uses
//   estimated queue depth to decide cwnd adjustments.
//
//   base_rtt is NOT frozen — it tracks the running minimum of all successful
//   latency samples, so cwnd naturally grows as the model warms up and
//   individual request latency drops.

struct CwndCtl {
    /// Current concurrency window — written by Vegas update after each request.
    cwnd: AtomicUsize,
    /// Currently in-flight requests.
    inflight: AtomicUsize,
    /// Condition variable + mutex: workers wait here instead of spin-looping.
    permit_mutex: Mutex<()>,
    permit_cv: Condvar,
    /// Smoothed round-trip time (ms). EWMA, for display only.
    srtt_ms: Mutex<f64>,
    /// Vegas base_rtt — minimum observed successful latency (ms).
    vegas_base_rtt_ms: Mutex<f64>,
    /// Requests completed since last probe snapshot (workers inc, supervisor resets).
    completed_this_window: AtomicUsize,
    /// Peak inflight observed (for final summary).
    peak_inflight: AtomicUsize,
    /// Peak cwnd observed (for final summary).
    peak_cwnd: AtomicUsize,
    /// Probe log — supervisor appends one record per cycle.
    probe_log: Mutex<Vec<ProbeRecord>>,
    /// Wall-clock instant when this controller was created.
    start: Instant,
}

impl CwndCtl {
    fn new() -> Self {
        Self {
            cwnd: AtomicUsize::new(CWND_INIT),
            inflight: AtomicUsize::new(0),
            permit_mutex: Mutex::new(()),
            permit_cv: Condvar::new(),
            srtt_ms: Mutex::new(0.0),
            vegas_base_rtt_ms: Mutex::new(0.0),
            completed_this_window: AtomicUsize::new(0),
            peak_inflight: AtomicUsize::new(0),
            peak_cwnd: AtomicUsize::new(CWND_INIT),
            probe_log: Mutex::new(Vec::new()),
            start: Instant::now(),
        }
    }

    /// Block until a concurrency permit is available.
    /// Uses Condvar-based blocking — zero CPU while waiting.
    fn acquire(&self) {
        loop {
            let c = self.cwnd.load(Ordering::Relaxed);
            let f = self.inflight.fetch_add(1, Ordering::SeqCst);
            if f < c {
                // Got permit — track peak inflight.
                let inf = f + 1;
                let mut prev = self.peak_inflight.load(Ordering::Relaxed);
                while inf > prev {
                    match self.peak_inflight.compare_exchange_weak(
                        prev, inf, Ordering::Relaxed, Ordering::Relaxed,
                    ) {
                        Ok(_) => break,
                        Err(p) => prev = p,
                    }
                }
                return;
            }
            // Overshot — undo and block on Condvar.
            self.inflight.fetch_sub(1, Ordering::SeqCst);
            let guard = self.permit_mutex.lock().unwrap();
            // Re-check before sleeping (handles spurious wakeups and races).
            if self.inflight.load(Ordering::Relaxed) < self.cwnd.load(Ordering::Relaxed) {
                drop(guard); // explicitly drop before retry
                continue; // permit freed while locking — retry
            }
            drop(self.permit_cv.wait(guard).unwrap());
        }
    }

    /// Report a successful completion.  Workers call this; Vegas adjusts cwnd.
    fn release_success(&self, latency_ms: f64) {
        self.inflight.fetch_sub(1, Ordering::SeqCst);
        self.completed_this_window.fetch_add(1, Ordering::Relaxed);

        // Update SRTT EWMA (for display).
        {
            let mut srtt = self.srtt_ms.lock().unwrap();
            if *srtt == 0.0 {
                *srtt = latency_ms;
            } else {
                *srtt = 0.875 * *srtt + 0.125 * latency_ms;
            }
        }

        // Vegas congestion control — per-sample update.
        self.vegas_update(latency_ms);

        self.permit_cv.notify_one();
    }

    /// Vegas congestion control: estimate queue depth, adjust cwnd.
    ///
    /// Uses TCP Vegas logic adapted for LLM workloads:
    /// - base_rtt = running minimum latency (NOT frozen — continuously updated)
    /// - est_queue = (in_flight / latency) × (latency - base_rtt)
    /// - Grow when est_queue < α × log₁₀(cwnd)
    /// - Shrink when est_queue > β × log₁₀(cwnd)
    #[allow(clippy::cast_precision_loss)]
    fn vegas_update(&self, latency_ms: f64) {
        let cwnd = self.cwnd.load(Ordering::Relaxed);
        let inflight = self.inflight.load(Ordering::Relaxed).max(1); // snapshot after release

        let mut base = self.vegas_base_rtt_ms.lock().unwrap();

        // Track minimum observed latency — this is the key fix: base_rtt is
        // continuously updated, so cwnd can grow as the model warms up.
        if *base == 0.0 {
            *base = latency_ms;
            return; // first sample, no decision yet
        }
        if latency_ms < *base {
            *base = latency_ms;
            // New lower baseline — potentially room to grow.
            // Fall through to normal evaluation.
        }

        let extra_latency = latency_ms - *base;
        if extra_latency <= 0.0 {
            return; // at or below baseline, no congestion signal
        }

        // throughput = inflight / latency (jobs per ms)
        // est_queue = throughput × extra_latency  (estimated queued jobs)
        let throughput = inflight as f64 / latency_ms.max(0.001);
        let est_queue = throughput * extra_latency;

        // Thresholds scale with log₁₀(cwnd) — more slack at higher cwnd.
        let log_cwnd = (cwnd as f64).log10().max(1.0);
        let alpha = VEGAS_ALPHA * log_cwnd;
        let beta = VEGAS_BETA * log_cwnd;

        let new_cwnd = if est_queue > beta {
            // Congestion: too many queued jobs.
            cwnd.saturating_sub(1).max(CWND_MIN)
        } else if est_queue < alpha {
            // Headroom: room to increase concurrency.
            (cwnd + 1).min(MAX_SAFETY_CWND)
        } else {
            // Sweet spot: alpha..=beta band, no change.
            cwnd
        };

        if new_cwnd != cwnd {
            self.cwnd.store(new_cwnd, Ordering::Relaxed);

            // Track peak cwnd.
            let mut p = self.peak_cwnd.load(Ordering::Relaxed);
            while new_cwnd > p {
                match self.peak_cwnd.compare_exchange_weak(
                    p, new_cwnd, Ordering::Relaxed, Ordering::Relaxed,
                ) {
                    Ok(_) => break,
                    Err(v) => p = v,
                }
            }

            // Wake blocked workers if cwnd increased.
            if new_cwnd > cwnd {
                self.permit_cv.notify_all();
            }
        }
    }

    /// Report a failed completion (after all retries exhausted).
    fn release_failure(&self) {
        self.inflight.fetch_sub(1, Ordering::SeqCst);
        self.completed_this_window.fetch_add(1, Ordering::Relaxed);
        self.permit_cv.notify_one();
        // Don't update SRTT on failure — the error isn't a latency sample.
    }

    /// Release a permit temporarily (for retry backoff) WITHOUT counting
    /// the request as completed.  Caller must re-acquire() after the sleep.
    fn release_retry(&self) {
        self.inflight.fetch_sub(1, Ordering::SeqCst);
        self.permit_cv.notify_one();
    }

    fn srtt(&self) -> f64 {
        *self.srtt_ms.lock().unwrap()
    }

    fn base_rtt(&self) -> f64 {
        *self.vegas_base_rtt_ms.lock().unwrap()
    }

    /// Wake all workers blocked on `acquire()`.  Used by the supervisor
    /// after cwnd is increased, and by tests.
    #[allow(dead_code)]
    fn notify_waiters(&self) {
        self.permit_cv.notify_all();
    }
}

// ---- Supervisor: periodic progress print + probe log ----

/// Runs the supervisor for one or two backends.
///
/// Every PROBE_INTERVAL the supervisor snapshots cwnd/inflight/srtt/base_rtt
/// and appends a record to the probe log.  It does NOT make any congestion
/// control decisions — Vegas handles that per-sample in `release_success()`.
///
/// The supervisor also prints periodic progress to stderr.
fn run_supervisor(
    gpu_ctl: &Arc<CwndCtl>,
    igpu_ctl: Option<&Arc<CwndCtl>>,
    total: usize,
    done: &Arc<AtomicUsize>,
) {
    let mut last_completed: usize = 0;

    loop {
        thread::sleep(PROBE_INTERVAL);
        let d = done.load(Ordering::Relaxed);
        if d >= total {
            break;
        }

        append_probe_log(gpu_ctl, total, &mut last_completed);

        let gpu_cwnd = gpu_ctl.cwnd.load(Ordering::Relaxed);
        let gpu_inf = gpu_ctl.inflight.load(Ordering::Relaxed);
        let gpu_srtt = gpu_ctl.srtt();
        let gpu_base = gpu_ctl.base_rtt();

        if let Some(ictl) = igpu_ctl {
            append_probe_log(ictl, total, &mut last_completed);
            let icwnd = ictl.cwnd.load(Ordering::Relaxed);
            let iinf = ictl.inflight.load(Ordering::Relaxed);
            let isrtt = ictl.srtt();
            let ibase = ictl.base_rtt();
            eprint!(
                "\r  [{d}/{total}] dGPU cwnd={gpu_cwnd} infl={gpu_inf} srtt={gpu_srtt:.0}ms base={gpu_base:.0}ms | iGPU cwnd={icwnd} infl={iinf} srtt={isrtt:.0}ms base={ibase:.0}ms  ",
            );
        } else {
            eprint!(
                "\r  [{d}/{total}] cwnd={gpu_cwnd} infl={gpu_inf} srtt={gpu_srtt:.0}ms base={gpu_base:.0}ms  ",
            );
        }
    }
}

/// Snapshot the current state into the probe log for one backend.
fn append_probe_log(ctl: &CwndCtl, total: usize, last_completed: &mut usize) {
    let completed_now = ctl.completed_this_window.load(Ordering::Relaxed);
    let completed_in_window = completed_now.saturating_sub(*last_completed);
    *last_completed = completed_now;

    let current_cwnd = ctl.cwnd.load(Ordering::Relaxed);
    let snapshot_inflight = ctl.inflight.load(Ordering::Relaxed);
    let srtt = ctl.srtt();
    let base_rtt = ctl.base_rtt();
    let window_throughput =
        completed_in_window as f64 / PROBE_INTERVAL.as_secs_f64();
    let per_task_ms = if completed_in_window > 0 {
        PROBE_INTERVAL.as_millis() as f64 / completed_in_window as f64
    } else {
        0.0
    };

    // Compute Vegas est_queue for the log (approximate, using last known values).
    let est_queue = if base_rtt > 0.0 && srtt > base_rtt {
        let inf = snapshot_inflight.max(1) as f64;
        let throughput = inf / srtt.max(0.001);
        throughput * (srtt - base_rtt)
    } else {
        0.0
    };

    let log_cwnd = (current_cwnd as f64).log10().max(1.0);
    let alpha = VEGAS_ALPHA * log_cwnd;
    let beta = VEGAS_BETA * log_cwnd;
    let action = if est_queue > beta {
        "shrink"
    } else if est_queue < alpha {
        "grow"
    } else {
        "steady"
    };

    let max_cwnd = total.min(MAX_SAFETY_CWND);
    let vegas_action = if current_cwnd >= max_cwnd {
        "capped"
    } else {
        action
    };

    ctl.probe_log.lock().unwrap().push(ProbeRecord {
        elapsed_ms: ctl.start.elapsed().as_millis() as u64,
        cwnd: current_cwnd,
        inflight: snapshot_inflight,
        completed_in_window,
        throughput_rps: window_throughput,
        srtt_ms: srtt,
        base_rtt_ms: base_rtt,
        per_task_ms,
        est_queue,
        vegas_action: vegas_action.into(),
    });
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
        total, dir_count, file_count, config.model, gpu_threads, igpu_threads, CWND_INIT,
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
    if let Err(e) = llm::preload_model(&config.endpoint, &config.model) {
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
        items[*idx].source = Source::Rule;
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
            // --- stderr: compact table ---
            info!("");
            info!("{}", "=".repeat(80));
            info!("  PROBE LOG (Vegas congestion control)");
            info!("{}", "=".repeat(80));
            info!(
                "  {:>7} {:>5} {:>5} {:>7} {:>8} {:>7} {:>7} {:>7} {:>8} {:>7}",
                "elapsed", "cwnd", "infl", "done/w", "rps", "srtt", "base", "t/ms", "queue", "action"
            );
            info!("  {}", "-".repeat(76));
            for r in log.iter() {
                info!(
                    "  {:>6}ms {:>4} {:>4} {:>6} {:>7.1} {:>6.0} {:>6.0} {:>7.0} {:>7.1} {:>7}",
                    r.elapsed_ms, r.cwnd, r.inflight, r.completed_in_window,
                    r.throughput_rps, r.srtt_ms, r.base_rtt_ms, r.per_task_ms,
                    r.est_queue, r.vegas_action,
                );
            }
            info!("{}", "=".repeat(80));

            // --- Disk: detailed table ---
            if let Some(ref mut rep) = report {
                let _ = rep.section("PROBE LOG (Vegas congestion control)");
                let _ = rep.line(&format!(
                    "  {:>7} {:>5} {:>5} {:>7} {:>8} {:>7} {:>7} {:>7} {:>8} {:>7}",
                    "elapsed", "cwnd", "infl", "done/w", "rps", "srtt", "base", "t/ms", "queue", "action"
                ));
                let _ = rep.line(&format!("  {}", "-".repeat(76)));
                for r in log.iter() {
                    let _ = rep.line(&format!(
                        "  {:>6}ms {:>4} {:>4} {:>6} {:>7.1} {:>6.0} {:>6.0} {:>7.0} {:>7.1} {:>7}",
                        r.elapsed_ms, r.cwnd, r.inflight, r.completed_in_window,
                        r.throughput_rps, r.srtt_ms, r.base_rtt_ms, r.per_task_ms,
                        r.est_queue, r.vegas_action,
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
    const REPORT_TOP_PER_GROUP: usize = 15;

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
    if !llm::health_check(&config.endpoint) {
        warn!("\n[LLM] Ollama not responding, skipping final report.");
        if let Some(ref mut rep) = report {
            let _ = rep.line("[LLM] Ollama not responding, skipping final report.");
        }
    } else {
        for attempt in 1..=2 {
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
                work.push(WorkItem {
                    idx: i,
                    path: it.path.clone(),
                    kind: WorkKind::Dir {
                        samples: sample_children(it.frn, index, sample_count),
                        ancestor_context,
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
        WorkKind::Dir { samples, ancestor_context } => {
            llm::summarize_dir(endpoint, model, &path_str, samples, ancestor_context.as_deref())
        }
        WorkKind::File {
            ext,
            parent_dir,
            siblings,
            ancestor_context,
        } => llm::summarize_file(endpoint, model, &path_str, parent_dir, siblings, ext, ancestor_context.as_deref()),
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
    use crate::index::build_index;
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
    //  CwndCtl — init (Vegas)
    // ===================================================================

    #[test]
    fn cwndctl_init_defaults() {
        let ctl = CwndCtl::new();
        assert_eq!(ctl.cwnd.load(Ordering::Relaxed), CWND_INIT);
        assert_eq!(ctl.inflight.load(Ordering::Relaxed), 0);
        assert_eq!(ctl.srtt(), 0.0);
        assert_eq!(ctl.base_rtt(), 0.0);
        assert_eq!(ctl.peak_inflight.load(Ordering::Relaxed), 0);
        assert_eq!(ctl.peak_cwnd.load(Ordering::Relaxed), CWND_INIT);
        assert!(ctl.probe_log.lock().unwrap().is_empty());
    }

    // ===================================================================
    //  CwndCtl — acquire / release (single-threaded)
    // ===================================================================

    #[test]
    fn cwndctl_acquire_increments_inflight() {
        let ctl = CwndCtl::new();
        assert_eq!(ctl.inflight.load(Ordering::Relaxed), 0);
        ctl.acquire();
        assert_eq!(ctl.inflight.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn cwndctl_release_success_decrements_inflight() {
        let ctl = CwndCtl::new();
        ctl.acquire();
        assert_eq!(ctl.inflight.load(Ordering::Relaxed), 1);
        ctl.release_success(100.0);
        assert_eq!(ctl.inflight.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn cwndctl_release_failure_decrements_inflight() {
        let ctl = CwndCtl::new();
        ctl.acquire();
        ctl.release_failure();
        assert_eq!(ctl.inflight.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn cwndctl_acquires_up_to_cwnd_before_blocking() {
        let ctl = CwndCtl::new();
        for _ in 0..CWND_INIT {
            ctl.acquire();
        }
        assert_eq!(ctl.inflight.load(Ordering::Relaxed), CWND_INIT);
        ctl.release_success(50.0);
        ctl.acquire();
        assert_eq!(ctl.inflight.load(Ordering::Relaxed), CWND_INIT);
    }

    #[test]
    fn cwndctl_acquire_blocks_when_cwnd_zero() {
        let ctl = Arc::new(CwndCtl::new());
        ctl.cwnd.store(0, Ordering::Relaxed);

        let done = Arc::new(AtomicUsize::new(0));
        let done_clone = Arc::clone(&done);
        let ctl_clone = Arc::clone(&ctl);

        let h = thread::spawn(move || {
            ctl_clone.acquire();
            done_clone.store(1, Ordering::Relaxed);
        });

        thread::sleep(Duration::from_millis(50));
        assert_eq!(done.load(Ordering::Relaxed), 0);

        ctl.cwnd.store(1, Ordering::Relaxed);
        ctl.notify_waiters();

        h.join().unwrap();
        assert_eq!(done.load(Ordering::Relaxed), 1);
    }

    // ===================================================================
    //  CwndCtl — peak tracking
    // ===================================================================

    #[test]
    fn cwndctl_peak_inflight_tracks_max() {
        let ctl = CwndCtl::new();
        ctl.cwnd.store(4, Ordering::Relaxed);
        ctl.notify_waiters();
        ctl.acquire();
        ctl.acquire();
        ctl.acquire();
        assert_eq!(ctl.peak_inflight.load(Ordering::Relaxed), 3);
        ctl.release_success(50.0);
        ctl.release_success(50.0);
        assert_eq!(ctl.peak_inflight.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn cwndctl_peak_inflight_persists_below_current() {
        let ctl = CwndCtl::new();
        ctl.cwnd.store(8, Ordering::Relaxed);
        ctl.notify_waiters();
        for _ in 0..8 {
            ctl.acquire();
        }
        assert_eq!(ctl.peak_inflight.load(Ordering::Relaxed), 8);
        for _ in 0..8 {
            ctl.release_success(50.0);
        }
        assert_eq!(ctl.peak_inflight.load(Ordering::Relaxed), 8);
        ctl.acquire();
        ctl.acquire();
        assert_eq!(ctl.peak_inflight.load(Ordering::Relaxed), 8);
    }

    // ===================================================================
    //  CwndCtl — SRTT (EWMA)
    // ===================================================================

    #[test]
    fn cwndctl_srtt_first_sample_sets_directly() {
        let ctl = CwndCtl::new();
        assert_eq!(ctl.srtt(), 0.0);
        ctl.release_success(250.0);
        assert!((ctl.srtt() - 250.0).abs() < 0.001);
    }

    #[test]
    fn cwndctl_srtt_ewma_converges() {
        let ctl = CwndCtl::new();
        ctl.release_success(100.0);
        ctl.release_success(200.0);
        let expected = 0.875 * 100.0 + 0.125 * 200.0;
        assert!((ctl.srtt() - expected).abs() < 0.001);
        ctl.release_success(112.5);
        let expected2 = 0.875 * expected + 0.125 * 112.5;
        assert!((ctl.srtt() - expected2).abs() < 0.001);
    }

    #[test]
    fn cwndctl_release_failure_does_not_update_srtt() {
        let ctl = CwndCtl::new();
        ctl.release_success(100.0);
        assert!((ctl.srtt() - 100.0).abs() < 0.001);
        ctl.release_failure();
        assert!((ctl.srtt() - 100.0).abs() < 0.001);
    }

    // ===================================================================
    //  Vegas congestion control — base_rtt tracking
    // ===================================================================

    #[test]
    fn vegas_base_rtt_tracks_minimum() {
        let ctl = CwndCtl::new();
        ctl.acquire();
        ctl.release_success(2000.0);
        assert!((ctl.base_rtt() - 2000.0).abs() < 0.01);

        ctl.acquire();
        ctl.release_success(1500.0);
        assert!((ctl.base_rtt() - 1500.0).abs() < 0.01); // dropped

        ctl.acquire();
        ctl.release_success(1800.0);
        assert!((ctl.base_rtt() - 1500.0).abs() < 0.01); // no change, 1800 > 1500
    }

    #[test]
    fn vegas_zero_base_rtt_no_decision() {
        // First sample just sets base_rtt, doesn't change cwnd.
        let ctl = CwndCtl::new();
        assert_eq!(ctl.cwnd.load(Ordering::Relaxed), CWND_INIT);
        ctl.acquire();
        ctl.release_success(1000.0);
        assert_eq!(ctl.cwnd.load(Ordering::Relaxed), CWND_INIT); // cwnd unchanged
        assert!((ctl.base_rtt() - 1000.0).abs() < 0.01);
    }

    #[test]
    fn vegas_update_drops_base_rtt_then_grows_cwnd() {
        // base_rtt starts at 2000ms (cold), then drops to ~1000ms (warm).
        // cwnd should grow once samples are above the new lower baseline.
        let ctl = CwndCtl::new();
        ctl.cwnd.store(4, Ordering::Relaxed);

        // First: establish high baseline (cold start).
        ctl.acquire();
        ctl.release_success(2000.0);
        assert_eq!(ctl.cwnd.load(Ordering::Relaxed), 4); // first sample, no decision
        assert!((ctl.base_rtt() - 2000.0).abs() < 0.01);

        // Second: latency drops below baseline → base_rtt drops to new minimum.
        ctl.acquire();
        ctl.release_success(1010.0);
        assert!((ctl.base_rtt() - 1010.0).abs() < 0.01); // base dropped
        // cwnd may not change because extra_latency ≈ 0 on the drop sample.

        // Third: latency slightly above new baseline → est_queue tiny → grow.
        ctl.acquire();
        ctl.release_success(1020.0);
        assert!(ctl.cwnd.load(Ordering::Relaxed) > 4,
            "cwnd should grow on 3rd sample, got {}", ctl.cwnd.load(Ordering::Relaxed));
    }

    // ===================================================================
    //  Vegas congestion control — queue estimation
    // ===================================================================

    /// Helper: create a CwndCtl pre-seeded with base_rtt and SRTT.
    fn ctl_with_vegas_state(base_rtt: f64, cwnd_val: usize, srtt_val: f64) -> CwndCtl {
        let ctl = CwndCtl::new();
        *ctl.vegas_base_rtt_ms.lock().unwrap() = base_rtt;
        ctl.cwnd.store(cwnd_val, Ordering::Relaxed);
        if srtt_val > 0.0 {
            *ctl.srtt_ms.lock().unwrap() = srtt_val;
        }
        ctl
    }

    #[test]
    fn vegas_grows_when_queue_is_low() {
        // base=500, latency=510 (near baseline) → est_queue ≈ 0 → grow.
        let ctl = ctl_with_vegas_state(500.0, 4, 510.0);
        ctl.inflight.store(4, Ordering::Relaxed); // simulate inflight
        ctl.vegas_update(510.0);
        assert!(ctl.cwnd.load(Ordering::Relaxed) > 4, "expected cwnd to grow, got {}", ctl.cwnd.load(Ordering::Relaxed));
    }

    #[test]
    fn vegas_shrinks_when_queue_is_high() {
        // base=500, latency=2200 → est_queue > beta=6 → shrink.
        // inflight=8, throughput=8/2200≈0.00364, extra=1700, est_queue≈6.18
        let ctl = ctl_with_vegas_state(500.0, 8, 2200.0);
        ctl.inflight.store(8, Ordering::Relaxed);
        ctl.vegas_update(2200.0);
        assert!(ctl.cwnd.load(Ordering::Relaxed) < 8,
            "expected cwnd to shrink, got {}", ctl.cwnd.load(Ordering::Relaxed));
    }

    #[test]
    fn vegas_steady_when_queue_in_band() {
        // cwnd=4 (log10(clamped to 1), alpha=3, beta=6), base=500, latency=2000.
        // inflight=4, throughput=4/2000=0.002, extra=1500, est_queue=0.002*1500=3.0.
        // est_queue=3.0 == alpha → not < alpha → not grow.
        // est_queue=3.0 < beta=6 → not > beta → not shrink. → STEADY.
        let ctl = ctl_with_vegas_state(500.0, 4, 2000.0);
        ctl.inflight.store(4, Ordering::Relaxed);
        ctl.vegas_update(2000.0);
        assert_eq!(ctl.cwnd.load(Ordering::Relaxed), 4,
            "expected steady cwnd=4, got {}", ctl.cwnd.load(Ordering::Relaxed));
    }

    #[test]
    fn vegas_respects_cwnd_min() {
        let ctl = ctl_with_vegas_state(500.0, CWND_MIN, 2000.0);
        ctl.inflight.store(CWND_MIN, Ordering::Relaxed);
        ctl.vegas_update(2000.0);
        assert!(ctl.cwnd.load(Ordering::Relaxed) >= CWND_MIN);
    }

    #[test]
    fn vegas_respects_cwnd_max() {
        // cwnd must never exceed MAX_SAFETY_CWND.
        // Start just below max — if it grows, it caps at MAX_SAFETY_CWND.
        let near_max = MAX_SAFETY_CWND - 1;
        let ctl = ctl_with_vegas_state(500.0, near_max, 510.0);
        ctl.inflight.store(near_max, Ordering::Relaxed);
        ctl.vegas_update(510.0);
        assert!(ctl.cwnd.load(Ordering::Relaxed) <= MAX_SAFETY_CWND,
            "cwnd must not exceed MAX_SAFETY_CWND");
    }

    #[test]
    fn vegas_tracks_peak_cwnd() {
        let ctl = ctl_with_vegas_state(500.0, 4, 500.0);
        assert_eq!(ctl.peak_cwnd.load(Ordering::Relaxed), CWND_INIT);
        ctl.inflight.store(4, Ordering::Relaxed);
        ctl.vegas_update(510.0);
        let peak = ctl.peak_cwnd.load(Ordering::Relaxed);
        assert!(peak >= ctl.cwnd.load(Ordering::Relaxed));
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

    // ===================================================================
    //  completed_this_window counter
    // ===================================================================

    #[test]
    fn cwndctl_completed_counter_increments_on_success_and_failure() {
        let ctl = CwndCtl::new();
        assert_eq!(ctl.completed_this_window.load(Ordering::Relaxed), 0);
        ctl.release_success(100.0);
        assert_eq!(ctl.completed_this_window.load(Ordering::Relaxed), 1);
        ctl.release_failure();
        assert_eq!(ctl.completed_this_window.load(Ordering::Relaxed), 2);
        ctl.release_success(50.0);
        assert_eq!(ctl.completed_this_window.load(Ordering::Relaxed), 3);
    }

    // ===================================================================
    //  run_supervisor — integration tests
    // ===================================================================

    #[test]
    fn run_supervisor_exits_immediately_when_all_done() {
        let ctl = Arc::new(CwndCtl::new());
        let total: usize = 0;
        let done = Arc::new(AtomicUsize::new(0));

        let ctl_clone = Arc::clone(&ctl);
        let done_clone = Arc::clone(&done);
        let h = thread::spawn(move || {
            run_supervisor(&ctl_clone, None, total, &done_clone);
        });

        h.join().unwrap();
        let log = ctl.probe_log.lock().unwrap();
        assert!(log.is_empty());
    }

    #[test]
    fn run_supervisor_runs_until_done_reaches_total() {
        let ctl = Arc::new(CwndCtl::new());
        let total: usize = 10;
        let done = Arc::new(AtomicUsize::new(0));

        let ctl_clone = Arc::clone(&ctl);
        let done_clone = Arc::clone(&done);

        let h = thread::spawn(move || {
            run_supervisor(&ctl_clone, None, total, &done_clone);
        });

        thread::sleep(Duration::from_millis(100));
        done.store(3, Ordering::Relaxed);
        thread::sleep(Duration::from_millis(600));
        done.store(7, Ordering::Relaxed);
        thread::sleep(Duration::from_millis(600));
        done.store(10, Ordering::Relaxed);

        h.join().unwrap();

        let log = ctl.probe_log.lock().unwrap();
        assert!(log.len() >= 2, "expected >=2 probes, got {}", log.len());
        for r in log.iter() {
            assert_eq!(r.throughput_rps, 0.0);
        }
    }

    // ===================================================================
    //  Selective Repeat — retry delay calculation
    // ===================================================================

    #[test]
    fn selective_repeat_delays_are_exponential() {
        let expected: Vec<Duration> = (0..3)
            .map(|a| RETRY_BASE_DELAY * 2u32.pow(a))
            .collect();
        assert_eq!(expected[0], Duration::from_millis(200));
        assert_eq!(expected[1], Duration::from_millis(400));
        assert_eq!(expected[2], Duration::from_millis(800));
    }

    #[test]
    fn selective_repeat_max_retries_is_4_attempts() {
        assert_eq!(MAX_RETRIES, 3);
        let attempts: Vec<u32> = (0..=MAX_RETRIES).collect();
        assert_eq!(attempts.len(), 4);
        assert_eq!(*attempts.last().unwrap(), 3);
    }

    // ===================================================================
    //  CwndCtl — concurrent stress
    // ===================================================================

    #[test]
    fn cwndctl_concurrent_acquire_release_no_deadlock() {
        let ctl = Arc::new(CwndCtl::new());
        let n_threads = 16;
        let n_ops = 50;

        let mut handles = vec![];
        for _ in 0..n_threads {
            let c = Arc::clone(&ctl);
            let h = thread::spawn(move || {
                for _ in 0..n_ops {
                    c.acquire();
                    thread::sleep(Duration::from_micros(100));
                    c.release_success(10.0);
                }
            });
            handles.push(h);
        }

        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(ctl.inflight.load(Ordering::Relaxed), 0);
        assert_eq!(
            ctl.completed_this_window.load(Ordering::Relaxed),
            n_threads * n_ops
        );
    }

    #[test]
    fn cwndctl_cwnd_change_during_load() {
        let ctl = Arc::new(CwndCtl::new());
        let ctl2 = Arc::clone(&ctl);
        let stop = Arc::new(AtomicUsize::new(0));
        let stop2 = Arc::clone(&stop);

        let changer = thread::spawn(move || {
            let mut toggle = false;
            while stop2.load(Ordering::Relaxed) == 0 {
                if toggle {
                    ctl2.cwnd.store(16, Ordering::Relaxed);
                } else {
                    ctl2.cwnd.store(1, Ordering::Relaxed);
                }
                toggle = !toggle;
                thread::sleep(Duration::from_millis(5));
            }
        });

        let mut workers = vec![];
        for _ in 0..4 {
            let c = Arc::clone(&ctl);
            workers.push(thread::spawn(move || {
                for _ in 0..30 {
                    c.acquire();
                    thread::sleep(Duration::from_micros(50));
                    c.release_success(5.0);
                }
            }));
        }

        for w in workers {
            w.join().unwrap();
        }
        stop.store(1, Ordering::Relaxed);
        changer.join().unwrap();

        assert_eq!(ctl.inflight.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn cwndctl_inflight_never_negative() {
        let ctl = Arc::new(CwndCtl::new());
        let n = 100;

        let mut handles = vec![];
        for _ in 0..4 {
            let c = Arc::clone(&ctl);
            handles.push(thread::spawn(move || {
                for _ in 0..n {
                    c.acquire();
                    c.release_success(1.0);
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(ctl.inflight.load(Ordering::Relaxed), 0);
        assert!(ctl.peak_inflight.load(Ordering::Relaxed) > 0);
    }

    // ===================================================================
    //  Constants sanity checks
    // ===================================================================

    #[test]
    fn constants_are_sane() {
        assert!(CWND_INIT >= CWND_MIN);
        assert!(CWND_MIN >= 2);
        assert!(MAX_SAFETY_CWND > CWND_INIT);
        assert!(MAX_SAFETY_CWND <= 65536);
        assert!(VEGAS_ALPHA > 0.0);
        assert!(VEGAS_BETA > VEGAS_ALPHA);
        assert!(MAX_RETRIES <= 5);
        assert_eq!(RETRY_BASE_DELAY, Duration::from_millis(200));
        assert!(PROBE_INTERVAL >= Duration::from_millis(100));
    }

    // ===================================================================
    //  ProbeRecord construction
    // ===================================================================

    #[test]
    fn probe_record_construction() {
        let r = ProbeRecord {
            elapsed_ms: 500,
            cwnd: 16,
            inflight: 16,
            completed_in_window: 8,
            throughput_rps: 16.0,
            srtt_ms: 2500.0,
            base_rtt_ms: 1000.0,
            per_task_ms: 62.5,
            est_queue: 2.5,
            vegas_action: "grow".into(),
        };
        assert_eq!(r.elapsed_ms, 500);
        assert_eq!(r.cwnd, 16);
        assert_eq!(r.inflight, 16);
        assert_eq!(r.completed_in_window, 8);
        assert!((r.throughput_rps - 16.0).abs() < 0.01);
        assert!((r.srtt_ms - 2500.0).abs() < 0.01);
        assert!((r.base_rtt_ms - 1000.0).abs() < 0.01);
        assert!((r.per_task_ms - 62.5).abs() < 0.01);
        assert!((r.est_queue - 2.5).abs() < 0.01);
        assert_eq!(r.vegas_action, "grow");
    }
}
