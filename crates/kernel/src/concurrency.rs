#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Workers {
    count: usize,
}

impl Workers {
    pub fn new(count: usize) -> Self {
        Self {
            count: count.max(1),
        }
    }

    pub fn count(&self) -> usize {
        self.count
    }

    pub fn is_multicore(&self) -> bool {
        self.count > 1
    }
}

impl Default for Workers {
    fn default() -> Self {
        Self::new(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_workers() {
        assert_eq!(Workers::new(0).count(), 1);
    }

    #[test]
    fn multicore() {
        assert!(Workers::new(4).is_multicore());
        assert!(!Workers::default().is_multicore());
    }
}
