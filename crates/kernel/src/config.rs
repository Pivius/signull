//! Kernel configurations

use crate::{hardware::Hardware, sched::SchedPolicy};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FeatureFlags {
    pub multicore: bool,
    pub preemtion: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KernelConfig {
    pub scheduler: SchedPolicy,
    pub workers: usize,
    pub quantum: u64,
    pub features: FeatureFlags,
}

impl KernelConfig {
    pub fn new(quantum: u64) -> Self {
        Self {
            scheduler: SchedPolicy::Fifo,
            workers: 1,
            quantum,
            features: FeatureFlags::default(),
        }
    }

    pub fn validate(&self, hw: &Hardware) -> KernelConfig {
        let mut cfg = *self;
        let allowed = if cfg.features.multicore {
            hw.cores.max(1)
        } else {
            1
        };

        cfg.workers = cfg.workers.clamp(1, allowed);
        cfg
    }
}

impl Default for KernelConfig {
    fn default() -> Self {
        Self::new(1000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hardware_without_mc() {
        let hw = Hardware {
            cores: 4,
            ..Hardware::default()
        };
        let cfg = KernelConfig::new(100).validate(&hw);
        assert_eq!(cfg.workers, 1, "4 cores does not imply 4 workers");
    }

    #[test]
    fn hardware_with_mc() {
        let hw = Hardware {
            cores: 4,
            ..Hardware::default()
        };
        let mut cfg = KernelConfig::new(100);
        cfg.features.multicore = true;
        cfg.workers = 3;
        assert_eq!(cfg.validate(&hw).workers, 3);

        cfg.workers = 99;
        assert_eq!(cfg.validate(&hw).workers, 4, "clamped to cores");
    }

    #[test]
    fn atleast_one_worker() {
        let hw = Hardware::single_core(64);
        let mut cfg = KernelConfig::new(10);

        cfg.features.multicore = true;
        cfg.workers = 0;

        assert_eq!(cfg.validate(&hw).workers, 1);
    }
}
