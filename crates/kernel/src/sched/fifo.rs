//! First-in, first-out scheduling.

use super::Scheduler;
use crate::proc::{ProcState, ProcTable};

/// Runs the earliest runnable process first.
pub struct Fifo;

impl Scheduler for Fifo {
    fn name(&self) -> &'static str {
        "fifo"
    }

    fn pick(&mut self, table: &ProcTable) -> Option<usize> {
        table
            .iter()
            .filter(|p| p.state == ProcState::Runnable)
            .min_by_key(|p| (p.sched.enqueued, p.pid))
            .map(|p| p.pid)
    }

    fn on_run(&mut self, pid: usize, used: u64, table: &mut ProcTable) {
        if let Some(proc) = table.get_mut(pid) {
            proc.sched.cpu_ticks += used;
        }
    }
}
