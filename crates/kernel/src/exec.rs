//! Loads an image into address space

use bytecode::ProgramImage;

use crate::memory::{
    AddressSpace, MemoryError, PAGE_SIZE, PTE_R, PTE_U, PTE_W, PhysicalMemory, USER_TOP,
    page_round_up,
};

/// First available virtual address
pub const DATA_BASE: usize = 0x0001_0000;
/// Stack size in pages
pub const STACK_PAGES: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecError {
    OutOfMemory,
    BadImage,
}

impl From<MemoryError> for ExecError {
    fn from(err: MemoryError) -> Self {
        match err {
            MemoryError::OutOfMemory => ExecError::OutOfMemory,
            _ => ExecError::BadImage,
        }
    }
}

pub struct Loaded {
    pub space: AddressSpace,
    pub data_addrs: Vec<usize>,
    /// Exclusive top of the user stack region.
    pub stack_top: usize,
    /// Lowest address of the user stack region.
    pub stack_base: usize,
}

/// Map the image's data and a new stack into a unused address space.
pub fn load_image(mem: &mut PhysicalMemory, image: &ProgramImage) -> Result<Loaded, ExecError> {
    let mut space = AddressSpace::new(mem).ok_or(ExecError::OutOfMemory)?;

    let stack_base = USER_TOP - STACK_PAGES * PAGE_SIZE;
    for page in 0..STACK_PAGES {
        space.alloc_and_map(mem, stack_base + page * PAGE_SIZE, PTE_R | PTE_W | PTE_U)?;
    }

    let mut data_addrs = Vec::with_capacity(image.data.len());
    let mut next = DATA_BASE;
    for blob in &image.data {
        let region = page_round_up(blob.len().max(1));
        let start = next;

        for page in 0..region / PAGE_SIZE {
            space.alloc_and_map(mem, start + page * PAGE_SIZE, PTE_R | PTE_W | PTE_U)?;
        }

        if !blob.is_empty() {
            space.write(mem, start, blob)?;
        }

        data_addrs.push(start);
        next += region;
    }

    Ok(Loaded {
        space,
        data_addrs,
        stack_top: USER_TOP,
        stack_base,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytecode::Op;

    #[test]
    fn maps_data_and_stack() {
        let mut mem = PhysicalMemory::new(128);
        let image = ProgramImage::new(vec![Op::Halt], 0)
            .with_data(vec![b"hello".to_vec(), vec![0u8; 5000]]);
        let mut loaded = load_image(&mut mem, &image).unwrap();

        assert_eq!(loaded.data_addrs.len(), 2);
        assert_eq!(loaded.data_addrs[0], DATA_BASE);
        // The second blob spills past one page, so it starts on the next page.
        assert_eq!(loaded.data_addrs[1], DATA_BASE + PAGE_SIZE);

        let mut buf = [0u8; 5];
        loaded
            .space
            .read(&mem, loaded.data_addrs[0], &mut buf)
            .unwrap();
        assert_eq!(&buf, b"hello");

        loaded
            .space
            .write(&mut mem, USER_TOP - 8, &[1, 2, 3, 4, 5, 6, 7, 8])
            .unwrap();
    }

    #[test]
    fn empty_data_yields_no_addresses() {
        let mut mem = PhysicalMemory::new(64);
        let image = ProgramImage::new(vec![Op::Halt], 0);
        let loaded = load_image(&mut mem, &image).unwrap();
        assert!(loaded.data_addrs.is_empty());
    }
}
