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
//  Dynamic concurrency-control (cwnd) — SRTT-based congestion probe
// ============================================================================

/// Starting concurrency window.  We probe upward aggressively so a low
/// starting value is fine — the sliding-window supervisor doubles it quickly.
pub const CWND_INIT: usize = 2;

/// Minimum cwnd (never drop below 2 — cwnd=1 can't observe parallelism benefit).
pub const CWND_MIN: usize = 2;

/// Linear backoff step.  When congestion is detected, subtract this fixed
/// amount from cwnd instead of halving (multiplicative decrease).
/// Uses CWND_INIT as the natural unit of concurrency reduction.
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
//  SRTT-based congestion probe thresholds
// ============================================================================
//
// Design (user's insight):
//   After doubling cwnd, if latency < 2× previous latency, the new cwnd is valid.
//   This directly captures the parallelism-to-latency trade-off: "can the server
//   handle double the load without doubling latency?"
//
// Growth phase:
//   1. Record srtt_before at current cwnd.
//   2. Double cwnd, wait one PROBE_INTERVAL.
//   3. If srtt_now < srtt_before × SRTT_GROW_THRESHOLD → GROW, repeat.
//      Otherwise → rollback, enter steady.
//
// Steady phase:
//   - SRTT > baseline × SRTT_SHRINK_THRESHOLD → real congestion → SHRINK.
//   - SRTT < baseline × SRTT_RECOVER_THRESHOLD → headroom → GROW (+1/8).
//   - Otherwise → STEADY.

/// SRTT must stay below baseline × this factor for a cwnd doubling to be valid.
/// "Latency < 2× after doubling cwnd" = server can handle the extra parallelism.
pub const SRTT_GROW_THRESHOLD: f64 = 2.0;

/// SRTT above baseline × this factor indicates real congestion → shrink.
pub const SRTT_SHRINK_THRESHOLD: f64 = 3.0;

/// SRTT below baseline × this factor indicates headroom → can grow.
pub const SRTT_RECOVER_THRESHOLD: f64 = 0.85;

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
pub const REPORT_TIMEOUT: Duration = Duration::from_secs(15);

// ============================================================================
//  Sanity checks (compile-time)
// ============================================================================

#[allow(clippy::absurd_extreme_comparisons)]
const _: () = {
    assert!(CWND_INIT >= CWND_MIN);
    assert!(CWND_MIN >= 2);
    assert!(MAX_SAFETY_CWND > CWND_INIT);
    assert!(MAX_WORKER_THREADS > CWND_INIT);
    assert!(MAX_WORKER_THREADS <= 4096);
    assert!(MAX_SAFETY_CWND <= 65536);
    assert!(SRTT_GROW_THRESHOLD > 1.0);
    assert!(SRTT_SHRINK_THRESHOLD > SRTT_GROW_THRESHOLD);
    assert!(SRTT_RECOVER_THRESHOLD < 1.0);
};
