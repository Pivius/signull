use super::Scheduler;
use crate::proc::{ProcState, ProcTable};

/// Always runs the runnable process with the lowest priority number.
///
/// Lower numbers are more urgent.
pub struct Priority;

impl Scheduler for Priority {
    fn name(&self) -> &'static str {
        "priority"
    }

    fn pick(&mut self, table: &ProcTable) -> Option<usize> {
        table
            .iter()
            .filter(|p| p.state == ProcState::Runnable)
            .min_by_key(|p| (p.sched.priority, p.sched.enqueued, p.pid))
            .map(|p| p.pid)
    }

    fn on_run(&mut self, pid: usize, used: u64, table: &mut ProcTable) {
        if let Some(proc) = table.get_mut(pid) {
            proc.sched.cpu_ticks += used;
        }
    }
}
