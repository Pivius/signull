//! Stack interpreter.

use crate::isa::{
    MemError, MemWidth, Op, Program, ProgramImage, SyscallAction, SyscallResult, VmError,
};

/// When a program executes [`Op::Syscall`] the interpreter calls [`Host::syscall`].
/// Memory access goes through [`Host::load`] and [`Host::store`].
pub trait Host {
    fn syscall(&mut self, number: u16, args: &[i64]) -> SyscallResult;

    fn load(&mut self, addr: usize, width: MemWidth) -> Result<u64, MemError> {
        let _ = (addr, width);
        Err(MemError::Unmapped)
    }

    fn store(&mut self, addr: usize, width: MemWidth, value: u64) -> Result<(), MemError> {
        let _ = (addr, width, value);
        Err(MemError::NotWritable)
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct NullHost;

impl Host for NullHost {
    fn syscall(&mut self, _number: u16, _args: &[i64]) -> SyscallResult {
        SyscallResult::continuing(0)
    }
}

fn mask(width: MemWidth) -> u64 {
    match width {
        MemWidth::B8 => 0xff,
        MemWidth::B16 => 0xffff,
        MemWidth::B32 => 0xffff_ffff,
        MemWidth::B64 => u64::MAX,
    }
}

/// A saved call-site. `base` is the caller's frame base, restored by `Ret`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frame {
    pub ret: usize,
    pub base: usize,
}

pub const DEFAULT_MAX_OPERAND: usize = 4096;
pub const DEFAULT_MAX_CALL_DEPTH: usize = 256;
pub const DEFAULT_FRAME_TOP: usize = 0x0001_0000;
pub const DEFAULT_FRAME_LIMIT: usize = 0x0000_0000;

#[derive(Clone, Debug)]
pub struct Vm {
    pub program: Program,
    pub stack: Vec<i64>,
    pub pc: usize,
    pub frame_base: usize,
    pub frame_top: usize,
    /// Lowest address frames may occupy. `Enter` below this is a
    /// [`VmError::FrameOverflow`].
    pub frame_limit: usize,
    pub call_stack: Vec<Frame>,
    pub max_operand: usize,
    pub max_call_depth: usize,
    pub halted: bool,
    pub blocked: bool,
    pub exited: bool,
    pub exit_code: i64,
    pub steps: u64,
    pub stopped: bool,
}

impl Vm {
    pub fn new(program: Program, local_count: usize) -> Self {
        let entry_base = DEFAULT_FRAME_TOP.saturating_sub(local_count.saturating_mul(8));
        Self {
            program,
            stack: Vec::with_capacity(32),
            pc: 0,
            frame_base: entry_base,
            frame_top: DEFAULT_FRAME_TOP,
            frame_limit: DEFAULT_FRAME_LIMIT,
            call_stack: Vec::new(),
            max_operand: DEFAULT_MAX_OPERAND,
            max_call_depth: DEFAULT_MAX_CALL_DEPTH,
            halted: false,
            blocked: false,
            exited: false,
            exit_code: 0,
            steps: 0,
            stopped: false,
        }
    }

    pub fn from_image(image: &ProgramImage, data_addrs: Vec<usize>) -> Self {
        let mut vm = Self::new(image.resolve(data_addrs), image.locals);
        vm.pc = image.entry;
        vm
    }

    pub fn set_frame_region(&mut self, top: usize, limit: usize) {
        self.frame_base = top;
        self.frame_top = top;
        self.frame_limit = limit;
    }

    pub fn is_runnable(&self) -> bool {
        !self.halted && !self.blocked && !self.stopped
    }

    pub fn push(&mut self, value: i64) {
        self.stack.push(value);
    }

    pub fn pop(&mut self) -> Result<i64, VmError> {
        self.stack.pop().ok_or(VmError::StackUnderflow)
    }

    fn pop2(&mut self) -> Result<(i64, i64), VmError> {
        let b = self.pop()?;
        let a = self.pop()?;
        Ok((a, b))
    }

    fn jump(&mut self, target: usize) -> Result<(), VmError> {
        if target > self.program.code.len() {
            return Err(VmError::BadJump(target));
        }

        self.pc = target;
        Ok(())
    }

