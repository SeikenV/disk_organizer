//! Runtime backend selection for llama.cpp: prefer CUDA, then Vulkan, then CPU.
//! The probe is a trait so tests can simulate accelerators being unavailable.

/// A llama.cpp compute backend, in descending preference order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Cuda,
    Vulkan,
    Cpu,
}

impl Backend {
    /// Subdirectory under the tools dir holding this backend's `llama-server`.
    pub fn dir_name(self) -> &'static str {
        match self {
            Backend::Cuda => "cuda",
            Backend::Vulkan => "vulkan",
            Backend::Cpu => "cpu",
        }
    }
}

/// Detects which accelerators are usable on this machine. Real impl probes the
/// system; tests inject a mock.
pub trait AcceleratorProbe {
    fn cuda_available(&self) -> bool;
    fn vulkan_available(&self) -> bool;
}

/// Pick the first backend in `prefs` whose accelerator is available; CPU is the
/// always-available floor and is returned if nothing else qualifies.
pub fn select_backend(probe: &dyn AcceleratorProbe, prefs: &[Backend]) -> Backend {
    for &b in prefs {
        let ok = match b {
            Backend::Cuda => probe.cuda_available(),
            Backend::Vulkan => probe.vulkan_available(),
            Backend::Cpu => true,
        };
        if ok {
            return b;
        }
    }
    Backend::Cpu
}

/// Default preference order.
pub fn default_prefs() -> Vec<Backend> {
    vec![Backend::Cuda, Backend::Vulkan, Backend::Cpu]
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Mock {
        cuda: bool,
        vulkan: bool,
    }
    impl AcceleratorProbe for Mock {
        fn cuda_available(&self) -> bool { self.cuda }
        fn vulkan_available(&self) -> bool { self.vulkan }
    }

    #[test]
    fn prefers_cuda_when_available() {
        let p = Mock { cuda: true, vulkan: true };
        assert_eq!(select_backend(&p, &default_prefs()), Backend::Cuda);
    }

    #[test]
    fn falls_back_to_vulkan_when_no_cuda() {
        let p = Mock { cuda: false, vulkan: true };
        assert_eq!(select_backend(&p, &default_prefs()), Backend::Vulkan);
    }

    #[test]
    fn falls_back_to_cpu_when_no_accelerator() {
        // The required case: simulate CUDA and Vulkan unavailable.
        let p = Mock { cuda: false, vulkan: false };
        assert_eq!(select_backend(&p, &default_prefs()), Backend::Cpu);
    }

    #[test]
    fn respects_custom_prefs_order() {
        let p = Mock { cuda: true, vulkan: true };
        assert_eq!(select_backend(&p, &[Backend::Vulkan, Backend::Cuda]), Backend::Vulkan);
        assert_eq!(select_backend(&p, &[Backend::Cpu]), Backend::Cpu);
    }
}
