//! Context switch between processes

use bytecode::Vm;

use crate::trap::TrapFrame;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Context {
    frame: Option<TrapFrame>,
}

impl Context {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_saved(&self) -> bool {
        self.frame.is_some()
    }

    pub fn save(&mut self, vm: &Vm) {
        self.frame = Some(TrapFrame::capture(vm));
    }

    pub fn restore(&self, vm: &mut Vm) -> bool {
        match &self.frame {
            Some(frame) => {
                frame.restore(vm);
                true
            }
            None => false,
        }
    }
}

pub fn switch_to(from: &mut Context, from_vm: &Vm, to: &Context, to_vm: &mut Vm) -> bool {
    from.save(from_vm);
    to.restore(to_vm)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytecode::{Op, Program};

    #[test]
    fn switch_saves_current_restores_next() {
        let mut a = Vm::new(Program::new(vec![Op::Push(1), Op::Halt]), 1);
        a.pc = 1;
        a.stack.push(7);

        let mut b = Vm::new(Program::new(vec![Op::Push(2), Op::Halt]), 1);
        b.pc = 1;
        b.stack.push(9);

        let mut ctx_a = Context::new();
        let mut ctx_b = Context::new();
        ctx_b.save(&b);

        // Clobber b to prove the switch restores its saved context.
        b.pc = 0;
        b.stack.clear();

        assert!(switch_to(&mut ctx_a, &a, &ctx_b, &mut b));
        assert!(ctx_a.is_saved());
        assert_eq!(b.pc, 1);
        assert_eq!(b.stack, vec![9]);
    }

    #[test]
    fn restore_without_save_fails() {
        let mut vm = Vm::new(Program::new(vec![Op::Halt]), 0);
        let ctx = Context::new();
        assert!(!ctx.restore(&mut vm));
    }
}