    fn load_op(&mut self, host: &mut dyn Host, width: MemWidth) -> Result<(), VmError> {
        self.pc += 1;
        let addr = self.pop()?;

        if addr < 0 {
            return Err(VmError::Memory(MemError::Unmapped));
        }

        let value = host.load(addr as usize, width).map_err(VmError::Memory)?;
        self.push((value & mask(width)) as i64);

        Ok(())
    }

    fn store_op(&mut self, host: &mut dyn Host, width: MemWidth) -> Result<(), VmError> {
        self.pc += 1;
        let value = self.pop()?;
        let addr = self.pop()?;

        if addr < 0 {
            return Err(VmError::Memory(MemError::Unmapped));
        }

        host.store(addr as usize, width, (value as u64) & mask(width))
            .map_err(VmError::Memory)?;

        Ok(())
    }

    pub fn step(&mut self, host: &mut dyn Host) -> Result<(), VmError> {
        if self.halted {
            return Err(VmError::Halted);
        }

        if self.blocked {
            return Err(VmError::Blocked);
        }

        let op = *self.program.code.get(self.pc).ok_or(VmError::OutOfBounds)?;
        self.steps += 1;

        match op {
            Op::Push(value) => {
                self.pc += 1;
                self.push(value);
            }
            Op::Pop => {
                self.pc += 1;
                self.pop()?;
            }
            Op::Dup => {
                self.pc += 1;
                let value = *self.stack.last().ok_or(VmError::StackUnderflow)?;
                self.push(value);
            }
            Op::Swap => {
                self.pc += 1;
                let (a, b) = self.pop2()?;
                self.push(b);
                self.push(a);
            }
            Op::Load(slot) => {
                self.pc += 1;
                let addr = self.frame_base + slot as usize * 8;
                let value = host.load(addr, MemWidth::B64).map_err(VmError::Memory)?;
                self.push(value as i64);
            }
            Op::Store(slot) => {
                self.pc += 1;
                let value = self.pop()?;
                let addr = self.frame_base + slot as usize * 8;
                host.store(addr, MemWidth::B64, value as u64)
                    .map_err(VmError::Memory)?;
            }
            Op::Lea(slot) => {
                self.pc += 1;
                let addr = self.frame_base + slot as usize * 8;
                self.push(addr as i64);
            }
            Op::Add => {
                self.pc += 1;
                let (a, b) = self.pop2()?;
                self.push(a.wrapping_add(b));
            }
            Op::Sub => {
                self.pc += 1;
                let (a, b) = self.pop2()?;
                self.push(a.wrapping_sub(b));
            }
            Op::Mul => {
                self.pc += 1;
                let (a, b) = self.pop2()?;
                self.push(a.wrapping_mul(b));
            }
            Op::Div => {
                self.pc += 1;
                let (a, b) = self.pop2()?;
                if b == 0 {
                    return Err(VmError::DivideByZero);
                }
                self.push(a.wrapping_div(b));
            }
            Op::Mod => {
                self.pc += 1;
                let (a, b) = self.pop2()?;
                if b == 0 {
                    return Err(VmError::DivideByZero);
                }
                self.push(a.wrapping_rem(b));
            }
            Op::Neg => {
                self.pc += 1;
                let a = self.pop()?;
                self.push(a.wrapping_neg());
            }
            Op::And => {
                self.pc += 1;
                let (a, b) = self.pop2()?;
                self.push(a & b);
            }
            Op::Or => {
                self.pc += 1;
                let (a, b) = self.pop2()?;
                self.push(a | b);
            }
            Op::Xor => {
                self.pc += 1;
                let (a, b) = self.pop2()?;
                self.push(a ^ b);
            }
            Op::Shl => {
                self.pc += 1;
                let (a, b) = self.pop2()?;
                self.push(a.wrapping_shl(b as u32));
            }
            Op::ShrU => {
                self.pc += 1;
                let (a, b) = self.pop2()?;
                self.push(((a as u64).wrapping_shr(b as u32)) as i64);
            }
            Op::Sar => {
                self.pc += 1;
                let (a, b) = self.pop2()?;
                self.push(a.wrapping_shr(b as u32));
            }
            Op::BitNot => {
                self.pc += 1;
                let a = self.pop()?;
                self.push(!a);
            }
            Op::Eq => self.compare(|a, b| a == b)?,
            Op::Ne => self.compare(|a, b| a != b)?,
            Op::Lt => self.compare(|a, b| a < b)?,
            Op::Le => self.compare(|a, b| a <= b)?,
            Op::Gt => self.compare(|a, b| a > b)?,
            Op::Ge => self.compare(|a, b| a >= b)?,
            Op::LtU => self.compare(|a, b| (a as u64) < (b as u64))?,
            Op::LeU => self.compare(|a, b| (a as u64) <= (b as u64))?,
            Op::GtU => self.compare(|a, b| (a as u64) > (b as u64))?,
            Op::GeU => self.compare(|a, b| (a as u64) >= (b as u64))?,
            Op::Not => {
                self.pc += 1;
                let a = self.pop()?;
                self.push(i64::from(a == 0));
            }
            Op::Jmp(target) => self.jump(target)?,
            Op::Jz(target) => {
                let a = self.pop()?;
                if a == 0 {
                    self.jump(target)?;
                } else {
                    self.pc += 1;
                }
            }
            Op::Jnz(target) => {
                let a = self.pop()?;
                if a != 0 {
                    self.jump(target)?;
                } else {
                    self.pc += 1;
                }
            }
            Op::Enter(argc, nlocals) => {
                self.pc += 1;
                let argc = argc as usize;
                let nlocals = nlocals as usize;

                if nlocals < argc {
                    return Err(VmError::BadFrame);
                }

                if self.stack.len() < argc {
                    return Err(VmError::StackUnderflow);
                }

                let bytes = nlocals.checked_mul(8).ok_or(VmError::FrameOverflow)?;
                let base = self
                    .frame_base
                    .checked_sub(bytes)
                    .ok_or(VmError::FrameOverflow)?;
                if base < self.frame_limit {
                    return Err(VmError::FrameOverflow);
                }

                for slot in 0..nlocals {
                    host.store(base + slot * 8, MemWidth::B64, 0)
                        .map_err(VmError::Memory)?;
                }

                let start = self.stack.len() - argc;
                for (i, value) in self.stack[start..].iter().enumerate() {
                    host.store(base + i * 8, MemWidth::B64, *value as u64)
                        .map_err(VmError::Memory)?;
                }
                self.stack.truncate(start);
                self.frame_base = base;
            }
            Op::Call(target) => {
                if self.call_stack.len() >= self.max_call_depth {
                    return Err(VmError::CallDepthExceeded);
                }

                self.call_stack.push(Frame {
                    ret: self.pc + 1,
                    base: self.frame_base,
                });
                self.jump(target)?;
            }
            Op::Ret => {
                let frame = self.call_stack.pop().ok_or(VmError::EmptyCallStack)?;
                self.pc = frame.ret;
                self.frame_base = frame.base;
            }
            Op::Load8 => self.load_op(host, MemWidth::B8)?,
            Op::Load16 => self.load_op(host, MemWidth::B16)?,
            Op::Load32 => self.load_op(host, MemWidth::B32)?,
            Op::Load64 => self.load_op(host, MemWidth::B64)?,
            Op::Store8 => self.store_op(host, MemWidth::B8)?,
            Op::Store16 => self.store_op(host, MemWidth::B16)?,
            Op::Store32 => self.store_op(host, MemWidth::B32)?,
            Op::Store64 => self.store_op(host, MemWidth::B64)?,
            Op::SignExt8 => {
                self.pc += 1;
                let a = self.pop()?;
                self.push(i64::from(a as i8));
            }
            Op::SignExt16 => {
                self.pc += 1;
                let a = self.pop()?;
                self.push(i64::from(a as i16));
            }
            Op::SignExt32 => {
                self.pc += 1;
                let a = self.pop()?;
                self.push(i64::from(a as i32));
            }
            Op::PushData(index) => {
                self.pc += 1;
                let base = *self
                    .program
                    .data_addrs
                    .get(index as usize)
                    .ok_or(VmError::BadData(index))?;
                self.push(base as i64);
            }
            Op::Syscall(number, argc) => {
                let n = argc as usize;
                if self.stack.len() < n {
                    return Err(VmError::StackUnderflow);
                }

                let start = self.stack.len() - n;
                let result = host.syscall(number, &self.stack[start..]);

                match result.action {
                    SyscallAction::Continue => {
                        self.stack.truncate(start);
                        self.push(result.value);
                        self.pc += 1;
                        if result.stop {
                            self.stopped = true;
                        }
                    }
                    SyscallAction::Block => {
                        self.blocked = true;
                        if !result.retry {
                            self.stack.truncate(start);
                            self.push(result.value);
                            self.pc += 1;
                        }
                    }
                    SyscallAction::Exit(code) => {
                        self.stack.truncate(start);
                        self.push(result.value);
                        self.exited = true;
                        self.exit_code = code;
                        self.halted = true;
                    }
                }
            }
            Op::Halt => {
                self.halted = true;
            }
        }

        if self.stack.len() > self.max_operand {
            return Err(VmError::StackOverflow);
        }

        Ok(())
    }

