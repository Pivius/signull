use crate::memory::{PAGE_SIZE, PTE_COUNT, phys::PhysicalMemory};

pub const PTE_V: u64 = 1 << 0; // valid
pub const PTE_R: u64 = 1 << 1; // readable
pub const PTE_W: u64 = 1 << 2; // writable
pub const PTE_X: u64 = 1 << 3; // executable
pub const PTE_U: u64 = 1 << 4; // accessible in user mode

const PTE_FLAGS_MASK: u64 = 0x1ff;
const PTE_ADDR_MASK: u64 = !(PAGE_SIZE as u64 - 1);

pub const VA_LIMIT: usize = 1 << 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapError {
    OutOfMemory,
    AlreadyMapped,
    InvalidAddress,
}

#[derive(Clone, Debug)]
pub struct PageTable {
    root: usize,
}

impl PageTable {
    pub fn new(mem: &mut PhysicalMemory) -> Option<Self> {
        let root = mem.alloc()?;

        mem.zero(root);
        Some(Self { root })
    }

    /// satp
    pub fn root(&self) -> usize {
        self.root
    }

    fn l0_index(va: usize) -> usize {
        (va >> 12) & (PTE_COUNT - 1)
    }

    fn l1_index(va: usize) -> usize {
        (va >> 21) & (PTE_COUNT - 1)
    }

    fn read_pte(mem: &PhysicalMemory, table: usize, index: usize) -> u64 {
        let mut buf = [0u8; 8];

        mem.read(table, index * 8, &mut buf);
        u64::from_le_bytes(buf)
    }

    fn write_pte(mem: &mut PhysicalMemory, table: usize, index: usize, pte: u64) {
        mem.write(table, index * 8, &pte.to_le_bytes());
    }

    fn child_table_read(mem: &PhysicalMemory, table: usize, index: usize) -> Option<usize> {
        let pte = Self::read_pte(mem, table, index);

        if pte & PTE_V == 0 || pte & (PTE_R | PTE_W | PTE_X) != 0 {
            return None;
        }
        Some(((pte & PTE_ADDR_MASK) >> 12) as usize)
    }

    fn walk_alloc(&self, mem: &mut PhysicalMemory, va: usize) -> Option<usize> {
        let index = Self::l1_index(va);
        let pte = Self::read_pte(mem, self.root, index);

        if pte & PTE_V != 0 {
            Some(((pte & PTE_ADDR_MASK) >> 12) as usize)
        } else {
            let new = mem.alloc()?;

            mem.zero(new);
            Self::write_pte(mem, self.root, index, ((new as u64) << 12) | PTE_V);
            Some(new)
        }
    }

    fn walk_read(&self, mem: &PhysicalMemory, va: usize) -> Option<usize> {
        Self::child_table_read(mem, self.root, Self::l1_index(va))
    }

    // Map virtual address to frame with flags
    pub fn map(
        &self,
        mem: &mut PhysicalMemory,
        va: usize,
        frame: usize,
        flags: u64,
    ) -> Result<(), MapError> {
        if !va.is_multiple_of(PAGE_SIZE) || va >= VA_LIMIT || frame >= mem.num_frames() {
            return Err(MapError::InvalidAddress);
        }

        let table = self.walk_alloc(mem, va).ok_or(MapError::OutOfMemory)?;
        let index = Self::l0_index(va);

        if Self::read_pte(mem, table, index) & PTE_V != 0 {
            return Err(MapError::AlreadyMapped);
        }

        Self::write_pte(mem, table, index, ((frame as u64) << 12) | flags | PTE_V);
        Ok(())
    }

    /// Remove mapping. Returns the frame it pointed at.
    pub fn unmap(&self, mem: &mut PhysicalMemory, va: usize) -> Option<usize> {
        let table = self.walk_read(mem, va)?;
        let index = Self::l0_index(va);
        let pte = Self::read_pte(mem, table, index);

        if pte & PTE_V == 0 {
            return None;
        }

        Self::write_pte(mem, table, index, 0);
        Some(((pte & PTE_ADDR_MASK) >> 12) as usize)
    }

    /// Translate `va` to `(frame, offset, flags)`.
    pub fn translate(&self, mem: &PhysicalMemory, va: usize) -> Option<(usize, usize, u64)> {
        let table = self.walk_read(mem, va)?;
        let index = Self::l0_index(va);
        let pte = Self::read_pte(mem, table, index);
        if pte & PTE_V == 0 {
            return None;
        }
        Some((
            ((pte & PTE_ADDR_MASK) >> 12) as usize,
            va & (PAGE_SIZE - 1),
            pte & PTE_FLAGS_MASK,
        ))
    }

