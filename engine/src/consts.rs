//! Centralized constants for the entire project.
//!
//! Every module MUST get its constants from this file — no more
//! scattered `const` definitions across the codebase.
//! Change a value here once and it takes effect everywhere,
//! including all tests.

use std::time::Duration;

// ============================================================================
//  MFT / model
// ============================================================================

/// MFT record number of the volume root directory.
pub const ROOT_FRN: u64 = 5;

// ============================================================================
//  Dynamic concurrency-control (cwnd) — throughput-driven
// ============================================================================
//
// Design (throughput-maximizing, not latency-minimizing):
//   Every PROBE_INTERVAL, the supervisor computes window throughput (req/s)
//   and compares it to the best observed throughput.  cwnd grows aggressively
//   while throughput improves; when throughput drops, cwnd snaps back to the
//   best-observed value.  Periodic upward probes test for increased capacity
//   (e.g., model warming up, competing workload finishing).
//
//   This is fundamentally different from Vegas: we don't care about individual
//   request latency — only total wall-clock time matters for batch workloads.

/// Starting concurrency window — computed at runtime to match
/// `worker_thread_limit` (2× logical processors).
///
/// On the RTX 4060 Mobile test machine (16 logical processors) this is 32,
/// which is the empirically measured peak-throughput concurrency.
/// Bypasses the slow ramp-up entirely: the throughput-driven algorithm
/// starts at the optimal point and only adjusts if things change.
pub fn cwnd_init() -> usize {
    worker_thread_limit(usize::MAX)
}

/// Minimum cwnd (never drop below 2 — cwnd=1 can't observe parallelism benefit).
pub const CWND_MIN: usize = 2;

/// Linear backoff step.  When congestion is detected, subtract this fixed
/// amount from cwnd instead of halving (multiplicative decrease).
/// Uses the smallest sensible unit of concurrency reduction.
pub const CWND_LINEAR_DECR: usize = 1;

/// Safety cap on concurrency — 2^16.  Not expected to be hit: the GPU
/// saturates long before this, but the user asked for a very large ceiling
/// so the probe, not a constant, is the real governor.
pub const MAX_SAFETY_CWND: usize = 65536;

/// Absolute safety cap on worker threads — never exceed this regardless of
/// machine size.  The real cap is computed dynamically by [`worker_thread_limit`].
const MAX_WORKER_THREADS: usize = 4096;

/// Compute the worker-thread cap for a given batch of work items.
///
/// Uses 2× the number of available logical processors as the parallelism
/// ceiling, bounded by:
///   - `total_work_items`  (never spawn more threads than work)
///   - `MAX_WORKER_THREADS` (absolute safety cap)
///   - at least 1 thread.
///
/// Each worker thread makes blocking HTTP calls to Ollama, so the extra
/// multiplier provides pipelining headroom above CPU count.
pub fn worker_thread_limit(total_work_items: usize) -> usize {
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4); // fallback: conservative
    let soft_cap = cpus.saturating_mul(2);
    total_work_items.min(soft_cap).min(MAX_WORKER_THREADS).max(1)
}

/// How often the supervisor probes throughput and adjusts cwnd.
pub const PROBE_INTERVAL: Duration = Duration::from_millis(500);

// ============================================================================
//  Throughput-driven cwnd control
// ============================================================================
//
// Every PROBE_INTERVAL, the supervisor:
//   1. Computes current_tp = completed_this_window / PROBE_INTERVAL (req/s)
//   2. Compares to best_tp (best observed throughput so far)
//   3. Grows cwnd while throughput improves, snaps back when it drops
//   4. Periodically probes upward to discover increased capacity

/// Additive cwnd increase per window while throughput is improving.
pub const TP_GROW_STEP: usize = 4;

/// How much to increase cwnd during a periodic upward probe.
pub const TP_PROBE_STEP: usize = 8;

/// Number of consecutive stable windows before triggering an upward probe.
/// 6 × 500 ms = 3 s of stable throughput before probing.
pub const TP_PROBE_AFTER_STABLE: u32 = 6;

/// Throughput ratio threshold: consider "still improving" if current >= prev * this.
/// 0.97 = allow 3% noise before declaring a drop.
pub const TP_IMPROVING_RATIO: f64 = 0.97;

/// Throughput ratio threshold for probe success: probe paid off if current > best * this.
/// 1.03 = require 3% improvement to confirm probe found more capacity.
pub const TP_PROBE_WIN_RATIO: f64 = 1.03;

// ============================================================================
//  Selective Repeat (retry)
// ============================================================================

/// Maximum retries per individual LLM request before giving up.
pub const MAX_RETRIES: u32 = 3;

/// Base backoff between retries (exponential: delay × 2^attempt).
pub const RETRY_BASE_DELAY: Duration = Duration::from_millis(200);

// ============================================================================
//  HTTP / LLM timeouts
// ============================================================================

/// Timeout for each LLM enrichment request.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Timeout for Ollama health-check ping.
pub const HEALTH_TIMEOUT: Duration = Duration::from_secs(2);

/// Shorter timeout for final report (faster fail if Ollama is busy).
/// Report generation needs more time: think:true + 32K ctx + num_predict(4096).
/// 15 s was way too tight — the thinking phase alone can take 10-20 s on
/// modest hardware.  120 s gives generous headroom without blocking forever.
pub const REPORT_TIMEOUT: Duration = Duration::from_secs(120);

// ============================================================================
//  Sanity checks (compile-time)
// ============================================================================

#[allow(clippy::absurd_extreme_comparisons)]
const _: () = {
    // CWND_MIN / MAX_SAFETY_CWND / MAX_WORKER_THREADS must be sane.
    // cwnd_init() is runtime-computed and always ≤ MAX_WORKER_THREADS
    // by construction (worker_thread_limit caps at MAX_WORKER_THREADS).
    assert!(CWND_MIN >= 2);
    assert!(MAX_WORKER_THREADS <= 4096);
    assert!(MAX_SAFETY_CWND <= 65536);
    assert!(TP_GROW_STEP > 0);
    assert!(TP_PROBE_STEP > TP_GROW_STEP);
    assert!(TP_IMPROVING_RATIO > 0.0 && TP_IMPROVING_RATIO < 1.0);
    assert!(TP_PROBE_WIN_RATIO > 1.0);
};