    fn compare(&mut self, test: fn(i64, i64) -> bool) -> Result<(), VmError> {
        self.pc += 1;
        let (a, b) = self.pop2()?;
        self.push(i64::from(test(a, b)));
        Ok(())
    }

    /// Run until the program halts, blocks or max set of instructions have
    /// Executed. Returns the number of instructions executed this call.
    pub fn run(&mut self, host: &mut dyn Host, max_steps: u64) -> Result<u64, VmError> {
        self.stopped = false;
        let start = self.steps;
        while self.is_runnable() && self.steps - start < max_steps {
            self.step(host)?;
        }
        Ok(self.steps - start)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct RecordingHost {
        calls: Vec<(u16, Vec<i64>)>,
        action: SyscallAction,
    }

    impl Host for RecordingHost {
        fn syscall(&mut self, number: u16, args: &[i64]) -> SyscallResult {
            self.calls.push((number, args.to_vec()));
            SyscallResult {
                value: args.iter().sum(),
                action: self.action,
                retry: true,
                stop: false,
            }
        }
    }

    #[derive(Default)]
    struct FlatMemory {
        bytes: Vec<u8>,
    }

    impl FlatMemory {
        fn new() -> Self {
            Self {
                bytes: vec![0; 0x1_0000],
            }
        }
    }

    impl Host for FlatMemory {
        fn syscall(&mut self, _number: u16, _args: &[i64]) -> SyscallResult {
            SyscallResult::continuing(0)
        }

        fn load(&mut self, addr: usize, width: MemWidth) -> Result<u64, MemError> {
            let n = width.bytes();
            let slice = self.bytes.get(addr..addr + n).ok_or(MemError::Unmapped)?;
            let mut buf = [0u8; 8];
            buf[..n].copy_from_slice(slice);
            Ok(u64::from_le_bytes(buf))
        }

        fn store(&mut self, addr: usize, width: MemWidth, value: u64) -> Result<(), MemError> {
            let n = width.bytes();
            let slice = self
                .bytes
                .get_mut(addr..addr + n)
                .ok_or(MemError::NotWritable)?;
            slice.copy_from_slice(&value.to_le_bytes()[..n]);
            Ok(())
        }
    }

    fn run_to_end(program: Program) -> Vm {
        let mut vm = Vm::new(program, 8);
        let mut host = FlatMemory::new();
        vm.run(&mut host, 10_000).expect("program should run");
        vm
    }

    #[test]
    fn factorial_of_five() {
        let program = Program::new(vec![
            Op::Push(5),
            Op::Store(0),
            Op::Push(1),
            Op::Store(1),
            Op::Load(0),
            Op::Push(0),
            Op::Gt,
            Op::Jz(17),
            Op::Load(1),
            Op::Load(0),
            Op::Mul,
            Op::Store(1),
            Op::Load(0),
            Op::Push(1),
            Op::Sub,
            Op::Store(0),
            Op::Jmp(4),
            Op::Load(1),
            Op::Halt,
        ]);
        let vm = run_to_end(program);
        assert_eq!(vm.stack, vec![120]);
        assert!(vm.halted);
    }

    #[test]
    fn syscall_receives_arguments_in_order() {
        let program = Program::new(vec![Op::Push(7), Op::Push(3), Op::Syscall(42, 2), Op::Halt]);
        let mut vm = Vm::new(program, 0);
        let mut host = RecordingHost::default();
        vm.run(&mut host, 100).unwrap();
        assert_eq!(host.calls, vec![(42, vec![7, 3])]);
        assert_eq!(vm.stack, vec![10]);
    }

    #[test]
    fn memory_ops_go_through_the_host() {
        let program = Program::new(vec![
            Op::Push(4),
            Op::Push(0x2a),
            Op::Store8,
            Op::Push(4),
            Op::Load8,
            Op::Halt,
        ]);
        let mut vm = Vm::new(program, 0);
        let mut host = FlatMemory::new();
        vm.run(&mut host, 100).unwrap();
        assert_eq!(host.bytes[4], 0x2a);
        assert_eq!(vm.stack, vec![0x2a]);
    }

    #[test]
    fn recursive_factorial_uses_frames() {
        let program = Program::new(vec![
            Op::Push(5),
            Op::Call(3),
            Op::Halt,
            Op::Enter(1, 1),
            Op::Load(0),
            Op::Push(1),
            Op::Le,
            Op::Jz(10),
            Op::Push(1),
            Op::Ret,
            Op::Load(0),
            Op::Load(0),
            Op::Push(1),
            Op::Sub,
            Op::Call(3),
            Op::Mul,
            Op::Ret,
        ]);
        let mut vm = Vm::new(program, 0);
        let mut host = FlatMemory::new();
        vm.run(&mut host, 10_000).unwrap();
        assert_eq!(vm.stack, vec![120]);
        assert!(vm.halted);
    }

    #[test]
    fn frames_isolate_locals_recursion() {
        // sum(n) = n + sum(n - 1); sum(3) == 6
        let program = Program::new(vec![
            Op::Push(3),
            Op::Call(3),
            Op::Halt,
            Op::Enter(1, 1),
            Op::Load(0),
            Op::Push(0),
            Op::Gt,
            Op::Jz(15),
            Op::Load(0),
            Op::Load(0),
            Op::Push(1),
            Op::Sub,
            Op::Call(3),
            Op::Add,
            Op::Ret,
            Op::Push(0),
            Op::Ret,
        ]);
        let mut vm = Vm::new(program, 0);
        let mut host = FlatMemory::new();
        vm.run(&mut host, 1000).unwrap();
        assert_eq!(vm.stack, vec![6]);
    }

    #[test]
    fn lea_pushes_a_slot_address() {
        let program = Program::new(vec![
            Op::Push(7),
            Op::Store(0),
            Op::Lea(0),
            Op::Store(1),
            Op::Load(1),
            Op::Load64,
            Op::Halt,
        ]);
        let mut vm = Vm::new(program, 4);
        let mut host = FlatMemory::new();
        vm.run(&mut host, 100).unwrap();
        assert_eq!(vm.stack, vec![7]);
    }

    #[test]
    fn bitwise_and_unsigned_ops() {
        let program = Program::new(vec![Op::Push(-1), Op::Push(1), Op::ShrU, Op::Halt]);
        let mut vm = Vm::new(program, 0);
        vm.run(&mut NullHost, 100).unwrap();
        assert_eq!(vm.stack, vec![i64::MAX]);

        let program = Program::new(vec![Op::Push(0b1010), Op::Push(0b0110), Op::And, Op::Halt]);
        let mut vm = Vm::new(program, 0);
        vm.run(&mut NullHost, 100).unwrap();
        assert_eq!(vm.stack, vec![0b0010]);

        let program = Program::new(vec![Op::Push(-1), Op::Push(1), Op::GtU, Op::Halt]);
        let mut vm = Vm::new(program, 0);
        vm.run(&mut NullHost, 100).unwrap();
        assert_eq!(vm.stack, vec![1]);
    }

    #[test]
    fn deep_recursion_overflows() {
        let program = Program::new(vec![
            Op::Push(0),
            Op::Call(3),
            Op::Halt,
            Op::Enter(0, 0),
            Op::Call(3),
            Op::Ret,
        ]);
        let mut vm = Vm::new(program, 0);
        vm.max_call_depth = 8;
        let err = vm.run(&mut NullHost, 10_000).unwrap_err();
        assert_eq!(err, VmError::CallDepthExceeded);
    }

    #[test]
    fn operand_stack_is_capped() {
        let program = Program::new(vec![Op::Push(0); DEFAULT_MAX_OPERAND + 8]);
        let mut vm = Vm::new(program, 0);
        let err = vm.run(&mut NullHost, 1_000_000).unwrap_err();
        assert_eq!(err, VmError::StackOverflow);
    }

    #[test]
    fn frame_region_is_capped() {
        let program = Program::new(vec![Op::Enter(0, 100), Op::Call(0)]);
        let mut vm = Vm::new(program, 0);
        vm.set_frame_region(0x8000, 0x4000);
        let mut host = FlatMemory::new();
        let err = vm.run(&mut host, 10_000).unwrap_err();
        assert_eq!(err, VmError::FrameOverflow);
    }
}
