use std::collections::{HashMap, VecDeque};

use bytecode::{Host, MemError, MemWidth, ProgramImage, SyscallResult, Vm};

use crate::{
    concurrency::Workers,
    config::KernelConfig,
    dev::{Console, Disk, Interrupt, InterruptController, Timer},
    exec::{self, ExecError},
    fs::{self, Fd, FdTable, File, FileTable, FsError, PipeEnd, T_DIR, T_FILE, Vfs},
    hardware::Hardware,
    memory::{AddressSpace, MemoryError, PhysicalMemory},
    proc::{Proc, ProcState, ProcTable},
    sched::{self, SchedPolicy, Scheduler},
    security::Credentials,
    sync::Chan,
    syscall::{
        self, O_ACCMODE, O_CREAT, O_RDONLY, O_TRUNC, O_WRONLY, ReadOutcome, SEEK_CUR, SEEK_END,
        SEEK_SET, SyscallEnv, SyscallError, WaitOutcome, WriteOutcome,
    },
    trap::TrapFrame,
};

/// Simulated disk latency in ticks: a file read/write blocks this long before
/// the "interrupt" completes it and wakes the process.
pub const DISK_LATENCY: u64 = 4;

/// Scheduler ticks per simulated second, for `uptime`/`date`.
pub const TICKS_PER_SEC: u64 = 1;

/// Unix timestamp (seconds) for the simulated epoch: 1990-01-01 00:00:00 UTC.
pub const EPOCH_1990: i64 = 631_152_000;

/// Kernel version string reported by `uname`.
pub const KERNEL_VERSION: &str = "0.9.1";
/// Kernel build tag reported by `uname`.
pub const KERNEL_BUILD: &str = "#1 1990";
/// Architecture / machine name reported by `uname`.
pub const KERNEL_MACHINE: &str = "k1.1";

/// Convert Unix seconds to `(year, month, day, hour, min, sec)` in UTC.
/// Hinnant's algorithm.
fn unix_to_civil(secs: i64) -> (i64, i64, i64, i64, i64, i64) {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (hour, min, sec) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    (y, m, d, hour, min, sec)
}

/// Largest single read/write the disk path services in one request.
pub const DISK_MAX_XFER: usize = 4096;

/// Pipe buffer capacity in bytes e.g PIPESZ.
pub const PIPE_SIZE: usize = 512;

/// Channel base for per-pipe wakeups.
pub const PIPE_CHAN_BASE: u64 = 0x1000;

fn reader_chan(id: usize) -> Chan {
    Chan(PIPE_CHAN_BASE + (id as u64) * 2)
}

fn writer_chan(id: usize) -> Chan {
    Chan(PIPE_CHAN_BASE + (id as u64) * 2 + 1)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpawnError {
    OutOfMemory,
    BadImage,
    NotFound,
}

impl From<ExecError> for SpawnError {
    fn from(err: ExecError) -> Self {
        match err {
            ExecError::OutOfMemory => SpawnError::OutOfMemory,
            ExecError::BadImage => SpawnError::BadImage,
        }
    }
}

impl From<FsError> for SpawnError {
    fn from(err: FsError) -> Self {
        match err {
            FsError::NoSpace => SpawnError::OutOfMemory,
            FsError::NotFound => SpawnError::NotFound,
            _ => SpawnError::BadImage,
        }
    }
}

/// A pending disk request, completed `DISK_LATENCY` ticks after enqueue.
enum DiskOp {
    Read {
        inum: u16,
        offset: u32,
        len: usize,
    },
    Write {
        inum: u16,
        offset: u32,
        data: Vec<u8>,
    },
}

struct DiskReq {
    pid: usize,
    op: DiskOp,
    deadline: u64,
}

/// Outcome of a completed disk request, consumed by the retried syscall.
enum DiskResult {
    Read(Vec<u8>),
    Write(usize),
    Error,
}

/// Queue of in-flight disk requests plus per-pid completed results.
#[derive(Default)]
struct DiskState {
    queue: VecDeque<DiskReq>,
    results: HashMap<usize, DiskResult>,
}

impl DiskState {
    fn enqueue(&mut self, pid: usize, op: DiskOp, now: u64) {
        self.queue.push_back(DiskReq {
            pid,
            op,
            deadline: now + DISK_LATENCY,
        });
    }

    fn take_result(&mut self, pid: usize) -> Option<DiskResult> {
        self.results.remove(&pid)
    }
}

/// A kernel pipe buffer with open-end refcounts.
struct Pipe {
    buf: VecDeque<u8>,
    read_refs: usize,
    write_refs: usize,
}

impl Pipe {
    fn new() -> Self {
        Self {
            buf: VecDeque::new(),
            read_refs: 1,
            write_refs: 1,
        }
    }
}

/// A loaded replacement image stashed by `exec`, applied once the quantum ends.
struct ExecRequest {
    space: AddressSpace,
    vm: Vm,
    args: Vec<Vec<u8>>,
}

/// Lightweight child info for `wait`, snapshotted before each quantum.
#[derive(Clone, Copy, Debug)]
struct ChildInfo {
    pid: usize,
    state: ProcState,
    exit_code: i64,
}

/// Snapshot of one process for the `ps` syscall, taken before each quantum.
#[derive(Clone, Debug)]
struct ProcSnapshot {
    pid: usize,
    ppid: usize,
    name: String,
    state: ProcState,
    uid: u32,
}

struct RunHost<'a> {
    mem: &'a mut PhysicalMemory,
    space: &'a mut AddressSpace,
    fds: &'a mut FdTable,
    files: &'a mut FileTable,
    console: &'a mut Console,
    disk: &'a mut Disk,
    disk_state: &'a mut DiskState,
    pipes: &'a mut Vec<Option<Pipe>>,
    args: &'a [Vec<u8>],
    children: &'a [ChildInfo],
    cwd: &'a mut u16,
    cwd_path: &'a mut String,
    /// The running process's credentials (`getuid`/`getgid`).
    creds: Credentials,
    hostname: &'a str,
    /// Kernel ring buffer for `dmesg`.
    klog: &'a [u8],
    proc_snapshot: &'a [ProcSnapshot],
    pid: usize,
    now: u64,
    block_chan: Option<Chan>,
    sleep_until: Option<u64>,
    yielded: bool,
    /// Set when `fork` is called; the child is built in `run_proc`.
    fork_requested: bool,
    /// Set when `exec` succeeds; the image swap happens in `run_proc`.
    exec_request: Option<ExecRequest>,
    /// Set when `wait` reaps a zombie; the slot is cleared in `run_proc`.
    wait_reap: Option<usize>,
    /// Set by `kill`; the target is zombified in `run_proc`.
    kill_request: Option<usize>,
}

impl Host for RunHost<'_> {
    fn syscall(&mut self, number: u16, args: &[i64]) -> SyscallResult {
        syscall::dispatch(self, number, args)
    }

    fn load(&mut self, addr: usize, width: MemWidth) -> Result<u64, MemError> {
        let n = width.bytes();
        let mut buf = [0u8; 8];

        self.space
            .read(self.mem, addr, &mut buf[..n])
            .map_err(|_| MemError::Unmapped)?;
        Ok(u64::from_le_bytes(buf))
    }

    fn store(&mut self, addr: usize, width: MemWidth, value: u64) -> Result<(), MemError> {
        let n = width.bytes();
        match self.space.write(self.mem, addr, &value.to_le_bytes()[..n]) {
            Ok(()) => Ok(()),
            Err(MemoryError::NotWritable) => Err(MemError::NotWritable),
            Err(_) => Err(MemError::Unmapped),
        }
    }
}

