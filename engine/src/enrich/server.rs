//! llama-server process lifecycle. This file: context sizing (pure) now;
//! spawn/health/shutdown (Task 3) next.

/// Total `-c` value to pass llama-server so each of `parallel` slots gets at
/// least `per_slot` tokens. (llama-server divides total context across slots.)
pub fn total_context(parallel: usize, per_slot: usize) -> usize {
    parallel.max(1) * per_slot.max(1)
}

/// True if a request of `prompt_tokens` + `max_output` fits one slot.
pub fn fits_slot(per_slot: usize, prompt_tokens: usize, max_output: usize) -> bool {
    prompt_tokens + max_output <= per_slot
}

#[cfg(test)]
mod ctx_tests {
    use super::*;

    #[test]
    fn total_context_multiplies() {
        assert_eq!(total_context(4, 4096), 16384);
        assert_eq!(total_context(8, 2048), 16384);
        assert_eq!(total_context(0, 4096), 4096); // guards against 0
    }

    #[test]
    fn fits_slot_checks_budget() {
        assert!(fits_slot(4096, 1645, 256)); // our real prompt fits 4096
        assert!(!fits_slot(1024, 1645, 256)); // the bug case: 1645 > 1024
    }
}
