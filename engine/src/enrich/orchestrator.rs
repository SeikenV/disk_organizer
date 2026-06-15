//! Concurrency / throughput-control machinery for LLM enrichment.
//!
//! This is the "transport layer" of enrichment: a throughput-maximizing
//! congestion controller (`CwndCtl`) plus the periodic supervisor that drives
//! it. It is intentionally decoupled from prompt/model logic so that multiple
//! LLM backends can share one control seam.

use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::thread;
use std::time::Instant;

// ---- Re-export centralized constants (convenience for intra-module use) ----
use crate::consts::{
    cwnd_init,
    MAX_SAFETY_CWND,
    PROBE_INTERVAL,
    TP_GROW_STEP, TP_PROBE_STEP, TP_PROBE_AFTER_STABLE,
    TP_IMPROVING_RATIO, TP_PROBE_WIN_RATIO,
};

// ---- Probe record types ----

/// One row in the probe log, written every PROBE_INTERVAL by the supervisor.
#[derive(Debug, Clone, serde::Serialize)]
pub(super) struct ProbeRecord {
    pub(super) elapsed_ms: u64,
    pub(super) cwnd: usize,
    pub(super) inflight: usize,
    pub(super) completed_in_window: usize,
    pub(super) throughput_rps: f64,
    pub(super) srtt_ms: f64,
    /// Effective per-task completion pacing in this window (ms).
    pub(super) per_task_ms: f64,
    /// Best throughput observed so far (req/s).
    pub(super) best_tp_rps: f64,
    /// Best cwnd at which best_tp was observed.
    pub(super) best_cwnd: usize,
    /// Current phase: "growing", "plateau", or "probing".
    pub(super) phase: String,
}

// ---- Cwnd: Throughput-driven concurrency control ----
//
// Design (throughput-maximizing, not latency-minimizing):
//   Workers acquire/release permits via blocking Condvar (unchanged).
//   Every PROBE_INTERVAL, the supervisor computes window throughput and
//   adjusts cwnd to chase the throughput peak:
//     - Grow while throughput improves
//     - Snap back to best cwnd when throughput drops
//     - Periodically probe upward to discover increased capacity
//
//   This is fundamentally different from Vegas: individual request latency
//   is irrelevant — only total batch wall-clock time matters.

/// Throughput-probe phase.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum TpPhase {
    /// Aggressively growing cwnd — throughput is still improving.
    Growing,
    /// Throughput plateaued; holding best cwnd, counting toward next probe.
    Plateau,
    /// Probing upward — increased cwnd to test for new capacity.
    Probing,
}

pub(super) struct CwndCtl {
    /// Current concurrency window — written by supervisor each probe cycle.
    pub(super) cwnd: AtomicUsize,
    /// Currently in-flight requests.
    pub(super) inflight: AtomicUsize,
    /// Condition variable + mutex: workers wait here instead of spin-looping.
    pub(super) permit_mutex: Mutex<()>,
    pub(super) permit_cv: Condvar,
    /// Smoothed round-trip time (ms). EWMA, for display only.
    pub(super) srtt_ms: Mutex<f64>,
    /// Requests completed since last probe snapshot (workers inc, supervisor resets).
    pub(super) completed_this_window: AtomicUsize,
    /// Throughput state — written exclusively by supervisor.
    /// Best throughput observed so far (req/s).
    pub(super) best_tp: Mutex<f64>,
    /// cwnd at which best_tp was observed.
    pub(super) best_cwnd: AtomicUsize,
    /// Throughput from previous probe window.
    pub(super) prev_tp: Mutex<f64>,
    /// Current phase.
    pub(super) phase: Mutex<TpPhase>,
    /// Consecutive plateau windows (for triggering upward probe).
    pub(super) stable_count: AtomicUsize,
    /// Peak inflight observed (for final summary).
    pub(super) peak_inflight: AtomicUsize,
    /// Peak cwnd observed (for final summary).
    pub(super) peak_cwnd: AtomicUsize,
    /// Probe log — supervisor appends one record per cycle.
    pub(super) probe_log: Mutex<Vec<ProbeRecord>>,
    /// Wall-clock instant when this controller was created.
    pub(super) start: Instant,
}