    /// Deep-copy every mapping into freshly allocated frames reachable from `dst`
    pub fn copy_into(&self, mem: &mut PhysicalMemory, dst: &PageTable) -> Result<(), MapError> {
        copy_level(mem, self.root, dst.root, 1)
    }

    pub fn free(&self, mem: &mut PhysicalMemory) {
        free_level(mem, self.root, 1);
    }
}

fn copy_level(
    mem: &mut PhysicalMemory,
    src_table: usize,
    dst_table: usize,
    level: u8,
) -> Result<(), MapError> {
    for index in 0..PTE_COUNT {
        let pte = PageTable::read_pte(mem, src_table, index);

        if pte & PTE_V == 0 {
            continue;
        }

        let child = ((pte & PTE_ADDR_MASK) >> 12) as usize;
        if level == 0 {
            let new = mem.alloc().ok_or(MapError::OutOfMemory)?;
            let page = mem.frame_bytes(child).to_vec();

            mem.frame_bytes_mut(new).copy_from_slice(&page);
            PageTable::write_pte(
                mem,
                dst_table,
                index,
                ((new as u64) << 12) | (pte & PTE_FLAGS_MASK),
            );
        } else {
            let new = mem.alloc().ok_or(MapError::OutOfMemory)?;

            mem.zero(new);
            PageTable::write_pte(mem, dst_table, index, ((new as u64) << 12) | PTE_V);
            copy_level(mem, child, new, level - 1)?;
        }
    }
    Ok(())
}

fn free_level(mem: &mut PhysicalMemory, table: usize, level: u8) {
    for index in 0..PTE_COUNT {
        let pte = PageTable::read_pte(mem, table, index);

        if pte & PTE_V == 0 {
            continue;
        }

        let child = ((pte & PTE_ADDR_MASK) >> 12) as usize;
        if level == 0 {
            mem.free(child);
        } else {
            free_level(mem, child, level - 1);
        }
    }
    mem.free(table);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_translate_unmap() {
        let mut mem = PhysicalMemory::new(16);
        let pt = PageTable::new(&mut mem).unwrap();
        let frame = mem.alloc().unwrap();

        pt.map(&mut mem, 0x1000, frame, PTE_R | PTE_W | PTE_U)
            .unwrap();

        let (f, off, flags) = pt.translate(&mem, 0x1234).unwrap();
        assert_eq!(f, frame);
        assert_eq!(off, 0x234);
        assert_eq!(flags & PTE_W, PTE_W);

        let removed = pt.unmap(&mut mem, 0x1000).unwrap();
        assert_eq!(removed, frame);
        assert!(pt.translate(&mem, 0x1000).is_none());
    }

    #[test]
    fn double_map_is_rejected() {
        let mut mem = PhysicalMemory::new(16);
        let pt = PageTable::new(&mut mem).unwrap();
        let a = mem.alloc().unwrap();
        let b = mem.alloc().unwrap();

        pt.map(&mut mem, 0x2000, a, PTE_R).unwrap();
        assert_eq!(
            pt.map(&mut mem, 0x2000, b, PTE_R),
            Err(MapError::AlreadyMapped)
        );
    }

    #[test]
    fn copy_is_independent() {
        let mut mem = PhysicalMemory::new(32);
        let src = PageTable::new(&mut mem).unwrap();
        let frame = mem.alloc().unwrap();
        mem.write(frame, 0, &[9, 9, 9, 9]);
        src.map(&mut mem, 0x5000, frame, PTE_R | PTE_W).unwrap();

        let dst = PageTable::new(&mut mem).unwrap();
        src.copy_into(&mut mem, &dst).unwrap();

        let (dframe, _, _) = dst.translate(&mem, 0x5000).unwrap();
        assert_ne!(dframe, frame);
        let mut buf = [0u8; 4];
        mem.read(dframe, 0, &mut buf);
        assert_eq!(buf, [9, 9, 9, 9]);

        // Mutating the copy must not affect the original.
        mem.write(dframe, 0, &[1, 1, 1, 1]);
        let mut orig = [0u8; 4];
        mem.read(frame, 0, &mut orig);
        assert_eq!(orig, [9, 9, 9, 9]);
    }

    #[test]
    fn freeing_returns_all_frames() {
        let mut mem = PhysicalMemory::new(32);
        let before = mem.free_frames();
        let pt = PageTable::new(&mut mem).unwrap();

        for i in 0..4 {
            let frame = mem.alloc().unwrap();
            pt.map(&mut mem, i * PAGE_SIZE, frame, PTE_R).unwrap();
        }
        pt.free(&mut mem);
        assert_eq!(mem.free_frames(), before);
    }
}
