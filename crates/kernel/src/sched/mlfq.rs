use crate::{
    proc::{ProcState, ProcTable},
    sched::Scheduler,
};

/// Feedback levels.
pub const LEVELS: usize = 3;

pub struct Mlfq {
    quantum: u64,
    levels: usize,
}

impl Mlfq {
    pub fn new(quantum: u64) -> Self {
        Self {
            quantum,
            levels: LEVELS,
        }
    }
}

impl Scheduler for Mlfq {
    fn name(&self) -> &'static str {
        "mlfq"
    }

    fn on_ready(&mut self, pid: usize, table: &mut ProcTable) {
        if let Some(proc) = table.get_mut(pid) {
            proc.sched.level = 0;
        }
    }

    fn pick(&mut self, table: &ProcTable) -> Option<usize> {
        for level in 0..self.levels {
            let best = table
                .iter()
                .filter(|p| p.state == ProcState::Runnable && p.sched.level == level)
                .min_by_key(|p| (p.sched.enqueued, p.pid))
                .map(|p| p.pid);
            if best.is_some() {
                return best;
            }
        }

        None
    }

    fn on_run(&mut self, pid: usize, used: u64, table: &mut ProcTable) {
        if let Some(proc) = table.get_mut(pid) {
            proc.sched.cpu_ticks += used;
            if used >= self.quantum && proc.sched.level + 1 < self.levels {
                proc.sched.level += 1;
            }
        }
    }
}
