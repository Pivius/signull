//! Trap entry and return from trap

use bytecode::{Frame, Vm};

/// Registers saved on kernel entry. Locals live in the process address space,
/// so only the frame pointer needs saving (not the local values).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrapFrame {
    pub pc: usize,
    pub frame_base: usize,
    pub call_stack: Vec<Frame>,
    pub stack: Vec<i64>,
}

impl TrapFrame {
    pub fn capture(vm: &Vm) -> Self {
        Self {
            pc: vm.pc,
            frame_base: vm.frame_base,
            call_stack: vm.call_stack.clone(),
            stack: vm.stack.clone(),
        }
    }

    pub fn restore(&self, vm: &mut Vm) {
        vm.pc = self.pc;
        vm.frame_base = self.frame_base;
        vm.call_stack.clone_from(&self.call_stack);
        vm.stack.clone_from(&self.stack);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrapCause {
    Syscall(u16),
    Timer,
    Device(crate::dev::Interrupt),
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytecode::{Op, Program};

    #[test]
    fn frame_round_trip() {
        let mut vm = Vm::new(Program::new(vec![Op::Push(1), Op::Halt]), 4);
        vm.pc = 1;
        vm.frame_base = 0x8000;
        vm.call_stack.push(Frame {
            ret: 9,
            base: 0x9000,
        });
        vm.stack.push(7);

        let frame = TrapFrame::capture(&vm);
        vm.pc = 0;
        vm.frame_base = 0;
        vm.call_stack.clear();
        vm.stack.clear();
        frame.restore(&mut vm);

        assert_eq!(vm.pc, 1);
        assert_eq!(vm.frame_base, 0x8000);
        assert_eq!(
            vm.call_stack,
            vec![Frame {
                ret: 9,
                base: 0x9000
            }]
        );
        assert_eq!(vm.stack, vec![7]);
    }
}
