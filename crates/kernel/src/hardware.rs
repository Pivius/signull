//! Simulated hardware.
//!

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hardware {
    pub cores: usize,
    pub ram_frames: usize,
    pub disk_blocks: usize,
    pub disk_block_size: usize,
    pub nic_ports: usize,
}

impl Hardware {
    pub fn single_core(ram_frames: usize) -> Self {
        Self {
            cores: 1,
            ram_frames,
            disk_blocks: 2048,
            disk_block_size: 512,
            nic_ports: 1,
        }
    }

    pub fn ram_kib(&self) -> usize {
        self.ram_frames * crate::memory::PAGE_SIZE / 1024
    }

    pub fn is_multicore(&self) -> bool {
        self.cores > 1
    }
}

impl Default for Hardware {
    fn default() -> Self {
        Self::single_core(1024)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_core() {
        let hw = Hardware::single_core(256);
        assert_eq!(hw.cores, 1);
        assert!(!hw.is_multicore());
    }
}
