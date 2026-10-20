use super::Scheduler;
use crate::proc::{ProcState, ProcTable};

/// Rotates through the runnable set, giving each process one quantum.
#[derive(Default)]
pub struct RoundRobin {
    cursor: usize,
}

impl RoundRobin {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Scheduler for RoundRobin {
    fn name(&self) -> &'static str {
        "round-robin"
    }

    fn pick(&mut self, table: &ProcTable) -> Option<usize> {
        let mut pids: Vec<usize> = table
            .iter()
            .filter(|p| p.state == ProcState::Runnable)
            .map(|p| p.pid)
            .collect();
        if pids.is_empty() {
            return None;
        }
        pids.sort_unstable();
        let index = self.cursor % pids.len();
        self.cursor = (index + 1) % pids.len();
        Some(pids[index])
    }

    fn on_run(&mut self, pid: usize, used: u64, table: &mut ProcTable) {
        if let Some(proc) = table.get_mut(pid) {
            proc.sched.cpu_ticks += used;
        }
    }
}