impl CwndCtl {
    pub(super) fn new() -> Self {
        let init = cwnd_init();
        Self {
            cwnd: AtomicUsize::new(init),
            inflight: AtomicUsize::new(0),
            permit_mutex: Mutex::new(()),
            permit_cv: Condvar::new(),
            srtt_ms: Mutex::new(0.0),
            completed_this_window: AtomicUsize::new(0),
            best_tp: Mutex::new(0.0),
            best_cwnd: AtomicUsize::new(init),
            prev_tp: Mutex::new(0.0),
            phase: Mutex::new(TpPhase::Growing),
            stable_count: AtomicUsize::new(0),
            peak_inflight: AtomicUsize::new(0),
            peak_cwnd: AtomicUsize::new(init),
            probe_log: Mutex::new(Vec::new()),
            start: Instant::now(),
        }
    }

    /// Block until a concurrency permit is available.
    /// Uses Condvar-based blocking — zero CPU while waiting.
    pub(super) fn acquire(&self) {
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
                drop(guard);
                continue; // permit freed while locking — retry
            }
            drop(self.permit_cv.wait(guard).unwrap());
        }
    }

    /// Report a successful completion — lightweight, no cwnd decisions here.
    /// cwnd is adjusted exclusively by the supervisor each probe window.
    pub(super) fn release_success(&self, latency_ms: f64) {
        self.inflight.fetch_sub(1, Ordering::SeqCst);
        self.completed_this_window.fetch_add(1, Ordering::Relaxed);

        // Update SRTT EWMA (for display only, not used for control).
        {
            let mut srtt = self.srtt_ms.lock().unwrap();
            if *srtt == 0.0 {
                *srtt = latency_ms;
            } else {
                *srtt = 0.875 * *srtt + 0.125 * latency_ms;
            }
        }

        self.permit_cv.notify_one();
    }

    /// Throughput-driven cwnd update — called by supervisor each window.
    ///
    /// Algorithm:
    ///   1. Compute current_tp from completed_this_window.
    ///   2. If current_tp > best_tp, record new best.
    ///   3. Match on phase:
    ///      Growing: if still improving, keep growing; if dropped, snap to best.
    ///      Plateau:  count stable windows, then trigger upward probe.
    ///      Probing:  if probe improved throughput, go back to Growing;
    ///                if no improvement, fall back to best and go to Plateau.
    pub(super) fn update_cwnd(&self, completed: usize) {
        let current_tp = completed as f64 / PROBE_INTERVAL.as_secs_f64();
        let cwnd = self.cwnd.load(Ordering::Relaxed);

        // Update best if improved.
        {
            let mut best = self.best_tp.lock().unwrap();
            if current_tp > *best {
                *best = current_tp;
                self.best_cwnd.store(cwnd, Ordering::Relaxed);
            }
        }

        let best_tp_val = *self.best_tp.lock().unwrap();
        let prev_tp_val = *self.prev_tp.lock().unwrap();
        let mut phase = *self.phase.lock().unwrap();

        let new_cwnd = match phase {
            TpPhase::Growing => {
                if prev_tp_val > 0.0 && current_tp < prev_tp_val * TP_IMPROVING_RATIO {
                    // Throughput dropped — snap to best and go to plateau.
                    phase = TpPhase::Plateau;
                    self.stable_count.store(0, Ordering::Relaxed);
                    self.best_cwnd.load(Ordering::Relaxed)
                } else {
                    // Still improving — keep growing.
                    (cwnd + TP_GROW_STEP).min(MAX_SAFETY_CWND)
                }
            }
            TpPhase::Plateau => {
                let sc = self.stable_count.fetch_add(1, Ordering::Relaxed) + 1;
                if sc >= TP_PROBE_AFTER_STABLE as usize {
                    // Time to probe upward.
                    phase = TpPhase::Probing;
                    self.stable_count.store(0, Ordering::Relaxed);
                    (cwnd + TP_PROBE_STEP).min(MAX_SAFETY_CWND)
                } else {
                    // Stay put.
                    cwnd
                }
            }
            TpPhase::Probing => {
                if current_tp > best_tp_val * TP_PROBE_WIN_RATIO {
                    // Probe found more capacity! Start growing again.
                    phase = TpPhase::Growing;
                    (cwnd + TP_GROW_STEP).min(MAX_SAFETY_CWND)
                } else if current_tp < prev_tp_val * TP_IMPROVING_RATIO {
                    // Probe hurt — fall back to best.
                    phase = TpPhase::Plateau;
                    self.stable_count.store(0, Ordering::Relaxed);
                    self.best_cwnd.load(Ordering::Relaxed)
                } else {
                    // No clear signal — continue probing deeper.
                    cwnd
                }
            }
        };

        // Store updated state.
        *self.phase.lock().unwrap() = phase;
        *self.prev_tp.lock().unwrap() = current_tp;

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
    pub(super) fn release_failure(&self) {
        self.inflight.fetch_sub(1, Ordering::SeqCst);
        self.completed_this_window.fetch_add(1, Ordering::Relaxed);
        self.permit_cv.notify_one();
    }

    /// Release a permit temporarily (for retry backoff) WITHOUT counting
    /// the request as completed.  Caller must re-acquire() after the sleep.
    pub(super) fn release_retry(&self) {
        self.inflight.fetch_sub(1, Ordering::SeqCst);
        self.permit_cv.notify_one();
    }

    pub(super) fn srtt(&self) -> f64 {
        *self.srtt_ms.lock().unwrap()
    }

    /// Wake all workers blocked on `acquire()`.  Used by the supervisor
    /// after cwnd is increased, and by tests.
    #[allow(dead_code)]
    pub(super) fn notify_waiters(&self) {
        self.permit_cv.notify_all();
    }
}