impl SyscallEnv for RunHost<'_> {
    fn pid(&self) -> usize {
        self.pid
    }

    fn copyin(&self, va: usize, len: usize) -> Result<Vec<u8>, SyscallError> {
        let mut buf = vec![0u8; len];
        self.space
            .read(self.mem, va, &mut buf)
            .map_err(|_| SyscallError::Fault)?;
        Ok(buf)
    }

    fn copyout(&mut self, va: usize, data: &[u8]) -> Result<(), SyscallError> {
        self.space
            .write(self.mem, va, data)
            .map_err(|_| SyscallError::Fault)
    }

    fn read(&mut self, fd: Fd, max: usize) -> Result<ReadOutcome, SyscallError> {
        let (id, file) = self.open_file(fd)?;
        match file {
            File::ConsoleIn => {
                if self.console.has_input() {
                    Ok(ReadOutcome::Data(self.console.read(max)))
                } else {
                    Ok(ReadOutcome::Blocked(Chan::CONSOLE))
                }
            }
            File::ConsoleOut => Err(SyscallError::BadFd),
            File::Pipe { pipe, end } => {
                if end != PipeEnd::Read {
                    return Err(SyscallError::BadFd);
                }
                self.read_pipe(pipe, max)
            }
            File::Inode {
                inum,
                offset,
                readable,
                ..
            } => {
                if !readable {
                    return Err(SyscallError::BadFd);
                }
                if let Some(result) = self.disk_state.take_result(self.pid) {
                    return match result {
                        DiskResult::Read(data) => {
                            self.advance_offset(id, data.len() as u32);
                            Ok(ReadOutcome::Data(data))
                        }
                        DiskResult::Write(_) => Err(SyscallError::Fault),
                        DiskResult::Error => Err(SyscallError::Fault),
                    };
                }
                let len = max.min(DISK_MAX_XFER);
                self.disk_state
                    .enqueue(self.pid, DiskOp::Read { inum, offset, len }, self.now);
                Ok(ReadOutcome::Blocked(Chan::DISK))
            }
        }
    }

    fn write(&mut self, fd: Fd, data: &[u8]) -> Result<WriteOutcome, SyscallError> {
        let (id, file) = self.open_file(fd)?;
        match file {
            File::ConsoleOut => {
                self.console.write(data);
                Ok(WriteOutcome::Written(data.len()))
            }
            File::ConsoleIn => Err(SyscallError::BadFd),
            File::Pipe { pipe, end } => {
                if end != PipeEnd::Write {
                    return Err(SyscallError::BadFd);
                }
                self.write_pipe(pipe, data)
            }
            File::Inode {
                inum,
                offset,
                writable,
                ..
            } => {
                if !writable {
                    return Err(SyscallError::BadFd);
                }
                if let Some(result) = self.disk_state.take_result(self.pid) {
                    return match result {
                        DiskResult::Write(len) => {
                            self.advance_offset(id, len as u32);
                            Ok(WriteOutcome::Written(len))
                        }
                        DiskResult::Read(_) => Err(SyscallError::Fault),
                        DiskResult::Error => Err(SyscallError::Fault),
                    };
                }
                let chunk = data.len().min(DISK_MAX_XFER);
                self.disk_state.enqueue(
                    self.pid,
                    DiskOp::Write {
                        inum,
                        offset,
                        data: data[..chunk].to_vec(),
                    },
                    self.now,
                );
                Ok(WriteOutcome::Blocked(Chan::DISK))
            }
        }
    }

    fn open(&mut self, path_va: usize, flags: i64) -> Result<Fd, SyscallError> {
        let path = self
            .space
            .read_cstr(self.mem, path_va, fs::MAX_PATH)
            .map_err(|_| SyscallError::Fault)?;
        match path.as_slice() {
            b"console" | b"tty" | b"/dev/console" => {
                let id = self.files.alloc(File::ConsoleOut);
                return Ok(self.fds.alloc(id));
            }
            _ => {}
        }

        let mut vfs = Vfs::mount(self.disk).map_err(map_fs_err)?;
        let accmode = flags & O_ACCMODE;
        let path_str = String::from_utf8_lossy(&path).to_string();
        let inum = if flags & O_CREAT != 0 {
            // Resolve the parent relative to cwd, then create the final name.
            vfs.touch_from(*self.cwd, &path_str).map_err(map_fs_err)?
        } else {
            vfs.namei_from(*self.cwd, &path_str)
                .map_err(map_fs_err)?
                .ok_or(SyscallError::NotFound)?
        };
        let kind = vfs.kind(inum).map_err(map_fs_err)?;
        if kind == T_DIR && accmode != O_RDONLY {
            return Err(SyscallError::Inval);
        }
        if flags & O_TRUNC != 0 && kind == T_FILE {
            vfs.truncate(inum).map_err(map_fs_err)?;
        }
        let id = self.files.alloc(File::Inode {
            inum,
            offset: 0,
            readable: accmode != O_WRONLY,
            writable: accmode != O_RDONLY,
        });
        Ok(self.fds.alloc(id))
    }

    fn close(&mut self, fd: Fd) -> Result<(), SyscallError> {
        let id = self.fds.close(fd).ok_or(SyscallError::BadFd)?;
        // Drop pipe end refcounts before releasing the file entry.
        if let Some(File::Pipe { pipe, end }) = self.files.get(id)
            && let Some(entry) = self.pipes.get_mut(pipe).and_then(|p| p.as_mut())
        {
            match end {
                PipeEnd::Read => entry.read_refs = entry.read_refs.saturating_sub(1),
                PipeEnd::Write => entry.write_refs = entry.write_refs.saturating_sub(1),
            }
        }
        self.files.release(id);
        Ok(())
    }

    fn dup(&mut self, fd: Fd) -> Result<Fd, SyscallError> {
        let id = self.fds.get(fd).ok_or(SyscallError::BadFd)?;
        self.files.retain(id);
        Ok(self.fds.alloc(id))
    }

    fn dup2(&mut self, oldfd: Fd, newfd: Fd) -> Result<Fd, SyscallError> {
        if oldfd < 0 || newfd < 0 {
            return Err(SyscallError::Inval);
        }
        let id = self.fds.get(oldfd).ok_or(SyscallError::BadFd)?;
        if oldfd == newfd {
            return Ok(newfd);
        }
        // Place oldfd's file at newfd, closing any occupant first.
        if let Some(prev) = self.fds.alloc_at(newfd, id) {
            self.files.release(prev);
        }
        self.files.retain(id);
        Ok(newfd)
    }

    fn fork(&mut self) -> SyscallResult {
        self.fork_requested = true;
        // Return value is patched to the child pid in run_proc; 0 here is the
        // placeholder the child will see (fork returns 0 to the child).
        SyscallResult::continuing_and_stop(0)
    }

    fn pipe(&mut self, fds_va: usize) -> Result<(), SyscallError> {
        let id = self.pipes.len();
        self.pipes.push(Some(Pipe::new()));
        let rid = self.files.alloc(File::Pipe {
            pipe: id,
            end: PipeEnd::Read,
        });
        let wid = self.files.alloc(File::Pipe {
            pipe: id,
            end: PipeEnd::Write,
        });
        let rfd = self.fds.alloc(rid);
        let wfd = self.fds.alloc(wid);
        // Each fd is a `sword` (4 bytes); write the two into the user array.
        let mut buf = [0u8; 8];
        buf[0..4].copy_from_slice(&rfd.to_le_bytes());
        buf[4..8].copy_from_slice(&wfd.to_le_bytes());
        self.copyout(fds_va, &buf).map_err(|_| SyscallError::Fault)
    }

    fn wait(&mut self, status_va: usize) -> Result<WaitOutcome, SyscallError> {
        // Find a zombie child in the snapshot taken before this quantum.
        let Some(zombie) = self.children.iter().find(|c| c.state == ProcState::Zombie) else {
            if !self.children.is_empty() {
                return Ok(WaitOutcome::Blocked(Chan::WAIT));
            }
            return Err(SyscallError::Inval); // no children
        };
        let pid = zombie.pid;
        let code = zombie.exit_code;
        self.copyout(status_va, &code.to_le_bytes()[..4])
            .map_err(|_| SyscallError::Fault)?;
        self.wait_reap = Some(pid);
        Ok(WaitOutcome::Done(pid as i64))
    }

    fn exec(&mut self, path_va: usize, argc: i64, argv_va: usize) -> SyscallResult {
        match self.try_exec(path_va, argc, argv_va) {
            Ok(req) => {
                self.exec_request = Some(req);
                SyscallResult::continuing_and_stop(0)
            }
            Err(_) => SyscallResult::continuing(-1),
        }
    }

    fn chdir(&mut self, path_va: usize) -> Result<(), SyscallError> {
        let path = self
            .space
            .read_cstr(self.mem, path_va, fs::MAX_PATH)
            .map_err(|_| SyscallError::Fault)?;
        let path = String::from_utf8_lossy(&path).to_string();
        let vfs = Vfs::mount(self.disk).map_err(map_fs_err)?;
        let inum = vfs
            .namei_from(*self.cwd, &path)
            .map_err(map_fs_err)?
            .ok_or(SyscallError::NotFound)?;
        if vfs.kind(inum).map_err(map_fs_err)? != T_DIR {
            return Err(SyscallError::Inval);
        }
        let path = vfs.path_of(inum).map_err(map_fs_err)?;
        *self.cwd = inum;
        *self.cwd_path = path;
        Ok(())
    }

    fn lseek(&mut self, fd: Fd, offset: i64, whence: i64) -> Result<i64, SyscallError> {
        let id = self.fds.get(fd).ok_or(SyscallError::BadFd)?;
        let Some(File::Inode {
            inum,
            offset: cur,
            readable,
            writable,
        }) = self.files.get(id)
        else {
            return Err(SyscallError::BadFd);
        };
        let base = match whence {
            SEEK_SET => 0i64,
            SEEK_CUR => i64::from(cur),
            SEEK_END => {
                let vfs = Vfs::mount(self.disk).map_err(map_fs_err)?;
                i64::from(vfs.stat(inum).map_err(map_fs_err)?.size)
            }
            _ => return Err(SyscallError::Inval),
        };
        let next = base + offset;
        if next < 0 || next > i64::from(fs::MAX_FILE) {
            return Err(SyscallError::Inval);
        }
        self.files.set(
            id,
            File::Inode {
                inum,
                offset: next as u32,
                readable,
                writable,
            },
        );
        Ok(next)
    }

    fn unlink(&mut self, path_va: usize) -> Result<(), SyscallError> {
        let path = self
            .space
            .read_cstr(self.mem, path_va, fs::MAX_PATH)
            .map_err(|_| SyscallError::Fault)?;
        let mut vfs = Vfs::mount(self.disk).map_err(map_fs_err)?;
        vfs.unlink(&String::from_utf8_lossy(&path))
            .map_err(map_fs_err)
    }

    fn mkdir(&mut self, path_va: usize) -> Result<(), SyscallError> {
        let path = self
            .space
            .read_cstr(self.mem, path_va, fs::MAX_PATH)
            .map_err(|_| SyscallError::Fault)?;
        let mut vfs = Vfs::mount(self.disk).map_err(map_fs_err)?;
        vfs.create(&String::from_utf8_lossy(&path), T_DIR)
            .map(|_| ())
            .map_err(map_fs_err)
    }

    fn stat(&mut self, path_va: usize, out_va: usize) -> Result<(), SyscallError> {
        let path = self
            .space
            .read_cstr(self.mem, path_va, fs::MAX_PATH)
            .map_err(|_| SyscallError::Fault)?;
        let path = String::from_utf8_lossy(&path).to_string();
        let vfs = Vfs::mount(self.disk).map_err(map_fs_err)?;
        let inum = vfs
            .namei_from(*self.cwd, &path)
            .map_err(map_fs_err)?
            .ok_or(SyscallError::NotFound)?;
        let stat = vfs.stat(inum).map_err(map_fs_err)?;
        let mut buf = [0u8; 8];
        buf[0..4].copy_from_slice(&u32::from(stat.kind).to_le_bytes());
        buf[4..8].copy_from_slice(&stat.size.to_le_bytes());
        self.copyout(out_va, &buf)
    }

    fn getcwd(&mut self, buf_va: usize, max: usize) -> Result<i64, SyscallError> {
        let path = self.cwd_path.as_bytes().to_vec();
        let take = path.len().min(max);
        self.copyout(buf_va, &path[..take])?;
        Ok(take as i64)
    }

    fn getuid(&self) -> i64 {
        i64::from(self.creds.uid)
    }

    fn getgid(&self) -> i64 {
        i64::from(self.creds.gid)
    }

    fn uptime(&self) -> i64 {
        (self.now / TICKS_PER_SEC) as i64
    }

    fn uname(&mut self, buf_va: usize) -> Result<(), SyscallError> {
        let fields: [&str; 5] = [
            "SIGNULL",
            self.hostname,
            KERNEL_VERSION,
            KERNEL_BUILD,
            KERNEL_MACHINE,
        ];
        let mut buf = vec![0u8; bytecode::abi::UTSNAME_LEN];
        for (i, field) in fields.iter().enumerate() {
            let start = i * bytecode::abi::UTS_FIELD_LEN;
            let bytes = field.as_bytes();
            let take = bytes.len().min(bytecode::abi::UTS_FIELD_LEN - 1);
            buf[start..start + take].copy_from_slice(&bytes[..take]);
        }
        self.copyout(buf_va, &buf)
    }

    fn statfs(&mut self, buf_va: usize) -> Result<(), SyscallError> {
        let vfs = Vfs::mount(self.disk).map_err(map_fs_err)?;
        let (total_b, free_b, total_i, free_i) = vfs.statfs().map_err(map_fs_err)?;
        let block_size = self.disk.block_size() as u32;
        let mut buf = vec![0u8; bytecode::abi::STATFS_LEN];
        for (i, value) in [total_b, free_b, total_i, free_i, block_size]
            .iter()
            .enumerate()
        {
            buf[i * 4..i * 4 + 4].copy_from_slice(&value.to_le_bytes());
        }
        self.copyout(buf_va, &buf)
    }

    fn ps(&mut self, buf_va: usize, max: usize) -> Result<i64, SyscallError> {
        let ent_len = bytecode::abi::PROCENT_LEN;
        let name_len = bytecode::abi::PROC_NAME_LEN;
        let capacity = max / ent_len;
        let mut buf = Vec::with_capacity(capacity * ent_len);
        let mut count = 0i64;
        for proc in self.proc_snapshot {
            if count as usize >= capacity {
                break;
            }
            let mut ent = vec![0u8; ent_len];
            ent[0..4].copy_from_slice(&(proc.pid as i32).to_le_bytes());
            ent[4..8].copy_from_slice(&(proc.ppid as i32).to_le_bytes());
            ent[8..12].copy_from_slice(&proc.uid.to_le_bytes());
            ent[12] = match proc.state {
                ProcState::Runnable => bytecode::abi::PS_RUNNABLE,
                ProcState::Running => bytecode::abi::PS_RUNNING,
                ProcState::Sleeping => bytecode::abi::PS_SLEEPING,
                ProcState::Zombie => bytecode::abi::PS_ZOMBIE,
            };
            let bytes = proc.name.as_bytes();
            let take = bytes.len().min(name_len - 1);
            ent[13..13 + take].copy_from_slice(&bytes[..take]);
            buf.extend_from_slice(&ent);
            count += 1;
        }
        self.copyout(buf_va, &buf)?;
        Ok(count)
    }

    fn kill(&mut self, pid: i64) -> Result<(), SyscallError> {
        if pid <= 0 {
            return Err(SyscallError::Inval);
        }
        let target = pid as usize;
        if !self.proc_snapshot.iter().any(|p| p.pid == target) {
            return Err(SyscallError::NotFound);
        }
        self.kill_request = Some(target);
        Ok(())
    }

    fn dmesg(&mut self, buf_va: usize, max: usize) -> Result<i64, SyscallError> {
        let take = self.klog.len().min(max);
        self.copyout(buf_va, &self.klog[..take])?;
        Ok(take as i64)
    }

    fn date(&mut self, buf_va: usize) -> Result<(), SyscallError> {
        let secs = EPOCH_1990 + (self.now / TICKS_PER_SEC) as i64;
        let (year, mon, day, hour, min, sec) = unix_to_civil(secs);
        let mut buf = vec![0u8; bytecode::abi::DATE_LEN];
        for (i, value) in [year, mon, day, hour, min, sec].iter().enumerate() {
            buf[i * 4..i * 4 + 4].copy_from_slice(&(*value as u32).to_le_bytes());
        }
        self.copyout(buf_va, &buf)
    }

    fn nblocks(&mut self, path_va: usize) -> Result<i64, SyscallError> {
        let path = self
            .space
            .read_cstr(self.mem, path_va, fs::MAX_PATH)
            .map_err(|_| SyscallError::Fault)?;
        let path = String::from_utf8_lossy(&path).to_string();
        let vfs = Vfs::mount(self.disk).map_err(map_fs_err)?;
        let blocks = vfs.path_blocks(&path).map_err(map_fs_err)?;
        Ok(i64::from(blocks))
    }

    fn argc(&self) -> i64 {
        self.args.len() as i64
    }

    fn arg(&mut self, index: i64, buf_va: usize, max: usize) -> Result<i64, SyscallError> {
        let index = usize::try_from(index).map_err(|_| SyscallError::Inval)?;
        let arg = self.args.get(index).ok_or(SyscallError::Inval)?;
        let take = arg.len().min(max);
        self.copyout(buf_va, &arg[..take])?;
        Ok(take as i64)
    }

    fn sbrk(&mut self, delta: isize) -> Result<usize, SyscallError> {
        self.space
            .sbrk(self.mem, delta)
            .map_err(|_| SyscallError::Fault)
    }

    fn exit(&mut self, _code: i64) {}

    fn block(&mut self, chan: Chan) {
        self.block_chan = Some(chan);
    }

    fn sleep(&mut self, ticks: u64) {
        self.sleep_until = Some(self.now + ticks);
    }

    fn yield_now(&mut self) {
        self.yielded = true;
    }
}

