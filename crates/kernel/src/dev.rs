//! Simulated console, timer, interrupt controller and disk.

use std::collections::VecDeque;

#[derive(Default)]
pub struct Console {
    input: VecDeque<u8>,
    output: Vec<u8>,
}

impl Console {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_byte(&mut self, byte: u8) {
        self.input.push_back(byte);
    }

    pub fn push_bytes(&mut self, bytes: &[u8]) {
        self.input.extend(bytes.iter().copied());
    }

    pub fn push_line(&mut self, line: &str) {
        self.push_bytes(line.as_bytes());
        self.push_byte(b'\n');
    }

    pub fn has_input(&self) -> bool {
        !self.input.is_empty()
    }

    pub fn read(&mut self, max: usize) -> Vec<u8> {
        self.input.drain(..max.min(self.input.len())).collect()
    }

    pub fn write(&mut self, bytes: &[u8]) {
        self.output.extend_from_slice(bytes);
    }

    pub fn output(&self) -> &[u8] {
        &self.output
    }

    pub fn output_len(&self) -> usize {
        self.output.len()
    }

    /// Clears buffer taking everything written so far
    pub fn take_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.output)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interrupt {
    Timer,
    Console,
    Disk,
}

/// FIFO of pending interrupts
#[derive(Default)]
pub struct InterruptController {
    pending: VecDeque<Interrupt>,
}

impl InterruptController {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn raise(&mut self, interrupt: Interrupt) {
        self.pending.push_back(interrupt);
    }

    pub fn pop(&mut self) -> Option<Interrupt> {
        self.pending.pop_front()
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

#[derive(Default)]
pub struct Timer {
    ticks: u64,
}

impl Timer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn tick(&mut self) -> u64 {
        self.ticks += 1;
        self.ticks
    }

    pub fn ticks(&self) -> u64 {
        self.ticks
    }
}

pub struct Disk {
    blocks: Vec<Vec<u8>>,
    block_size: usize,
}

impl Disk {
    pub fn new(block_count: usize, block_size: usize) -> Self {
        Self {
            blocks: vec![vec![0; block_size]; block_count],
            block_size,
        }
    }

    pub fn block_size(&self) -> usize {
        self.block_size
    }

    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }

    pub fn read_block(&self, index: usize) -> Option<&[u8]> {
        self.blocks.get(index).map(|b| b.as_slice())
    }

    pub fn write_block(&mut self, index: usize, data: &[u8]) -> bool {
        match self.blocks.get_mut(index) {
            Some(block) if data.len() == self.block_size => {
                block.copy_from_slice(data);
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn console_round_trip() {
        let mut console = Console::new();
        console.push_line("hello");
        assert!(console.has_input());
        assert_eq!(console.read(2), b"he");
        assert_eq!(console.read(100), b"llo\n");
        assert!(!console.has_input());

        console.write(b"out");
        assert_eq!(console.take_output(), b"out");
        assert_eq!(console.output_len(), 0);
    }

    #[test]
    fn interrupts_fifo() {
        let mut ic = InterruptController::new();
        ic.raise(Interrupt::Timer);
        ic.raise(Interrupt::Disk);
        assert_eq!(ic.pop(), Some(Interrupt::Timer));
        assert_eq!(ic.pop(), Some(Interrupt::Disk));
        assert!(ic.is_empty());
    }

    #[test]
    fn disk_blocks() {
        let mut disk = Disk::new(4, 8);
        assert!(disk.write_block(0, b"12345678"));
        assert_eq!(disk.read_block(0), Some(&b"12345678"[..]));
        assert!(!disk.write_block(9, b"12345678"));
        assert!(!disk.write_block(1, b"short"));
    }
}