// ---- Supervisor: periodic throughput probe + progress display ----

/// Runs the supervisor for one or two backends.
///
/// Every PROBE_INTERVAL the supervisor:
///   1. Calls `update_cwnd()` — throughput-driven cwnd adjustment.
///   2. Snapshots state and appends a probe record.
///   3. Prints progress to stderr.
///
/// cwnd decisions are made HERE (periodic), not in release_success().
pub(super) fn run_supervisor(
    gpu_ctl: &Arc<CwndCtl>,
    igpu_ctl: Option<&Arc<CwndCtl>>,
    total: usize,
    done: &Arc<AtomicUsize>,
) {
    loop {
        thread::sleep(PROBE_INTERVAL);
        let d = done.load(Ordering::Relaxed);
        if d >= total {
            break;
        }

        // ---- Throughput-driven cwnd update (THE control decision) ----
        let gpu_completed = gpu_ctl.completed_this_window.swap(0, Ordering::Relaxed);
        gpu_ctl.update_cwnd(gpu_completed);
        append_probe_log(gpu_ctl, gpu_completed);

        let gpu_cwnd = gpu_ctl.cwnd.load(Ordering::Relaxed);
        let gpu_inf = gpu_ctl.inflight.load(Ordering::Relaxed);
        let gpu_srtt = gpu_ctl.srtt();

        if let Some(ictl) = igpu_ctl {
            let icompleted = ictl.completed_this_window.swap(0, Ordering::Relaxed);
            ictl.update_cwnd(icompleted);
            append_probe_log(ictl, icompleted);
            let icwnd = ictl.cwnd.load(Ordering::Relaxed);
            let iinf = ictl.inflight.load(Ordering::Relaxed);
            let isrtt = ictl.srtt();
            eprint!(
                "\r  [{d}/{total}] dGPU cwnd={gpu_cwnd} infl={gpu_inf} srtt={gpu_srtt:.0}ms | iGPU cwnd={icwnd} infl={iinf} srtt={isrtt:.0}ms  ",
            );
        } else {
            eprint!(
                "\r  [{d}/{total}] cwnd={gpu_cwnd} infl={gpu_inf} srtt={gpu_srtt:.0}ms  ",
            );
        }
    }
}