fn map_fs_err(err: FsError) -> SyscallError {
    match err {
        FsError::NotFound => SyscallError::NotFound,
        FsError::NoSpace => SyscallError::NoSpace,
        FsError::Corrupt => SyscallError::Fault,
        _ => SyscallError::Inval,
    }
}

impl RunHost<'_> {
    fn open_file(&self, fd: Fd) -> Result<(usize, File), SyscallError> {
        let id = self.fds.get(fd).ok_or(SyscallError::BadFd)?;
        let file = self.files.get(id).ok_or(SyscallError::BadFd)?;
        Ok((id, file))
    }

    fn advance_offset(&mut self, id: usize, delta: u32) {
        if let Some(file) = self.files.get_mut(id)
            && let File::Inode { offset, .. } = file
        {
            *offset = offset.saturating_add(delta);
        }
    }

    fn read_pipe(&mut self, pipe: usize, max: usize) -> Result<ReadOutcome, SyscallError> {
        let (has_data, len, write_refs) = {
            let Some(entry) = self.pipes.get(pipe).and_then(|p| p.as_ref()) else {
                return Err(SyscallError::Inval);
            };
            (!entry.buf.is_empty(), entry.buf.len(), entry.write_refs)
        };
        if has_data {
            let take = max.min(len);
            let entry = self.pipes[pipe].as_mut().ok_or(SyscallError::Inval)?;
            let data: Vec<u8> = entry.buf.drain(..take).collect();
            return Ok(ReadOutcome::Data(data));
        }
        if write_refs == 0 {
            return Ok(ReadOutcome::Data(Vec::new())); // EOF
        }
        Ok(ReadOutcome::Blocked(reader_chan(pipe)))
    }

    fn write_pipe(&mut self, pipe: usize, data: &[u8]) -> Result<WriteOutcome, SyscallError> {
        let (read_refs, space) = {
            let Some(entry) = self.pipes.get(pipe).and_then(|p| p.as_ref()) else {
                return Err(SyscallError::Inval);
            };
            (entry.read_refs, PIPE_SIZE - entry.buf.len())
        };
        if read_refs == 0 {
            return Ok(WriteOutcome::Error); // broken pipe
        }
        if space == 0 {
            return Ok(WriteOutcome::Blocked(writer_chan(pipe)));
        }
        let take = space.min(data.len());
        let entry = self.pipes[pipe].as_mut().ok_or(SyscallError::Inval)?;
        entry.buf.extend(data[..take].iter().copied());
        Ok(WriteOutcome::Written(take))
    }

    fn try_exec(
        &mut self,
        path_va: usize,
        argc: i64,
        argv_va: usize,
    ) -> Result<ExecRequest, SyscallError> {
        let path = self
            .space
            .read_cstr(self.mem, path_va, fs::MAX_PATH)
            .map_err(|_| SyscallError::Fault)?;
        let path = String::from_utf8_lossy(&path).to_string();

        // Collect argv strings.
        let mut args = Vec::new();
        for i in 0..argc.max(0) {
            let slot_va = argv_va + (i as usize) * 4;
            let mut slot = [0u8; 4];
            self.space
                .read(self.mem, slot_va, &mut slot)
                .map_err(|_| SyscallError::Fault)?;
            let str_va = u32::from_le_bytes(slot) as usize;
            let s = self
                .space
                .read_cstr(self.mem, str_va, fs::MAX_PATH)
                .map_err(|_| SyscallError::Fault)?;
            args.push(s);
        }

        // Load the image from the VFS.
        let bytes = {
            let mut vfs = Vfs::mount(self.disk).map_err(|_| SyscallError::NotFound)?;
            let Some(inum) = vfs.namei(&path).map_err(|_| SyscallError::Fault)? else {
                return Err(SyscallError::NotFound);
            };
            let stat = vfs.stat(inum).map_err(|_| SyscallError::Fault)?;
            let mut buf = vec![0u8; stat.size as usize];
            let n = vfs
                .read(inum, 0, &mut buf)
                .map_err(|_| SyscallError::Fault)?;
            buf.truncate(n);
            buf
        };
        let image = bytecode::image::decode_image(&bytes).map_err(|_| SyscallError::NotFound)?;
        let loaded = exec::load_image(self.mem, &image).map_err(|_| SyscallError::Fault)?;
        let mut vm = Vm::from_image(&image, loaded.data_addrs);
        vm.set_frame_region(loaded.stack_top, loaded.stack_base);
        Ok(ExecRequest {
            space: loaded.space,
            vm,
            args,
        })
    }
}

