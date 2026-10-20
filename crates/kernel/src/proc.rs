//! Process table

use bytecode::Vm;

use crate::{
    fs::{FdTable, ROOT_INO},
    memory::{AddressSpace, PhysicalMemory},
    sched::SchedMeta,
    security::Credentials,
    sync::Chan,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcState {
    Runnable,
    Running,
    Sleeping,
    Zombie,
}

pub struct Proc {
    pub pid: usize,
    pub ppid: usize,
    pub name: String,
    pub state: ProcState,
    pub space: AddressSpace,
    pub vm: Vm,
    pub fds: FdTable,
    pub sched: SchedMeta,
    pub creds: Credentials,
    pub chan: Option<Chan>,
    pub wake_tick: Option<u64>,
    pub exit_code: i64,
    /// Spawn-time arguments, exposed through `argc`/`arg` syscalls.
    pub args: Vec<Vec<u8>>,
    /// Current working directory (inode).
    pub cwd: u16,
    /// Absolute path of `cwd`.
    pub cwd_path: String,
}

impl Proc {
    pub fn is_alive(&self) -> bool {
        self.state != ProcState::Zombie
    }
}

#[derive(Default)]
pub struct ProcTable {
    slots: Vec<Option<Proc>>,
    next_pid: usize,
    next_enqueue: u64,
}

impl ProcTable {
    pub fn new() -> Self {
        Self {
            slots: Vec::new(),
            next_pid: 1,
            next_enqueue: 0,
        }
    }

    pub fn alloc(&mut self, name: &str, space: AddressSpace, vm: Vm, creds: Credentials) -> usize {
        let pid = self.next_pid;
        self.next_pid += 1;
        let enqueued = self.next_enqueue;
        self.next_enqueue += 1;
        self.slots.push(Some(Proc {
            pid,
            ppid: 0,
            name: name.to_string(),
            state: ProcState::Runnable,
            space,
            vm,
            fds: FdTable::with_std(),
            sched: SchedMeta::new(enqueued),
            creds,
            chan: None,
            wake_tick: None,
            exit_code: 0,
            args: Vec::new(),
            cwd: ROOT_INO,
            cwd_path: "/".to_string(),
        }));
        pid
    }

    /// Insert a forked child: inherits the parent's fd table (already retained)
    /// and args, with a fresh pid and schedule metadata.
    pub fn alloc_forked(
        &mut self,
        parent: &Proc,
        space: AddressSpace,
        vm: Vm,
        fds: FdTable,
    ) -> usize {
        let pid = self.next_pid;
        self.next_pid += 1;
        let enqueued = self.next_enqueue;
        self.next_enqueue += 1;
        self.slots.push(Some(Proc {
            pid,
            ppid: parent.pid,
            name: parent.name.clone(),
            state: ProcState::Runnable,
            space,
            vm,
            fds,
            sched: SchedMeta::new(enqueued),
            creds: parent.creds,
            chan: None,
            wake_tick: None,
            exit_code: 0,
            args: parent.args.clone(),
            cwd: parent.cwd,
            cwd_path: parent.cwd_path.clone(),
        }));
        pid
    }

    fn index_of(&self, pid: usize) -> Option<usize> {
        self.slots
            .iter()
            .position(|slot| slot.as_ref().map(|p| p.pid == pid).unwrap_or(false))
    }

    pub fn take(&mut self, pid: usize) -> Option<Proc> {
        let index = self.index_of(pid)?;
        self.slots[index].take()
    }

    pub fn put(&mut self, pid: usize, proc: Proc) {
        match self.index_of(pid) {
            Some(index) => self.slots[index] = Some(proc),
            None => self.slots.push(Some(proc)),
        }
    }

    pub fn get(&self, pid: usize) -> Option<&Proc> {
        self.slots
            .iter()
            .filter_map(|slot| slot.as_ref())
            .find(|p| p.pid == pid)
    }

    pub fn get_mut(&mut self, pid: usize) -> Option<&mut Proc> {
        self.slots
            .iter_mut()
            .filter_map(|slot| slot.as_mut())
            .find(|p| p.pid == pid)
    }

    pub fn pids(&self) -> Vec<usize> {
        self.slots
            .iter()
            .filter_map(|slot| slot.as_ref().map(|p| p.pid))
            .collect()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Proc> {
        self.slots.iter().filter_map(|slot| slot.as_ref())
    }

    pub fn count(&self) -> usize {
        self.iter().count()
    }

    pub fn set_state(&mut self, pid: usize, state: ProcState) {
        if let Some(proc) = self.get_mut(pid) {
            proc.state = state;
        }
    }

    /// Make every process sleeping on `chan` runnable
    pub fn wake_channel(&mut self, chan: Chan) -> Vec<usize> {
        let mut woken = Vec::new();

        for proc in self.slots.iter_mut().flatten() {
            if proc.state == ProcState::Sleeping && proc.chan == Some(chan) {
                proc.state = ProcState::Runnable;
                proc.chan = None;
                woken.push(proc.pid);
            }
        }

        woken
    }

    pub fn wake_sleepers(&mut self, now: u64) -> Vec<usize> {
        let mut woken = Vec::new();

        for proc in self.slots.iter_mut().flatten() {
            if proc.state == ProcState::Sleeping
                && let Some(deadline) = proc.wake_tick
                && deadline <= now
            {
                proc.state = ProcState::Runnable;
                proc.wake_tick = None;
                proc.chan = None;
                woken.push(proc.pid);
            }
        }

        woken
    }

    /// Free a zombies address space. Returns true if reaped.
    pub fn reap(&mut self, pid: usize, mem: &mut PhysicalMemory) -> bool {
        let Some(index) = self.index_of(pid) else {
            return false;
        };
        let Some(proc) = &self.slots[index] else {
            return false;
        };

        if proc.state != ProcState::Zombie {
            return false;
        }

        proc.space.free(mem);
        self.slots[index] = None;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::AddressSpace;
    use bytecode::{Op, Program, Vm};

    fn make(mem: &mut PhysicalMemory, _name: &str) -> (AddressSpace, Vm) {
        let space = AddressSpace::new(mem).unwrap();
        let vm = Vm::new(Program::new(vec![Op::Halt]), 0);
        (space, vm)
    }

    #[test]
    fn alloc_and_wake() {
        let mut mem = PhysicalMemory::new(16);
        let mut table = ProcTable::new();
        let (s, v) = make(&mut mem, "a");
        let pid = table.alloc("a", s, v, Credentials::default());
        assert_eq!(table.count(), 1);

        table.get_mut(pid).unwrap().state = ProcState::Sleeping;
        table.get_mut(pid).unwrap().chan = Some(Chan::CONSOLE);
        assert_eq!(table.wake_channel(Chan::CONSOLE), vec![pid]);
        assert_eq!(table.get(pid).unwrap().state, ProcState::Runnable);
        assert!(table.wake_channel(Chan::CONSOLE).is_empty());
    }

    #[test]
    fn timed_wake() {
        let mut mem = PhysicalMemory::new(16);
        let mut table = ProcTable::new();
        let (s, v) = make(&mut mem, "a");
        let pid = table.alloc("a", s, v, Credentials::default());
        {
            let proc = table.get_mut(pid).unwrap();
            proc.state = ProcState::Sleeping;
            proc.wake_tick = Some(10);
        }
        assert!(table.wake_sleepers(9).is_empty());
        assert_eq!(table.wake_sleepers(10), vec![pid]);
        assert_eq!(table.get(pid).unwrap().state, ProcState::Runnable);
    }

    #[test]
    fn take_and_put() {
        let mut mem = PhysicalMemory::new(16);
        let mut table = ProcTable::new();
        let (s, v) = make(&mut mem, "a");
        let pid = table.alloc("a", s, v, Credentials::default());
        let mut proc = table.take(pid).unwrap();
        assert!(table.get(pid).is_none());
        proc.exit_code = 3;
        table.put(pid, proc);
        assert_eq!(table.get(pid).unwrap().exit_code, 3);
    }

    #[test]
    fn processes_start_with_std_descriptors() {
        let mut mem = PhysicalMemory::new(16);
        let mut table = ProcTable::new();
        let (s, v) = make(&mut mem, "a");
        let pid = table.alloc("a", s, v, Credentials::default());
        assert_eq!(table.get(pid).unwrap().fds.len(), 3);
    }

    #[test]
    fn reap() {
        let mut mem = PhysicalMemory::new(32);
        let mut table = ProcTable::new();
        let (s, v) = make(&mut mem, "a");
        let pid = table.alloc("a", s, v, Credentials::default());
        table.set_state(pid, ProcState::Zombie);
        let before = mem.free_frames();
        assert!(table.reap(pid, &mut mem));
        assert_eq!(table.count(), 0);
        assert_eq!(mem.free_frames(), before + 1); // root table page returned
    }
}