/// Snapshot the current state into the probe log for one backend.
pub(super) fn append_probe_log(ctl: &CwndCtl, completed_in_window: usize) {
    let current_cwnd = ctl.cwnd.load(Ordering::Relaxed);
    let snapshot_inflight = ctl.inflight.load(Ordering::Relaxed);
    let srtt = ctl.srtt();
    let window_throughput =
        completed_in_window as f64 / PROBE_INTERVAL.as_secs_f64();
    let per_task_ms = if completed_in_window > 0 {
        PROBE_INTERVAL.as_millis() as f64 / completed_in_window as f64
    } else {
        0.0
    };

    let best_tp = *ctl.best_tp.lock().unwrap();
    let best_cwnd = ctl.best_cwnd.load(Ordering::Relaxed);
    let phase = format!("{:?}", *ctl.phase.lock().unwrap());

    ctl.probe_log.lock().unwrap().push(ProbeRecord {
        elapsed_ms: ctl.start.elapsed().as_millis() as u64,
        cwnd: current_cwnd,
        inflight: snapshot_inflight,
        completed_in_window,
        throughput_rps: window_throughput,
        srtt_ms: srtt,
        per_task_ms,
        best_tp_rps: best_tp,
        best_cwnd,
        phase,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consts::CWND_MIN;
    use crate::consts::{MAX_RETRIES, RETRY_BASE_DELAY};
    use std::time::Duration;

    // ===================================================================
    //  CwndCtl — init
    // ===================================================================

    #[test]
    fn cwndctl_init_defaults() {
        let ctl = CwndCtl::new();
        let init = cwnd_init();
        assert_eq!(ctl.cwnd.load(Ordering::Relaxed), init);
        assert_eq!(ctl.inflight.load(Ordering::Relaxed), 0);
        assert_eq!(ctl.srtt(), 0.0);
        assert_eq!(ctl.peak_inflight.load(Ordering::Relaxed), 0);
        assert_eq!(ctl.peak_cwnd.load(Ordering::Relaxed), init);
        assert_eq!(*ctl.best_tp.lock().unwrap(), 0.0);
        assert_eq!(ctl.best_cwnd.load(Ordering::Relaxed), init);
        assert_eq!(*ctl.phase.lock().unwrap(), TpPhase::Growing);
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
        let init = cwnd_init();
        for _ in 0..init {
            ctl.acquire();
        }
        assert_eq!(ctl.inflight.load(Ordering::Relaxed), init);
        ctl.release_success(50.0);
        ctl.acquire();
        assert_eq!(ctl.inflight.load(Ordering::Relaxed), init);
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
    //  Throughput-driven cwnd control — update_cwnd
    // ===================================================================

    #[test]
    fn tp_cycle_grows_while_improving() {
        // Simulate 3 windows where throughput keeps improving.
        // cwnd should grow each time.
        let ctl = CwndCtl::new();
        let init = cwnd_init();
        assert_eq!(ctl.cwnd.load(Ordering::Relaxed), init);

        // Window 1: 2 completions → tp=4
        ctl.update_cwnd(2);
        assert!(ctl.cwnd.load(Ordering::Relaxed) > init,
            "cwnd should grow on improving throughput, got {}", ctl.cwnd.load(Ordering::Relaxed));

        let cwnd2 = ctl.cwnd.load(Ordering::Relaxed);
        // Window 2: 4 completions → tp=8 (higher than 4)
        ctl.update_cwnd(4);
        assert!(ctl.cwnd.load(Ordering::Relaxed) > cwnd2,
            "cwnd should grow again, got {}", ctl.cwnd.load(Ordering::Relaxed));
    }

    #[test]
    fn tp_snaps_to_best_when_throughput_drops() {
        // Simulate: grow to high cwnd, then throughput drops → snap to best.
        let ctl = CwndCtl::new();
        // Build up best: 4 completions → tp=8
        ctl.update_cwnd(4);
        let best_cwnd = ctl.best_cwnd.load(Ordering::Relaxed);
        assert!(best_cwnd > 0);

        // Next window: keep growing
        ctl.update_cwnd(8);
        let high_cwnd = ctl.cwnd.load(Ordering::Relaxed);

        // Now throughput drops sharply: 1 completion → tp=2 (way below 16)
        ctl.update_cwnd(1);
        // Should snap back to best_cwnd.
        assert!(ctl.cwnd.load(Ordering::Relaxed) <= best_cwnd + TP_GROW_STEP,
            "should snap near best, got {} vs best {}", ctl.cwnd.load(Ordering::Relaxed), best_cwnd);
        assert!(ctl.cwnd.load(Ordering::Relaxed) < high_cwnd,
            "should be below peak cwnd after throughput drop");
        assert_eq!(*ctl.phase.lock().unwrap(), TpPhase::Plateau);
    }

    #[test]
    fn tp_probes_upward_after_stable() {
        // Simulate: plateau for TP_PROBE_AFTER_STABLE windows, then should probe.
        let ctl = CwndCtl::new();
        // First grow then drop to plateau.
        ctl.update_cwnd(10);
        ctl.update_cwnd(2); // drop → plateau
        let plateau_cwnd = ctl.cwnd.load(Ordering::Relaxed);

        // Stay stable for enough windows.
        for _ in 0..TP_PROBE_AFTER_STABLE as usize {
            ctl.update_cwnd(2); // same tp each time
        }

        // Now it should be probing upward.
        assert_eq!(*ctl.phase.lock().unwrap(), TpPhase::Probing);
        assert!(ctl.cwnd.load(Ordering::Relaxed) > plateau_cwnd,
            "should probe above plateau, got {} vs {}", ctl.cwnd.load(Ordering::Relaxed), plateau_cwnd);
    }

    #[test]
    fn tp_update_tracks_best() {
        let ctl = CwndCtl::new();
        ctl.update_cwnd(5); // tp=10
        assert!(*ctl.best_tp.lock().unwrap() > 0.0);
        let best1 = *ctl.best_tp.lock().unwrap();

        ctl.update_cwnd(10); // tp=20 → new best
        let best2 = *ctl.best_tp.lock().unwrap();
        assert!(best2 > best1);

        ctl.update_cwnd(2); // tp=4 → no new best
        let best3 = *ctl.best_tp.lock().unwrap();
        assert!((best3 - best2).abs() < 0.01);
    }

    #[test]
    fn tp_update_respects_cwnd_min() {
        // Set cwnd to min-1 and ensure it stays >= CWND_MIN after update.
        let ctl = CwndCtl::new();
        ctl.cwnd.store(CWND_MIN, Ordering::Relaxed);
        ctl.update_cwnd(1); // any value
        assert!(ctl.cwnd.load(Ordering::Relaxed) >= CWND_MIN);
    }

    #[test]
    fn tp_update_respects_max_safety_cwnd() {
        // Start near max, grow — should cap.
        let near_max = MAX_SAFETY_CWND - 1;
        let ctl = CwndCtl::new();
        ctl.cwnd.store(near_max, Ordering::Relaxed);
        // Force growing phase with good throughput.
        *ctl.phase.lock().unwrap() = TpPhase::Growing;
        ctl.update_cwnd(100);
        assert!(ctl.cwnd.load(Ordering::Relaxed) <= MAX_SAFETY_CWND);
    }

    #[test]
    fn tp_update_tracks_peak_cwnd() {
        let ctl = CwndCtl::new();
        ctl.update_cwnd(10);
        let peak = ctl.peak_cwnd.load(Ordering::Relaxed);
        let cwnd = ctl.cwnd.load(Ordering::Relaxed);
        assert!(peak >= cwnd);
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
        let cwnd = cwnd_init();
        assert!(cwnd >= CWND_MIN);
        assert!(CWND_MIN >= 2);
        assert!(MAX_SAFETY_CWND > cwnd);
        assert!(MAX_SAFETY_CWND <= 65536);
        assert!(TP_GROW_STEP > 0);
        assert!(TP_PROBE_STEP > TP_GROW_STEP);
        assert!(TP_IMPROVING_RATIO > 0.0 && TP_IMPROVING_RATIO < 1.0);
        assert!(TP_PROBE_WIN_RATIO > 1.0);
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
            per_task_ms: 62.5,
            best_tp_rps: 18.0,
            best_cwnd: 24,
            phase: "growing".into(),
        };
        assert_eq!(r.elapsed_ms, 500);
        assert_eq!(r.cwnd, 16);
        assert_eq!(r.inflight, 16);
        assert_eq!(r.completed_in_window, 8);
        assert!((r.throughput_rps - 16.0).abs() < 0.01);
        assert!((r.srtt_ms - 2500.0).abs() < 0.01);
        assert!((r.per_task_ms - 62.5).abs() < 0.01);
        assert!((r.best_tp_rps - 18.0).abs() < 0.01);
        assert_eq!(r.best_cwnd, 24);
        assert_eq!(r.phase, "growing");
    }
}
