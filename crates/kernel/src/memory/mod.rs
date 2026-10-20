pub mod address;
pub mod pagetable;
pub mod phys;

pub use address::{AddressSpace, HEAP_BASE, MemoryError, USER_TOP};
pub use pagetable::{MapError, PTE_R, PTE_U, PTE_W, PTE_X, PageTable};
pub use phys::PhysicalMemory;

pub const PAGE_SIZE: usize = 4096;

// 64-bit entries per pagetable page.
pub const PTE_COUNT: usize = PAGE_SIZE / 8;

pub fn page_round_down(address: usize) -> usize {
    address & !(PAGE_SIZE - 1)
}

pub fn page_round_up(address: usize) -> usize {
    page_round_down(address + PAGE_SIZE - 1)
}
