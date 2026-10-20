//! Scheduling module for interchangeable scheduler policies.

mod fifo;
mod mlfq;
mod priority;
mod rr;

use crate::{
    proc::ProcTable,
    sched::{fifo::Fifo, mlfq::Mlfq, priority::Priority, rr::RoundRobin},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SchedMeta {
    /// Lower = Higher priority
    pub priority: i32,
    /// For CFS policies
    pub vruntime: u64,
    /// MLFQ queue level
    pub level: usize,
    /// Consumed instructions
    pub slice_used: u64,
    /// Instructions accounted
    pub cpu_ticks: u64,
    pub enqueued: u64,
    pub last_run: u64,
}

impl SchedMeta {
    pub fn new(enqueued: u64) -> Self {
        Self {
            priority: 0,
            vruntime: 0,
            level: 0,
            slice_used: 0,
            cpu_ticks: 0,
            enqueued,
            last_run: 0,
        }
    }
}

impl Default for SchedMeta {
    fn default() -> Self {
        Self::new(0)
    }
}

/// Selection of scheduling policies
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SchedPolicy {
    #[default]
    Fifo,
    RoundRobin,
    Priority,
    Mlfq,
}

impl SchedPolicy {
    pub fn label(self) -> &'static str {
        match self {
            SchedPolicy::Fifo => "fifo",
            SchedPolicy::RoundRobin => "round-robin",
            SchedPolicy::Priority => "priority",
            SchedPolicy::Mlfq => "mlfq",
        }
    }
}

pub trait Scheduler {
    fn name(&self) -> &'static str;

    fn on_ready(&mut self, pid: usize, table: &mut ProcTable) {
        let _ = (pid, table);
    }

    fn pick(&mut self, table: &ProcTable) -> Option<usize>;

    fn on_run(&mut self, pid: usize, used: u64, table: &mut ProcTable) {
        let _ = (pid, used, table);
    }
}

pub fn build(policy: SchedPolicy, quantum: u64) -> Box<dyn Scheduler> {
    match policy {
        SchedPolicy::Fifo => Box::new(Fifo),
        SchedPolicy::RoundRobin => Box::new(RoundRobin::new()),
        SchedPolicy::Priority => Box::new(Priority),
        SchedPolicy::Mlfq => Box::new(Mlfq::new(quantum)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::AddressSpace;
    use crate::memory::phys::PhysicalMemory;
    use crate::proc::ProcState;
    use crate::security::Credentials;
    use bytecode::{Op, Program, Vm};

    fn table(n: usize) -> (PhysicalMemory, ProcTable) {
        let mut mem = PhysicalMemory::new(64);
        let mut procs = ProcTable::new();
        for i in 0..n {
            let space = AddressSpace::new(&mut mem).unwrap();
            let vm = Vm::new(Program::new(vec![Op::Halt]), 0);
            procs.alloc(&format!("p{i}"), space, vm, Credentials::default());
        }
        (mem, procs)
    }

    #[test]
    fn fifo_runs_in_arrival_order() {
        let (_mem, mut procs) = table(3);
        let mut sched = Fifo;
        assert_eq!(sched.pick(&procs), Some(1));
        procs.set_state(1, ProcState::Sleeping);
        assert_eq!(sched.pick(&procs), Some(2));
    }

    #[test]
    fn build_returns_the_named_policy() {
        assert_eq!(build(SchedPolicy::Fifo, 10).name(), "fifo");
        assert_eq!(build(SchedPolicy::Mlfq, 10).name(), "mlfq");
    }
}
