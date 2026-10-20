//! Process address space

use crate::memory::{
    PAGE_SIZE, page_round_up,
    pagetable::{MapError, PTE_R, PTE_U, PTE_W, PageTable},
    phys::PhysicalMemory,
};

/// Top of the user address space
pub const USER_TOP: usize = 1 << 29;
/// Address where the heap begins
pub const HEAP_BASE: usize = 0x0040_0000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryError {
    Unmapped,
    NotWritable,
    OutOfMemory,
    AlradyMapped,
}

impl From<MapError> for MemoryError {
    fn from(err: MapError) -> Self {
        match err {
            MapError::OutOfMemory => MemoryError::OutOfMemory,
            MapError::AlreadyMapped => MemoryError::AlradyMapped,
            MapError::InvalidAddress => MemoryError::Unmapped,
        }
    }
}

pub struct AddressSpace {
    pt: PageTable,
    brk: usize,
}

impl AddressSpace {
    pub fn new(mem: &mut PhysicalMemory) -> Option<Self> {
        Some(Self {
            pt: PageTable::new(mem)?,
            brk: HEAP_BASE,
        })
    }

    pub fn root(&self) -> usize {
        self.pt.root()
    }

    pub fn brk(&self) -> usize {
        self.brk
    }

    pub fn alloc_and_map(
        &mut self,
        mem: &mut PhysicalMemory,
        va: usize,
        flags: u64,
    ) -> Result<usize, MemoryError> {
        let frame = mem.alloc().ok_or(MemoryError::OutOfMemory)?;

        mem.zero(frame);
        self.pt
            .map(mem, va, frame, flags)
            .map_err(MemoryError::from)?;
        Ok(frame)
    }

    pub fn map_existing(
        &mut self,
        mem: &mut PhysicalMemory,
        va: usize,
        frame: usize,
        flags: u64,
    ) -> Result<(), MemoryError> {
        self.pt
            .map(mem, va, frame, flags)
            .map_err(MemoryError::from)
    }

    pub fn translate(&self, mem: &PhysicalMemory, va: usize) -> Option<(usize, usize, u64)> {
        self.pt.translate(mem, va)
    }

    /// Copy `buf.len()` bytes out of physical memory.
    pub fn read(&self, mem: &PhysicalMemory, va: usize, buf: &mut [u8]) -> Result<(), MemoryError> {
        let mut done = 0;

        while done < buf.len() {
            let (frame, offset, _) = self
                .pt
                .translate(mem, va + done)
                .ok_or(MemoryError::Unmapped)?;
            let n = (PAGE_SIZE - offset).min(buf.len() - done);

            mem.read(frame, offset, &mut buf[done..done + n]);
            done += n;
        }
        Ok(())
    }

    /// Copy `data` into physical memory.
    pub fn write(
        &mut self,
        mem: &mut PhysicalMemory,
        va: usize,
        data: &[u8],
    ) -> Result<(), MemoryError> {
        let mut done = 0;

        while done < data.len() {
            let (frame, offset, flags) = self
                .pt
                .translate(mem, va + done)
                .ok_or(MemoryError::Unmapped)?;

            if flags & PTE_W == 0 {
                return Err(MemoryError::NotWritable);
            }

            let n = (PAGE_SIZE - offset).min(data.len() - done);

            mem.write(frame, offset, &data[done..done + n]);
            done += n;
        }

        Ok(())
    }

    pub fn write_u32(
        &mut self,
        mem: &mut PhysicalMemory,
        va: usize,
        value: u32,
    ) -> Result<(), MemoryError> {
        self.write(mem, va, &value.to_le_bytes())
    }

    pub fn read_u32(&self, mem: &PhysicalMemory, va: usize) -> Result<u32, MemoryError> {
        let mut buf = [0u8; 4];
        self.read(mem, va, &mut buf)?;
        Ok(u32::from_le_bytes(buf))
    }

    pub fn write_u64(
        &mut self,
        mem: &mut PhysicalMemory,
        va: usize,
        value: u64,
    ) -> Result<(), MemoryError> {
        self.write(mem, va, &value.to_le_bytes())
    }

    pub fn read_u64(&self, mem: &PhysicalMemory, va: usize) -> Result<u64, MemoryError> {
        let mut buf = [0u8; 8];
        self.read(mem, va, &mut buf)?;
        Ok(u64::from_le_bytes(buf))
    }

    /// Read null terminated string
    pub fn read_cstr(
        &self,
        mem: &PhysicalMemory,
        va: usize,
        max: usize,
    ) -> Result<Vec<u8>, MemoryError> {
        let mut out = Vec::new();

        for i in 0..max {
            let byte = self.read_u8(mem, va + i)?;
            if byte == 0 {
                return Ok(out);
            }

            out.push(byte);
        }
        Ok(out)
    }

    pub fn read_u8(&self, mem: &PhysicalMemory, va: usize) -> Result<u8, MemoryError> {
        let mut buf = [0u8; 1];
        self.read(mem, va, &mut buf)?;
        Ok(buf[0])
    }