pub struct Machine {
    pub hardware: Hardware,
    pub config: KernelConfig,
    pub mem: PhysicalMemory,
    pub procs: ProcTable,
    pub files: FileTable,
    pub console: Console,
    pub disk: Disk,
    pub timer: Timer,
    pub interrupts: InterruptController,
    pub workers: Workers,
    pub last_frame: Option<TrapFrame>,
    /// Host name reported by `uname`.
    pub hostname: String,
    /// Kernel ring buffer for `dmesg`.
    klog: Vec<u8>,
    disk_state: DiskState,
    pipes: Vec<Option<Pipe>>,
    sched: Box<dyn Scheduler>,
}

impl Machine {
    pub fn with_hardware(hardware: Hardware, config: KernelConfig) -> Self {
        let config = config.validate(&hardware);
        let workers = Workers::new(config.workers);
        let sched = sched::build(config.scheduler, config.quantum);

        let mut disk = Disk::new(hardware.disk_blocks, hardware.disk_block_size);
        // A blank disk becomes a fresh filesystem; an already-formatted disk
        // (e.g. restored from a save) is left alone.
        let _ = Vfs::format_if_needed(&mut disk);

        Self {
            mem: PhysicalMemory::new(hardware.ram_frames),
            disk,
            files: FileTable::with_std(),
            procs: ProcTable::new(),
            console: Console::new(),
            timer: Timer::new(),
            interrupts: InterruptController::new(),
            last_frame: None,
            hostname: "signull".to_owned(),
            klog: Vec::new(),
            disk_state: DiskState::default(),
            pipes: Vec::new(),
            hardware,
            config,
            workers,
            sched,
        }
    }

    /// A single core machine with default config
    pub fn new(ram_frames: usize, quantum: u64) -> Self {
        Self::with_hardware(
            Hardware::single_core(ram_frames),
            KernelConfig::new(quantum),
        )
    }

