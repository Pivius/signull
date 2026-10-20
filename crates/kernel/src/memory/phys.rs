//! `kalloc`/`kfree`

use super::PAGE_SIZE;

pub struct PhysicalMemory {
    bytes: Vec<u8>,
    allocated: Vec<bool>,
    free_frames: usize,
}

impl PhysicalMemory {
    pub fn new(num_frames: usize) -> Self {
        Self {
            bytes: vec![0; num_frames * PAGE_SIZE],
            allocated: vec![false; num_frames],
            free_frames: num_frames,
        }
    }

    pub fn num_frames(&self) -> usize {
        self.allocated.len()
    }

    pub fn free_frames(&self) -> usize {
        self.free_frames
    }

    pub fn is_allocated(&self, frame: usize) -> bool {
        self.allocated.get(frame).copied().unwrap_or(false)
    }

    pub fn alloc(&mut self) -> Option<usize> {
        let frame = self.allocated.iter().position(|&used| !used)?;

        self.allocated[frame] = true;
        self.free_frames -= 1;
        Some(frame)
    }

    pub fn free(&mut self, frame: usize) {
        debug_assert!(self.allocated.get(frame).copied().unwrap_or(false));
        if let Some(slot) = self.allocated.get_mut(frame)
            && *slot
        {
            *slot = false;
            self.free_frames += 1;
        }
    }

    pub fn zero(&mut self, frame: usize) {
        let start = frame * PAGE_SIZE;

        self.bytes[start..start + PAGE_SIZE].fill(0);
    }

    pub fn read(&self, frame: usize, offset: usize, buf: &mut [u8]) {
        assert!(offset + buf.len() <= PAGE_SIZE, "frame read out of range");
        let start = frame * PAGE_SIZE + offset;

        buf.copy_from_slice(&self.bytes[start..start + buf.len()]);
    }

    pub fn write(&mut self, frame: usize, offset: usize, data: &[u8]) {
        assert!(offset + data.len() <= PAGE_SIZE, "frame write out of range");
        let start = frame * PAGE_SIZE + offset;

        self.bytes[start..start + data.len()].copy_from_slice(data);
    }

    pub fn frame_bytes(&self, frame: usize) -> &[u8] {
        let start = frame * PAGE_SIZE;
        &self.bytes[start..start + PAGE_SIZE]
    }

    pub fn frame_bytes_mut(&mut self, frame: usize) -> &mut [u8] {
        let start = frame * PAGE_SIZE;
        &mut self.bytes[start..start + PAGE_SIZE]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocate_and_free() {
        let mut mem = PhysicalMemory::new(4);
        assert_eq!(mem.free_frames(), 4);
        let a = mem.alloc().unwrap();
        let b = mem.alloc().unwrap();
        assert_ne!(a, b);
        assert!(mem.is_allocated(a));
        assert_eq!(mem.free_frames(), 2);
        mem.free(a);
        assert!(!mem.is_allocated(a));
        assert_eq!(mem.free_frames(), 3);
    }

    #[test]
    fn read_write_round_trip() {
        let mut mem = PhysicalMemory::new(1);
        mem.write(0, 0, &[1, 2, 3, 4]);
        let mut buf = [0u8; 4];
        mem.read(0, 0, &mut buf);
        assert_eq!(buf, [1, 2, 3, 4]);
    }

    #[test]
    fn exhaustion_returns_none() {
        let mut mem = PhysicalMemory::new(1);
        assert!(mem.alloc().is_some());
        assert!(mem.alloc().is_none());
    }
}