    pub fn sbrk(&mut self, mem: &mut PhysicalMemory, delta: isize) -> Result<usize, MemoryError> {
        let old = self.brk;
        let new = if delta >= 0 {
            old.checked_add(delta as usize)
                .ok_or(MemoryError::OutOfMemory)?
        } else {
            old.checked_sub(delta.unsigned_abs())
                .ok_or(MemoryError::OutOfMemory)?
        };

        if new > USER_TOP {
            return Err(MemoryError::OutOfMemory);
        }

        let old_page = page_round_up(old);
        let new_page = page_round_up(new);

        if new > old {
            let mut va = old_page;

            while va < new_page {
                self.alloc_and_map(mem, va, PTE_R | PTE_W | PTE_U)?;
                va += PAGE_SIZE;
            }
        } else if new < old {
            let mut va = new_page;

            while va < old_page {
                if let Some(frame) = self.pt.unmap(mem, va) {
                    mem.free(frame);
                }

                va += PAGE_SIZE;
            }
        }

        self.brk = new;
        Ok(old)
    }

    pub fn fork_copy(
        mem: &mut PhysicalMemory,
        src: &AddressSpace,
    ) -> Result<AddressSpace, MemoryError> {
        let pt = PageTable::new(mem).ok_or(MemoryError::OutOfMemory)?;

        if let Err(err) = src.pt.copy_into(mem, &pt) {
            pt.free(mem);
            return Err(MemoryError::from(err));
        }

        Ok(AddressSpace { pt, brk: src.brk })
    }

    pub fn free(&self, mem: &mut PhysicalMemory) {
        self.pt.free(mem);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_and_read_all_pages() {
        let mut mem = PhysicalMemory::new(32);
        let mut space = AddressSpace::new(&mut mem).unwrap();
        space
            .alloc_and_map(&mut mem, 0x8000, PTE_R | PTE_W | PTE_U)
            .unwrap();
        space
            .alloc_and_map(&mut mem, 0x9000, PTE_R | PTE_W | PTE_U)
            .unwrap();

        let data = [7u8; 32];
        space.write(&mut mem, 0x8ff0, &data).unwrap();
        let mut buf = [0u8; 32];
        space.read(&mem, 0x8ff0, &mut buf).unwrap();
        assert_eq!(buf, data);
    }

    #[test]
    fn unmapped_access_fails() {
        let mut mem = PhysicalMemory::new(8);
        let space = AddressSpace::new(&mut mem).unwrap();
        let mut buf = [0u8; 4];
        assert_eq!(
            space.read(&mem, 0x1234, &mut buf),
            Err(MemoryError::Unmapped)
        );
    }

    #[test]
    fn read_only_page_rejects_write() {
        let mut mem = PhysicalMemory::new(16);
        let mut space = AddressSpace::new(&mut mem).unwrap();
        space
            .alloc_and_map(&mut mem, 0x1000, PTE_R | PTE_U)
            .unwrap();
        assert_eq!(
            space.write(&mut mem, 0x1000, &[1, 2, 3, 4]),
            Err(MemoryError::NotWritable)
        );
    }

    #[test]
    fn sbrk_grows_heap() {
        let mut mem = PhysicalMemory::new(64);
        let mut space = AddressSpace::new(&mut mem).unwrap();
        let free_before = mem.free_frames();
        let old = space.sbrk(&mut mem, 2 * PAGE_SIZE as isize).unwrap();
        assert_eq!(old, HEAP_BASE);
        space.write(&mut mem, old, &[1, 2, 3, 4]).unwrap();
        assert!(mem.free_frames() < free_before);
        let grown = mem.free_frames();

        let mut buf = [0u8; 4];
        space.read(&mem, old, &mut buf).unwrap();
        assert_eq!(buf, [1, 2, 3, 4]);

        space.sbrk(&mut mem, -(2 * PAGE_SIZE as isize)).unwrap();
        assert_eq!(mem.free_frames(), grown + 2);
    }

    #[test]
    fn fork_copy() {
        let mut mem = PhysicalMemory::new(64);
        let mut parent = AddressSpace::new(&mut mem).unwrap();
        parent
            .alloc_and_map(&mut mem, 0x1000, PTE_R | PTE_W | PTE_U)
            .unwrap();
        parent.write(&mut mem, 0x1000, &[42, 42, 42, 42]).unwrap();

        let mut child = AddressSpace::fork_copy(&mut mem, &parent).unwrap();
        let mut buf = [0u8; 4];
        child.read(&mem, 0x1000, &mut buf).unwrap();
        assert_eq!(buf, [42, 42, 42, 42]);

        child.write(&mut mem, 0x1000, &[0, 0, 0, 0]).unwrap();
        parent.read(&mem, 0x1000, &mut buf).unwrap();
        assert_eq!(buf, [42, 42, 42, 42]);
    }
}