    pub fn scheduler_name(&self) -> &'static str {
        self.sched.name()
    }

    pub fn set_scheduler(&mut self, policy: SchedPolicy) {
        self.config.scheduler = policy;
        self.sched = sched::build(policy, self.config.quantum);
    }

    pub fn set_workers(&mut self, workers: usize) {
        self.config.workers = workers;
        self.config = self.config.validate(&self.hardware);
        self.workers = Workers::new(self.config.workers);
    }

    pub fn enable_multicore(&mut self, workers: usize) {
        self.config.features.multicore = true;
        self.set_workers(workers);
    }

    pub fn spawn(&mut self, image: &ProgramImage, name: &str) -> Result<usize, SpawnError> {
        self.spawn_with_args(image, name, &[])
    }

    /// Spawn a program with argv accessible to it via `argc`/`arg`.
    pub fn spawn_with_args(
        &mut self,
        image: &ProgramImage,
        name: &str,
        argv: &[&[u8]],
    ) -> Result<usize, SpawnError> {
        let loaded = exec::load_image(&mut self.mem, image).map_err(SpawnError::from)?;
        let mut vm = Vm::from_image(image, loaded.data_addrs);
        vm.set_frame_region(loaded.stack_top, loaded.stack_base);
        let pid = self
            .procs
            .alloc(name, loaded.space, vm, Credentials::default());
        if let Some(proc) = self.procs.get_mut(pid) {
            proc.args = argv.iter().map(|a| a.to_vec()).collect();
        }
        self.sched.on_ready(pid, &mut self.procs);
        Ok(pid)
    }

    /// Read a whole file from the VFS.
    pub fn read_file(&mut self, path: &str) -> Result<Vec<u8>, FsError> {
        let mut vfs = Vfs::mount(&mut self.disk)?;
        let Some(inum) = vfs.namei(path)? else {
            return Err(FsError::NotFound);
        };
        let stat = vfs.stat(inum)?;
        let mut buf = vec![0u8; stat.size as usize];
        let n = vfs.read(inum, 0, &mut buf)?;
        buf.truncate(n);
        Ok(buf)
    }

    /// Create (or truncate) a file on the VFS with the given contents
    pub fn install_file(&mut self, path: &str, data: &[u8]) -> Result<(), FsError> {
        let mut vfs = Vfs::mount(&mut self.disk)?;
        let inum = vfs.touch(path)?;
        vfs.truncate_write(inum, data)?;
        Ok(())
    }

    /// Create a directory on the VFS.
    pub fn install_dir(&mut self, path: &str) -> Result<(), FsError> {
        let mut vfs = Vfs::mount(&mut self.disk)?;
        match vfs.create(path, T_DIR) {
            Ok(_) | Err(FsError::Exist) => Ok(()),
            Err(err) => Err(err),
        }
    }

    /// Advance timer, wake/interrupt, run a process per worker.
    pub fn schedule(&mut self) -> Option<usize> {
        let now = self.timer.tick();
        self.wake(now);

        let mut ran: Vec<(usize, u64)> = Vec::new();
        for _ in 0..self.workers.count() {
            let Some(pid) = self.sched.pick(&self.procs) else {
                break;
            };
            let used = self.run_proc(pid, now);
            ran.push((pid, used));
        }

        for (pid, used) in &ran {
            if let Some(proc) = self.procs.get_mut(*pid)
                && proc.state == ProcState::Running
            {
                proc.state = ProcState::Runnable;
            }
            self.sched.on_run(*pid, *used, &mut self.procs);
        }

        ran.first().map(|(pid, _)| *pid)
    }

    fn wake(&mut self, now: u64) {
        let sleepers = self.procs.wake_sleepers(now);
        for pid in sleepers {
            self.sched.on_ready(pid, &mut self.procs);
        }

        self.complete_disk(now);
        self.wake_pipes();

        if self.console.has_input() {
            self.interrupts.raise(Interrupt::Console);
        }

        while let Some(interrupt) = self.interrupts.pop() {
            if let Interrupt::Console = interrupt {
                let woken = self.procs.wake_channel(Chan::CONSOLE);

                for pid in woken {
                    self.sched.on_ready(pid, &mut self.procs);
                }
            }
        }
    }

    /// Wake processes sleeping on pipes that now have data, space, or hit
    /// EOF / broken-pipe. Readers and writers use separate channels so a
    /// writer-ready pipe doesn't prematurely wake a blocked reader.
    fn wake_pipes(&mut self) {
        for (id, entry) in self.pipes.iter().enumerate() {
            let Some(pipe) = entry.as_ref() else { continue };
            let has_data = !pipe.buf.is_empty();
            let eof = pipe.write_refs == 0;
            let has_space = pipe.buf.len() < PIPE_SIZE;
            let broken = pipe.read_refs == 0;
            if has_data || eof {
                for pid in self.procs.wake_channel(reader_chan(id)) {
                    self.sched.on_ready(pid, &mut self.procs);
                }
            }
            if has_space || broken {
                for pid in self.procs.wake_channel(writer_chan(id)) {
                    self.sched.on_ready(pid, &mut self.procs);
                }
            }
        }
    }

    /// Complete disk requests whose simulated latency has elapsed, then wake
    /// the requesting processes — the disk "interrupt".
    fn complete_disk(&mut self, now: u64) {
        let mut i = 0;
        while i < self.disk_state.queue.len() {
            if self.disk_state.queue[i].deadline <= now {
                let req = self.disk_state.queue.remove(i).expect("in range");
                let result = match req.op {
                    DiskOp::Read { inum, offset, len } => {
                        let mut buf = vec![0u8; len];
                        match Vfs::mount(&mut self.disk) {
                            Ok(mut vfs) => match vfs.read(inum, offset, &mut buf) {
                                Ok(n) => {
                                    buf.truncate(n);
                                    DiskResult::Read(buf)
                                }
                                Err(_) => DiskResult::Error,
                            },
                            Err(_) => DiskResult::Error,
                        }
                    }
                    DiskOp::Write { inum, offset, data } => match Vfs::mount(&mut self.disk) {
                        Ok(mut vfs) => match vfs.write(inum, offset, &data) {
                            Ok(n) => DiskResult::Write(n),
                            Err(_) => DiskResult::Error,
                        },
                        Err(_) => DiskResult::Error,
                    },
                };
                let pid = req.pid;
                self.disk_state.results.insert(pid, result);
                if let Some(proc) = self.procs.get_mut(pid)
                    && proc.state == ProcState::Sleeping
                    && proc.chan == Some(Chan::DISK)
                {
                    proc.state = ProcState::Runnable;
                    proc.chan = None;
                    self.sched.on_ready(pid, &mut self.procs);
                }
            } else {
                i += 1;
            }
        }
    }

    fn run_proc(&mut self, pid: usize, now: u64) -> u64 {
        let Some(mut proc) = self.procs.take(pid) else {
            return 0;
        };

        proc.state = ProcState::Running;
        proc.vm.blocked = false;
        self.last_frame = Some(TrapFrame::capture(&proc.vm));

        let children: Vec<ChildInfo> = self
            .procs
            .iter()
            .filter(|p| p.ppid == pid)
            .map(|p| ChildInfo {
                pid: p.pid,
                state: p.state,
                exit_code: p.exit_code,
            })
            .collect();

        let mut proc_snapshot: Vec<ProcSnapshot> = Vec::with_capacity(self.procs.count() + 1);
        proc_snapshot.push(ProcSnapshot {
            pid: proc.pid,
            ppid: proc.ppid,
            name: proc.name.clone(),
            state: proc.state,
            uid: proc.creds.uid,
        });
        for p in self.procs.iter() {
            proc_snapshot.push(ProcSnapshot {
                pid: p.pid,
                ppid: p.ppid,
                name: p.name.clone(),
                state: p.state,
                uid: p.creds.uid,
            });
        }

        let creds = proc.creds;

        let (
            err,
            used,
            blocked,
            exited,
            exit_code,
            block_chan,
            sleep_until,
            fork_req,
            exec_req,
            wait_reap,
            kill_req,
        ) = {
            let mut host = RunHost {
                mem: &mut self.mem,
                space: &mut proc.space,
                fds: &mut proc.fds,
                files: &mut self.files,
                console: &mut self.console,
                disk: &mut self.disk,
                disk_state: &mut self.disk_state,
                pipes: &mut self.pipes,
                args: &proc.args,
                children: &children,
                cwd: &mut proc.cwd,
                cwd_path: &mut proc.cwd_path,
                creds,
                hostname: &self.hostname,
                klog: &self.klog,
                proc_snapshot: &proc_snapshot,
                pid,
                now,
                block_chan: None,
                sleep_until: None,
                yielded: false,
                fork_requested: false,
                exec_request: None,
                wait_reap: None,
                kill_request: None,
            };
            let outcome = proc.vm.run(&mut host, self.config.quantum);
            let used = outcome.as_ref().copied().unwrap_or(0);
            (
                outcome.err(),
                used,
                proc.vm.blocked,
                proc.vm.exited,
                proc.vm.exit_code,
                host.block_chan,
                host.sleep_until,
                host.fork_requested,
                host.exec_request,
                host.wait_reap,
                host.kill_request,
            )
        };

        // Deferred `exec`: replace the address space and VM in place.
        if let Some(req) = exec_req {
            let old = std::mem::replace(&mut proc.space, req.space);
            old.free(&mut self.mem);
            proc.vm = req.vm;
            proc.args = req.args;
        }

        let was_running = err.is_none() && !exited && !blocked;
        if let Some(err) = err {
            proc.state = ProcState::Zombie;
            proc.exit_code = -1;
            let msg = format!("pid {pid} faulted: {err}\n");
            self.console.write(msg.as_bytes());
            self.klog_line(&msg);
        } else if exited {
            proc.state = ProcState::Zombie;
            proc.exit_code = exit_code;
        } else if blocked {
            proc.state = ProcState::Sleeping;
            proc.chan = block_chan;
            proc.wake_tick = sleep_until;
        } else {
            proc.state = ProcState::Running;
            proc.sched.last_run = now;
            proc.sched.slice_used = used;
        }

        // A child exited: wake the parent so its `wait` can reap it.
        if proc.state == ProcState::Zombie && proc.ppid != 0 {
            let ppid = proc.ppid;
            self.wake_parent(ppid);
        }

        // Deferred `fork`: clone the parent into a child.
        if fork_req
            && was_running
            && let Some(child_pid) = self.fork_child(&mut proc)
        {
            // fork returns the child pid to the parent.
            if let Some(top) = proc.vm.stack.last_mut() {
                *top = child_pid as i64;
            }
            self.sched.on_ready(child_pid, &mut self.procs);
        }

        // Deferred `wait`: free the reaped child's resources.
        if let Some(child_pid) = wait_reap {
            self.reap_child(child_pid);
        }

        // Deferred `kill`: zombify the target and wake its parent.
        if let Some(target) = kill_req {
            self.kill_proc(target);
        }

        self.procs.put(pid, proc);
        used
    }

    fn kill_proc(&mut self, target: usize) {
        if target == 0 {
            return;
        }
        if let Some(proc) = self.procs.get_mut(target)
            && proc.state != ProcState::Zombie
        {
            proc.state = ProcState::Zombie;
            proc.exit_code = -1;
            if proc.ppid != 0 {
                let ppid = proc.ppid;
                self.wake_parent(ppid);
            }
        }
    }

    /// Clone `parent` into a new child process. Returns the child's pid.
    fn fork_child(&mut self, parent: &mut Proc) -> Option<usize> {
        let space = AddressSpace::fork_copy(&mut self.mem, &parent.space).ok()?;
        let mut vm = parent.vm.clone();
        // The child's fork return value is 0.
        if let Some(top) = vm.stack.last_mut() {
            *top = 0;
        }
        let fds = parent.fds.fork_into(&mut self.files);
        // Inherited pipe ends bump their refcounts so the parent's close
        // doesn't free an end the child still holds.
        for fid in fds.file_ids() {
            if let Some(File::Pipe { pipe, end }) = self.files.get(fid)
                && let Some(entry) = self.pipes.get_mut(pipe).and_then(|p| p.as_mut())
            {
                match end {
                    PipeEnd::Read => entry.read_refs += 1,
                    PipeEnd::Write => entry.write_refs += 1,
                }
            }
        }
        let child_pid = self.procs.alloc_forked(parent, space, vm, fds);
        Some(child_pid)
    }

    /// Free a reaped child's address space and remove it from the table.
    fn reap_child(&mut self, pid: usize) {
        if let Some(child) = self.procs.take(pid) {
            child.space.free(&mut self.mem);
        }
    }

    /// Wake a parent sleeping on `wait` when one of its children exits.
    fn wake_parent(&mut self, ppid: usize) {
        if let Some(parent) = self.procs.get_mut(ppid)
            && parent.state == ProcState::Sleeping
            && parent.chan == Some(Chan::WAIT)
        {
            parent.state = ProcState::Runnable;
            parent.chan = None;
            self.sched.on_ready(ppid, &mut self.procs);
        }
    }

    pub fn run_steps(&mut self, ticks: usize) {
        for _ in 0..ticks {
            self.schedule();
            if self.procs.iter().all(|p| !p.is_alive()) {
                break;
            }
        }
    }

    /// Append a line to the kernel ring buffer (for `dmesg`).
    pub fn klog_line(&mut self, line: &str) {
        const KLOG_CAP: usize = 4096;
        for b in line.bytes() {
            if self.klog.len() >= KLOG_CAP {
                self.klog.remove(0);
            }
            self.klog.push(b);
        }
    }

    pub fn reap_zombies(&mut self) -> usize {
        let zombies: Vec<usize> = self
            .procs
            .iter()
            .filter(|p| {
                p.state == ProcState::Zombie
                    && (p.ppid == 0
                        || self
                            .procs
                            .get(p.ppid)
                            .is_none_or(|parent| !parent.is_alive()))
            })
            .map(|p| p.pid)
            .collect();

        let mut reaped = 0;
        for pid in zombies {
            if self.procs.reap(pid, &mut self.mem) {
                reaped += 1;
            }
        }

        reaped
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FeatureFlags;
    use crate::syscall::{SYS_EXIT, SYS_OPEN, SYS_READ, SYS_WRITE};
    use bytecode::Op;

    fn write_then_exit(data: &[u8]) -> ProgramImage {
        let code = vec![
            Op::Push(1),
            Op::PushData(0),
            Op::Push(data.len() as i64),
            Op::Syscall(SYS_WRITE, 3),
            Op::Push(0),
            Op::Syscall(SYS_EXIT, 1),
            Op::Halt,
        ];
        ProgramImage::new(code, 0).with_data(vec![data.to_vec()])
    }

    #[test]
    fn write_and_exit() {
        let mut machine = Machine::new(256, 1000);
        let pid = machine.spawn(&write_then_exit(b"hi\n"), "hello").unwrap();
        assert_eq!(machine.schedule(), Some(pid));
        assert_eq!(machine.console.output(), b"hi\n");
        assert_eq!(machine.procs.get(pid).unwrap().state, ProcState::Zombie);
        assert_eq!(machine.procs.get(pid).unwrap().exit_code, 0);
    }

    #[test]
    fn read_blocks_until_input_then_wakes() {
        let mut machine = Machine::new(256, 1000);
        let code = vec![
            Op::Push(0),
            Op::PushData(0),
            Op::Push(4),
            Op::Syscall(SYS_READ, 3),
            Op::Push(1),
            Op::PushData(0),
            Op::Push(4),
            Op::Syscall(SYS_WRITE, 3),
            Op::Push(0),
            Op::Syscall(SYS_EXIT, 1),
            Op::Halt,
        ];
        let image = ProgramImage::new(code, 0).with_data(vec![vec![0u8; 4]]);
        let pid = machine.spawn(&image, "cat").unwrap();

        machine.schedule();
        assert_eq!(machine.procs.get(pid).unwrap().state, ProcState::Sleeping);
        assert!(machine.console.output().is_empty());

        machine.console.push_bytes(b"abcd");
        machine.schedule();
        assert_eq!(machine.console.output(), b"abcd");
        assert_eq!(machine.procs.get(pid).unwrap().state, ProcState::Zombie);
    }

    #[test]
    fn store_load_goes_to_the_address_space() {
        // data[0] = 0x2a via Store8, then write it back to the console.
        let code = vec![
            Op::PushData(0),
            Op::Push(0x2a),
            Op::Store8,
            Op::Push(1),
            Op::PushData(0),
            Op::Push(1),
            Op::Syscall(SYS_WRITE, 3),
            Op::Push(0),
            Op::Syscall(SYS_EXIT, 1),
            Op::Halt,
        ];
        let image = ProgramImage::new(code, 0).with_data(vec![vec![0u8]]);
        let mut machine = Machine::new(256, 1000);
        machine.spawn(&image, "poke").unwrap();
        machine.schedule();
        assert_eq!(machine.console.output(), &[0x2a]);
    }

    #[test]
    fn process_isolation() {
        let mut machine = Machine::new(512, 1000);
        machine.spawn(&write_then_exit(b"A"), "a").unwrap();
        machine.spawn(&write_then_exit(b"B"), "b").unwrap();
        machine.run_steps(2);
        let mut output = machine.console.output().to_vec();
        output.sort_unstable();
        assert_eq!(output, b"AB");
        assert_eq!(machine.procs.count(), 2);
        assert_eq!(machine.reap_zombies(), 2);
        assert_eq!(machine.procs.count(), 0);
    }

    #[test]
    fn open_and_write_a_descriptor() {
        // data[0] = "console\0", data[1] = "hi\n".
        let code = vec![
            Op::PushData(0),
            Op::Push(0),
            Op::Syscall(SYS_OPEN, 2),
            Op::PushData(1),
            Op::Push(3),
            Op::Syscall(SYS_WRITE, 3),
            Op::Push(0),
            Op::Syscall(SYS_EXIT, 1),
            Op::Halt,
        ];
        let image =
            ProgramImage::new(code, 0).with_data(vec![b"console\0".to_vec(), b"hi\n".to_vec()]);
        let mut machine = Machine::new(256, 1000);
        machine.spawn(&image, "writer").unwrap();
        machine.schedule();
        assert_eq!(machine.console.output(), b"hi\n");
    }

    #[test]
    fn sinu_program_writes_to_the_console() {
        let src = r#"
            void main() {
                puts("hello, sinu\n");
            }
        "#;
        let image = lang::compile(src).expect("sinu program compiles");
        let mut machine = Machine::new(256, 1000);
        let pid = machine.spawn(&image, "hello").unwrap();
        machine.run_steps(4);
        assert_eq!(machine.console.output(), b"hello, sinu\n");
        assert_eq!(machine.procs.get(pid).unwrap().state, ProcState::Zombie);
        assert_eq!(machine.procs.get(pid).unwrap().exit_code, 0);
    }

    #[test]
    fn sinu_program_echoes_input() {
        let src = r#"
            void main() {
                u8 buf[8] = [];
                sword n = read(0, &buf[0], 8);
                write(1, &buf[0], n);
            }
        "#;
        let image = lang::compile(src).expect("sinu program compiles");
        let mut machine = Machine::new(256, 1000);
        let pid = machine.spawn(&image, "cat").unwrap();
        machine.schedule();
        assert_eq!(machine.procs.get(pid).unwrap().state, ProcState::Sleeping);
        machine.console.push_bytes(b"ping");
        machine.run_steps(4);
        assert_eq!(machine.console.output(), b"ping");
        assert_eq!(machine.procs.get(pid).unwrap().state, ProcState::Zombie);
    }

    #[test]
    fn sinu_program_uses_structs_pointers_and_arrays() {
        let src = r#"
            struct Span { word start; word len; };
            word total(*u8 s, word n) {
                word i = 0;
                word sum = 0;
                while (i < n) {
                    sum = sum + cast<word>(*(s + i));
                    i = i + 1;
                }
                return sum;
            }
            void main() {
                u8 msg[3] = [65, 66, 10];
                Span span = { start = 0, len = 3 };
                span.start = 0;
                word n = total(&msg[0], 3);
                write(1, &msg[0], cast<sword>(span.len));
                if (n == cast<word>(65 + 66 + 10)) { exit(0); }
                exit(1);
            }
        "#;
        let image = lang::compile(src).expect("sinu program compiles");
        let mut machine = Machine::new(256, 1000);
        let pid = machine.spawn(&image, "phase-f").unwrap();
        machine.run_steps(8);
        assert_eq!(machine.console.output(), b"AB\n");
        let proc = machine.procs.get(pid).unwrap();
        assert_eq!(proc.state, ProcState::Zombie);
        assert_eq!(proc.exit_code, 0);
    }

    #[test]
    fn sinu_file_roundtrip_through_the_disk() {
        let src = r#"
            void main() {
                sword fd = open("/data.txt", 258); // O_CREAT|O_RDWR
                if (fd < 0) { puts("open-w fail\n"); exit(1); }
                u8 msg[6];
                msg[0] = 'h'; msg[1] = 'e'; msg[2] = 'l'; msg[3] = 'l'; msg[4] = 'o';
                write(fd, &msg[0], 5);
                close(fd);

                sword fd2 = open("/data.txt", 0); // O_RDONLY
                if (fd2 < 0) { puts("open-r fail\n"); exit(1); }
                u8 buf[8];
                sword n = read(fd2, &buf[0], 5);
                write(1, &buf[0], n);
                close(fd2);
                write(1, "\n", 1);
            }
        "#;
        let image = lang::compile(src).expect("sinu program compiles");
        let mut machine = Machine::new(256, 100);
        let pid = machine.spawn(&image, "filerw").unwrap();
        // Disk latency is DISK_LATENCY ticks per I/O; several round-trips need
        // generous headroom. run until the process exits or we hit a ceiling.
        for _ in 0..100 {
            machine.schedule();
            if machine.procs.get(pid).unwrap().state == ProcState::Zombie {
                break;
            }
        }
        let proc = machine.procs.get(pid).unwrap();
        assert_eq!(proc.state, ProcState::Zombie, "program should finish");
        assert_eq!(proc.exit_code, 0, "program should exit cleanly");
        assert_eq!(machine.console.output(), b"hello\n");
        // The file persists on the simulated disk.
        assert_eq!(machine.read_file("/data.txt").unwrap(), b"hello");
    }

    #[test]
    fn disk_io_blocks_and_another_process_runs() {
        // A reader blocks on disk latency while a writer process completes.
        let reader_src = r#"
            void main() {
                sword fd = open("/slow.txt", 0);
                u8 buf[4];
                read(fd, &buf[0], 4);
                write(1, &buf[0], 4);
                close(fd);
            }
        "#;
        let writer_src = r#"
            void main() {
                puts("W");
            }
        "#;
        let mut machine = Machine::new(256, 50);
        machine.install_file("/slow.txt", b"data").unwrap();
        let reader = machine
            .spawn(&lang::compile(reader_src).unwrap(), "reader")
            .unwrap();
        let writer = machine
            .spawn(&lang::compile(writer_src).unwrap(), "writer")
            .unwrap();

        machine.run_steps(2);
        assert_eq!(
            machine.procs.get(reader).unwrap().state,
            ProcState::Sleeping,
            "reader sleeps on the disk channel"
        );
        assert_eq!(
            machine.procs.get(writer).unwrap().state,
            ProcState::Zombie,
            "writer completed while reader waited"
        );
        assert_eq!(machine.console.output(), b"W");

        machine.run_steps(10);
        assert_eq!(machine.procs.get(reader).unwrap().state, ProcState::Zombie);
        assert_eq!(machine.console.output(), b"Wdata");
    }

    #[test]
    fn spawn_with_args_exposes_argv() {
        let src = r#"
            void main() {
                sword n = argc();
                u8 buf[32];
                for (sword i = 0; i < n; i = i + 1) {
                    sword len = arg(i, &buf[0], 31);
                    write(1, &buf[0], len);
                    write(1, "\n", 1);
                }
        }
        "#;
        let image = lang::compile(src).expect("compiles");
        let mut machine = Machine::new(256, 100);
        let pid = machine
            .spawn_with_args(&image, "args", &[b"args", b"hello", b"world"])
            .unwrap();
        for _ in 0..20 {
            machine.schedule();
            if machine.procs.get(pid).unwrap().state == ProcState::Zombie {
                break;
            }
        }
        assert_eq!(machine.console.output(), b"args\nhello\nworld\n");
    }

    #[test]
    fn fork_and_wait_parent_gets_child_exit_code() {
        let src = r#"
            void main() {
                sword pid = fork();
                if (pid == 0) {
                    write(1, "C", 1);
                    exit(7);
                }
                sword status = 0;
                sword got = wait(&status);
                if (got == pid && status == 7) { puts("ok\n"); }
            }
        "#;
        let image = lang::compile(src).expect("compiles");
        let mut machine = Machine::new(256, 100);
        let pid = machine.spawn(&image, "forkwait").unwrap();
        for _ in 0..30 {
            machine.schedule();
            if machine.procs.get(pid).unwrap().state == ProcState::Zombie {
                break;
            }
        }
        assert_eq!(machine.procs.get(pid).unwrap().state, ProcState::Zombie);
        assert_eq!(machine.console.output(), b"Cok\n");
    }

    #[test]
    fn pipe_single_process_roundtrip() {
        let src = r#"
            void main() {
                sword fds[2];
                pipe(&fds[0]);
                write(fds[1], "yo", 2);
                u8 buf[4];
                sword n = read(fds[0], &buf[0], 2);
                close(fds[0]);
                close(fds[1]);
                write(1, &buf[0], n);
            }
        "#;
        let image = lang::compile(src).expect("compiles");
        let mut machine = Machine::new(256, 100);
        let pid = machine.spawn(&image, "p1").unwrap();
        for _ in 0..20 {
            machine.schedule();
            if machine.procs.get(pid).unwrap().state == ProcState::Zombie {
                break;
            }
        }
        assert_eq!(machine.console.output(), b"yo");
    }

    #[test]
    fn pipe_passes_data_between_processes() {
        let src = r#"
            void main() {
                sword fds[2];
                pipe(&fds[0]);
                sword pid = fork();
                if (pid == 0) {
                    close(fds[0]);
                    write(fds[1], "hi", 2);
                    close(fds[1]);
                    exit(0);
                }
                close(fds[1]);
                u8 buf[4];
                sword n = read(fds[0], &buf[0], 2);
                close(fds[0]);
                sword st = 0;
                wait(&st);
                write(1, &buf[0], n);
            }
        "#;
        let image = lang::compile(src).expect("compiles");
        let mut machine = Machine::new(256, 100);
        let pid = machine.spawn(&image, "pipetest").unwrap();
        for _ in 0..40 {
            machine.schedule();
            if machine.procs.get(pid).unwrap().state == ProcState::Zombie {
                break;
            }
        }
        assert_eq!(machine.procs.get(pid).unwrap().state, ProcState::Zombie);
        assert_eq!(machine.console.output(), b"hi");
    }

    #[test]
    fn dup2_redirects_stdout_to_a_pipe() {
        let src = r#"
            void main() {
                sword fds[2];
                pipe(&fds[0]);
                sword pid = fork();
                if (pid == 0) {
                    close(fds[0]);
                    dup2(fds[1], 1);
                    close(fds[1]);
                    puts("redir");
                    exit(0);
                }
                close(fds[1]);
                u8 buf[8];
                sword n = read(fds[0], &buf[0], 8);
                close(fds[0]);
                sword st = 0;
                wait(&st);
                write(1, &buf[0], n);
                puts("\n");
            }
        "#;
        let image = lang::compile(src).expect("compiles");
        let mut machine = Machine::new(256, 100);
        let pid = machine.spawn(&image, "redir").unwrap();
        for _ in 0..40 {
            machine.schedule();
            if machine.procs.get(pid).unwrap().state == ProcState::Zombie {
                break;
            }
        }
        assert_eq!(machine.console.output(), b"redir\n");
    }

    #[test]
    fn shell_runs_a_pipeline() {
        // Install echo + cat + sh onto the VFS, spawn sh, drive `echo hi | cat`.
        let echo_src = include_str!("../progs/echo.sn");
        let cat_src = include_str!("../progs/cat.sn");
        let sh_src = include_str!("../progs/sh.sn");

        let mut machine = Machine::new(512, 200);
        let _ = machine.install_dir("/bin");
        for (name, src) in [("echo", echo_src), ("cat", cat_src), ("sh", sh_src)] {
            let image = lang::compile(src).expect("compiles");
            let bytes = lang::encode_image(&image);
            machine
                .install_file(&format!("/bin/{name}"), &bytes)
                .expect("install");
        }

        let sh_image = lang::compile(sh_src).unwrap();
        let pid = machine.spawn(&sh_image, "sh").unwrap();

        // Feed the command line to sh's stdin (console).
        machine.console.push_bytes(b"echo hi | cat\n");
        for _ in 0..120 {
            machine.schedule();
            let Some(p) = machine.procs.get(pid) else {
                break;
            };
            if p.state == ProcState::Zombie {
                break;
            }
        }
        let out = String::from_utf8_lossy(machine.console.output()).to_string();
        assert!(out.contains("hi"), "pipeline should print hi; got: {out:?}");
    }

    #[test]
    fn shell_cd_then_ls_relative() {
        let ls_src = include_str!("../progs/ls.sn");
        let sh_src = include_str!("../progs/sh.sn");

        let mut machine = Machine::new(512, 200);
        let _ = machine.install_dir("/bin");
        let _ = machine.install_dir("/home");
        let _ = machine.install_dir("/home/sub");
        machine.install_file("/home/note", b"x").unwrap();
        for (name, src) in [("ls", ls_src), ("sh", sh_src)] {
            let image = lang::compile(src).expect("compiles");
            let bytes = lang::encode_image(&image);
            machine
                .install_file(&format!("/bin/{name}"), &bytes)
                .expect("install");
        }
        let sh_image = lang::compile(sh_src).unwrap();
        let pid = machine.spawn(&sh_image, "sh").unwrap();
        machine.console.push_bytes(b"cd /home\nls\n");
        for _ in 0..250 {
            machine.schedule();
            let Some(p) = machine.procs.get(pid) else {
                break;
            };
            if p.state == ProcState::Zombie {
                break;
            }
        }
        let out = String::from_utf8_lossy(machine.console.output()).to_string();
        assert!(out.contains("note"), "cd+ls shows the file; got: {out:?}");
        assert!(out.contains("sub/"), "cd+ls shows the dir; got: {out:?}");
    }

    #[test]
    fn shell_ls_shows_files_and_dirs() {
        let ls_src = include_str!("../progs/ls.sn");
        let sh_src = include_str!("../progs/sh.sn");

        let mut machine = Machine::new(512, 200);
        let _ = machine.install_dir("/bin");
        let _ = machine.install_dir("/home");
        let _ = machine.install_dir("/home/sub");
        machine.install_file("/home/note", b"x").unwrap();
        for (name, src) in [("ls", ls_src), ("sh", sh_src)] {
            let image = lang::compile(src).expect("compiles");
            let bytes = lang::encode_image(&image);
            machine
                .install_file(&format!("/bin/{name}"), &bytes)
                .expect("install");
        }
        let sh_image = lang::compile(sh_src).unwrap();
        let pid = machine.spawn(&sh_image, "sh").unwrap();
        machine.console.push_bytes(b"ls /home\n");
        for _ in 0..200 {
            machine.schedule();
            let Some(p) = machine.procs.get(pid) else {
                break;
            };
            if p.state == ProcState::Zombie {
                break;
            }
        }
        let out = String::from_utf8_lossy(machine.console.output()).to_string();
        assert!(out.contains("note"), "ls shows the file; got: {out:?}");
        assert!(
            out.contains("sub/"),
            "ls shows the dir with slash; got: {out:?}"
        );
    }

    #[test]
    fn chdir_makes_relative_opens_resolve_against_cwd() {
        let src = r#"
            void main() {
                u8 dir[6];
                dir[0] = '/';
                dir[1] = 'h';
                dir[2] = 'o';
                dir[3] = 'm';
                dir[4] = 'e';
                dir[5] = 0;
                if (chdir(&dir[0]) < 0) { puts("chdir fail\n"); exit(1); }
                sword fd = open("readme", 0); // relative -> /home/readme
                if (fd < 0) { puts("open fail\n"); exit(1); }
                u8 buf[32];
                sword n = read(fd, &buf[0], 11);
                close(fd);
                write(1, &buf[0], n);
            }
        "#;
        let image = lang::compile(src).expect("compiles");
        let mut machine = Machine::new(256, 100);
        let _ = machine.install_dir("/home");
        machine
            .install_file("/home/readme", b"Welcome to SIGNULL.")
            .unwrap();
        let pid = machine.spawn(&image, "cdtest").unwrap();
        for _ in 0..40 {
            machine.schedule();
            if machine.procs.get(pid).unwrap().state == ProcState::Zombie {
                break;
            }
        }
        assert_eq!(machine.procs.get(pid).unwrap().state, ProcState::Zombie);
        assert_eq!(machine.console.output(), b"Welcome to ");
        // chdir also records the display path.
        assert_eq!(
            machine.procs.get(pid).map(|p| p.cwd_path.clone()),
            Some("/home".to_string())
        );
    }

    #[test]
    fn ls_marks_directories_with_trailing_slash() {
        let ls_src = include_str!("../progs/ls.sn");
        let mut machine = Machine::new(256, 100);
        let _ = machine.install_dir("/home");
        let _ = machine.install_dir("/home/sub");
        machine.install_file("/home/note", b"x").unwrap();
        // Run `ls /home` via the compiled image with argv.
        let ls_image = lang::compile(ls_src).unwrap();
        let pid = machine
            .spawn_with_args(&ls_image, "ls", &[b"ls", b"/home"])
            .unwrap();
        for _ in 0..40 {
            machine.schedule();
            if machine.procs.get(pid).unwrap().state == ProcState::Zombie {
                break;
            }
        }
        let out = String::from_utf8_lossy(machine.console.output()).to_string();
        assert!(out.contains("note"), "lists note; got: {out:?}");
        assert!(
            !out.contains("note/"),
            "note is a file, no slash; got: {out:?}"
        );
        assert!(
            out.contains("sub/"),
            "sub is a dir, gets a slash; got: {out:?}"
        );
        assert!(!out.contains("./"), "skips .; got: {out:?}");
    }

    /// Compile a sinu program, spawn it, and run a few quanta.
    fn run_sinu(src: &str, quantum: u64) -> (Machine, usize) {
        let image = lang::compile(src).expect("sinu program compiles");
        let mut machine = Machine::new(256, quantum);
        let pid = machine.spawn(&image, "prog").unwrap();
        machine.run_steps(2);
        (machine, pid)
    }

    #[test]
    fn runaway_recursion_faults_at_the_call_depth_cap() {
        let (machine, pid) = run_sinu("void f() { f(); } void main() { f(); }", 1000);
        let proc = machine.procs.get(pid).unwrap();
        assert_eq!(proc.state, ProcState::Zombie);
        assert_eq!(proc.exit_code, -1);
        assert_eq!(
            machine.console.output(),
            format!("pid {pid} faulted: call depth limit reached\n").as_bytes()
        );
    }

    #[test]
    fn big_frames_fault_at_the_frame_cap() {
        let (machine, pid) = run_sinu(
            "void f() { u8 a[4096] = []; f(); } void main() { f(); }",
            1000,
        );
        let proc = machine.procs.get(pid).unwrap();
        assert_eq!(proc.state, ProcState::Zombie);
        assert_eq!(proc.exit_code, -1);
        assert_eq!(
            machine.console.output(),
            format!("pid {pid} faulted: out of stack space\n").as_bytes()
        );
    }

    #[test]
    fn divide_by_zero_faults() {
        let (machine, pid) = run_sinu("int main() { int z = 0; return 10 / z; }", 1000);
        let proc = machine.procs.get(pid).unwrap();
        assert_eq!(proc.state, ProcState::Zombie);
        assert_eq!(proc.exit_code, -1);
        assert_eq!(
            machine.console.output(),
            format!("pid {pid} faulted: division by zero\n").as_bytes()
        );
    }

    #[test]
    fn bounds_violation_traps_with_the_trap_exit_code() {
        let (machine, pid) = run_sinu(
            "int main() { int a[2] = [1, 2]; int i = 5; return a[i]; }",
            1000,
        );
        let proc = machine.procs.get(pid).unwrap();
        assert_eq!(proc.state, ProcState::Zombie);
        assert_eq!(proc.exit_code, bytecode::abi::TRAP_EXIT_CODE);
        assert!(machine.console.output().is_empty());
    }

    #[test]
    fn runaway_loop_is_preempted_not_killed() {
        let spinner = lang::compile("void main() { while (true) {} }").unwrap();
        let good = lang::compile(r#"void main() { puts("ok\n"); }"#).unwrap();

        let mut machine = Machine::new(256, 64);
        machine.set_scheduler(SchedPolicy::RoundRobin);
        let bad = machine.spawn(&spinner, "spin").unwrap();
        let ok = machine.spawn(&good, "good").unwrap();

        machine.run_steps(6);

        assert_eq!(machine.console.output(), b"ok\n");
        assert_eq!(machine.procs.get(ok).unwrap().state, ProcState::Zombie);
        assert_eq!(
            machine.procs.get(bad).unwrap().state,
            ProcState::Runnable,
            "the runaway process is preempted, never killed"
        );
    }

    #[test]
    fn swap_sched() {
        let mut machine = Machine::new(256, 1000);
        assert_eq!(machine.scheduler_name(), "fifo");
        machine.set_scheduler(SchedPolicy::Mlfq);
        assert_eq!(machine.scheduler_name(), "mlfq");
        assert_eq!(machine.config.scheduler, SchedPolicy::Mlfq);
    }

    #[test]
    fn non_compat_hardware_config() {
        let hw = Hardware {
            cores: 4,
            ..Hardware::single_core(256)
        };
        let mut config = KernelConfig::new(1000);
        config.workers = 4;
        let machine = Machine::with_hardware(hw, config);
        assert_eq!(
            machine.workers.count(),
            1,
            "silicon alone must not multicore"
        );
    }

    #[test]
    fn multicore_runs_two_processes_in_one_tick() {
        let hw = Hardware {
            cores: 4,
            ..Hardware::single_core(512)
        };
        let mut config = KernelConfig::new(1000);
        config.features = FeatureFlags {
            multicore: true,
            ..Default::default()
        };
        config.workers = 2;
        let mut machine = Machine::with_hardware(hw, config);
        assert_eq!(machine.workers.count(), 2);
        machine.spawn(&write_then_exit(b"A"), "a").unwrap();
        machine.spawn(&write_then_exit(b"B"), "b").unwrap();
        machine.run_steps(1);
        let mut output = machine.console.output().to_vec();
        output.sort_unstable();
        assert_eq!(output, b"AB", "both workers ran within one tick");
    }

    #[test]
    fn getcwd_reports_the_display_path_after_chdir() {
        let src = r#"
            void main() {
                u8 dir[6];
                dir[0] = '/'; dir[1] = 'h'; dir[2] = 'o'; dir[3] = 'm'; dir[4] = 'e'; dir[5] = 0;
                chdir(&dir[0]);
                u8 buf[64];
                sword n = syscall(23, &buf[0], 63); // getcwd
                write(1, &buf[0], n);
            }
        "#;
        let image = lang::compile(src).expect("compiles");
        let mut machine = Machine::new(256, 2000);
        let _ = machine.install_dir("/home");
        let pid = machine.spawn(&image, "pwdtest").unwrap();
        machine.run_steps(2);
        assert_eq!(machine.console.output(), b"/home");
        assert_eq!(machine.procs.get(pid).unwrap().state, ProcState::Zombie);
    }

    #[test]
    fn uname_reports_sysname_and_hostname() {
        let src = r#"
            void main() {
                u8 buf[320];
                syscall(27, &buf[0]); // uname
                write(1, &buf[0], 7);      // "SIGNULL"
                write(1, &buf[64], 4);     // hostname (sig-r04 -> "sig-")
            }
        "#;
        let image = lang::compile(src).expect("compiles");
        let mut machine = Machine::new(256, 2000);
        machine.hostname = "sig-r04".to_owned();
        let pid = machine.spawn(&image, "unametest").unwrap();
        machine.run_steps(2);
        assert_eq!(machine.console.output(), b"SIGNULLsig-");
        assert_eq!(machine.procs.get(pid).unwrap().state, ProcState::Zombie);
    }

    #[test]
    fn date_is_at_or_after_the_1990_epoch() {
        let src = r#"
            void main() {
                u8 buf[24];
                syscall(32, &buf[0]); // date
                // Write the 4-byte LE year to the console.
                write(1, &buf[0], 4);
            }
        "#;
        let image = lang::compile(src).expect("compiles");
        let mut machine = Machine::new(256, 2000);
        let pid = machine.spawn(&image, "datetest").unwrap();
        machine.run_steps(2);
        let out = machine.console.output();
        assert_eq!(out.len(), 4);
        let year = u32::from_le_bytes([out[0], out[1], out[2], out[3]]);
        assert!(year >= 1990, "year should be >= 1990, got {year}");
        assert_eq!(machine.procs.get(pid).unwrap().state, ProcState::Zombie);
    }

    #[test]
    fn statfs_reports_nonzero_totals() {
        let src = r#"
            void main() {
                u8 buf[20];
                syscall(28, &buf[0]); // statfs
                write(1, &buf[0], 20);
            }
        "#;
        let image = lang::compile(src).expect("compiles");
        let mut machine = Machine::new(256, 2000);
        let pid = machine.spawn(&image, "dfstest").unwrap();
        machine.run_steps(2);
        let out = machine.console.output();
        assert_eq!(out.len(), 20);
        let total_b = u32::from_le_bytes([out[0], out[1], out[2], out[3]]);
        let total_i = u32::from_le_bytes([out[8], out[9], out[10], out[11]]);
        assert!(total_b > 0, "total_blocks should be > 0");
        assert!(total_i > 0, "total_inodes should be > 0");
        assert_eq!(machine.procs.get(pid).unwrap().state, ProcState::Zombie);
    }

    #[test]
    fn ps_lists_the_running_process() {
        let src = r#"
            void main() {
                u8 buf[256];
                sword n = syscall(29, &buf[0], 256); // ps
                // Write the first 4 bytes (pid) of the first record.
                write(1, &buf[0], 4);
            }
        "#;
        let image = lang::compile(src).expect("compiles");
        let mut machine = Machine::new(256, 2000);
        let pid = machine.spawn(&image, "pstest").unwrap();
        machine.run_steps(2);
        let out = machine.console.output();
        assert_eq!(out.len(), 4);
        let reported = i32::from_le_bytes([out[0], out[1], out[2], out[3]]) as usize;
        assert_eq!(reported, pid, "ps should report the process's own pid");
    }

    #[test]
    fn kill_terminates_a_target_process() {
        // A child that loops forever; the parent kills it.
        let child = r#"
            void main() { while (true) { yield(); } }
        "#;
        let mut machine = Machine::new(256, 2000);
        let child_image = lang::compile(child).expect("child compiles");
        let child_pid = machine.spawn(&child_image, "spinner").unwrap();
        // Let it run so it is in the process table.
        machine.run_steps(1);
        assert!(machine.procs.get(child_pid).unwrap().is_alive());

        // Drive kill directly through the machine's kill path.
        machine.kill_proc(child_pid);
        assert_eq!(
            machine.procs.get(child_pid).unwrap().state,
            ProcState::Zombie
        );
    }

    #[test]
    fn nblocks_counts_a_file_blocks() {
        let src = r#"
            void main() {
                u8 p[8];
                p[0] = '/'; p[1] = 'f'; p[2] = ' '; p[3] = 0; // unused path stub
                // We install /data below; build its path.
                u8 path[6];
                path[0] = '/'; path[1] = 'd'; path[2] = 'a'; path[3] = 't'; path[4] = 'a'; path[5] = 0;
                sword b = syscall(33, &path[0]); // nblocks
                // Write the low byte of the block count.
                u8 out[1];
                out[0] = cast<u8>(b);
                write(1, &out[0], 1);
            }
        "#;
        let image = lang::compile(src).expect("compiles");
        let mut machine = Machine::new(256, 2000);
        // A small file (10 bytes) occupies 1 data block.
        machine.install_file("/data", &[0u8; 10]).unwrap();
        let pid = machine.spawn(&image, "blockstest").unwrap();
        machine.run_steps(2);
        let out = machine.console.output();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0], 1, "a small file occupies 1 block; got {}", out[0]);
        assert_eq!(machine.procs.get(pid).unwrap().state, ProcState::Zombie);
    }

    #[test]
    fn shell_survives_spawn_chdir_and_creds() {
        let sh_src = include_str!("../progs/sh.sn");
        let mut machine = Machine::new(1024, 20000);
        let _ = machine.install_dir("/bin");
        let _ = machine.install_dir("/home");
        let _ = machine.install_dir("/home/alice");
        let sh_image = lang::compile(sh_src).expect("sh compiles");
        machine
            .install_file("/bin/sh", &lang::encode_image(&sh_image))
            .unwrap();

        let bytes = machine.read_file("/bin/sh").unwrap();
        let image = lang::decode_image(&bytes).unwrap();
        let pid = machine.spawn(&image, "sh").unwrap();

        // Set the home cwd + creds, exactly like the game does.
        let inum = {
            let vfs = Vfs::mount(&mut machine.disk).unwrap();
            vfs.namei("/home/alice").unwrap().unwrap()
        };
        {
            let proc = machine.procs.get_mut(pid).unwrap();
            proc.cwd = inum;
            proc.cwd_path = "/home/alice".to_string();
            proc.creds = Credentials {
                uid: 1000,
                gid: 1000,
            };
        }

        // Run a few ticks: the shell should block on console read, not die.
        machine.run_steps(3);
        let state = machine.procs.get(pid).unwrap().state;
        assert_ne!(
            state,
            ProcState::Zombie,
            "shell must survive spawn+chdir+creds; console: {:?}",
            String::from_utf8_lossy(machine.console.output())
        );
    }

    #[test]
    fn full_userland_install_leaves_room_to_create_users() {
        let mut machine = Machine::new(1024, 20000);
        let _ = machine.install_dir("/bin");
        let _ = machine.install_dir("/home");
        let _ = machine.install_dir("/tmp");
        let _ = machine.install_dir("/etc");
        let _ = machine.install_dir("/root");

        // Install every program the game ships.
        let progs_dir = format!("{}/progs", env!("CARGO_MANIFEST_DIR"));
        for name in [
            "sh", "echo", "cat", "ls", "rm", "mkdir", "nano", "pwd", "cp", "mv", "clear", "grep",
            "find", "head", "tail", "whoami", "id", "ps", "kill", "df", "du", "dmesg", "uptime",
            "date", "uname", "touch",
        ] {
            let path = format!("{progs_dir}/{name}.sn");
            let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
            let image = lang::compile(&src).unwrap_or_else(|e| panic!("compile {name}: {e}"));
            machine
                .install_file(&format!("/bin/{name}"), &lang::encode_image(&image))
                .unwrap_or_else(|e| panic!("install {name}: {e}"));
        }

        // Seed the root account + /etc/passwd.
        machine.install_file("/root/.keep", b"").unwrap();
        machine
            .install_file("/etc/passwd", b"root:x:0:0::/root:/bin/sh\n")
            .unwrap();

        // Now create a brand-new user account, like finish_new_user does.
        let home = "/home/alice";
        machine
            .install_dir(home)
            .unwrap_or_else(|e| panic!("mkdir {home} after full install: {e}"));
        let mut pw = machine.read_file("/etc/passwd").unwrap();
        pw.extend_from_slice(b"alice:h:1000:1000::/home/alice:/bin/sh\n");
        machine
            .install_file("/etc/passwd", &pw)
            .unwrap_or_else(|e| panic!("write /etc/passwd after full install: {e}"));

        // The new user's home must be resolvable.
        let inum = {
            let vfs = Vfs::mount(&mut machine.disk).unwrap();
            vfs.namei(home).unwrap().unwrap()
        };
        assert!(inum > 0);
    }
}
