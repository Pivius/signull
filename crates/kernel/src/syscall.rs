use bytecode::SyscallResult;

use crate::{fs::Fd, sync::Chan};

pub use bytecode::abi::{
    O_ACCMODE, O_CREAT, O_RDONLY, O_RDWR, O_TRUNC, O_WRONLY, SEEK_CUR, SEEK_END, SEEK_SET, SYS_ARG,
    SYS_ARGC, SYS_CHDIR, SYS_CLOSE, SYS_DATE, SYS_DMESG, SYS_DUP, SYS_DUP2, SYS_EXEC, SYS_EXIT,
    SYS_FORK, SYS_GETCWD, SYS_GETGID, SYS_GETPID, SYS_GETUID, SYS_KILL, SYS_LSEEK, SYS_MKDIR,
    SYS_NBLOCKS, SYS_OPEN, SYS_PIPE, SYS_PS, SYS_READ, SYS_SBRK, SYS_SLEEP, SYS_STAT, SYS_STATFS,
    SYS_UNAME, SYS_UNLINK, SYS_UPTIME, SYS_WAIT, SYS_WRITE, SYS_YIELD,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyscallError {
    Fault,
    BadFd,
    NotFound,
    NoSpace,
    Inval,
}

/// Result of a read attempt: either bytes, or a sleep channel when nothing
/// is available yet (the caller blocks and the syscall is retried on wake).
#[derive(Debug)]
pub enum ReadOutcome {
    Data(Vec<u8>),
    Blocked(Chan),
}

/// Result of a write attempt: bytes written, or a sleep channel when the
/// backing device is not ready yet.
#[derive(Debug)]
pub enum WriteOutcome {
    Written(usize),
    Blocked(Chan),
    /// The pipe has no readers or another write error.
    Error,
}

/// Result of a wait attempt: a reaped child pid, or a sleep channel.
#[derive(Debug)]
pub enum WaitOutcome {
    Done(i64),
    Blocked(Chan),
}

pub trait SyscallEnv {
    fn pid(&self) -> usize;
    fn copyin(&self, va: usize, len: usize) -> Result<Vec<u8>, SyscallError>;
    fn copyout(&mut self, va: usize, data: &[u8]) -> Result<(), SyscallError>;
    fn read(&mut self, fd: Fd, max: usize) -> Result<ReadOutcome, SyscallError>;
    fn write(&mut self, fd: Fd, data: &[u8]) -> Result<WriteOutcome, SyscallError>;
    fn open(&mut self, path_va: usize, flags: i64) -> Result<Fd, SyscallError>;
    fn close(&mut self, fd: Fd) -> Result<(), SyscallError>;
    fn dup(&mut self, fd: Fd) -> Result<Fd, SyscallError>;
    fn dup2(&mut self, oldfd: Fd, newfd: Fd) -> Result<Fd, SyscallError>;
    fn fork(&mut self) -> SyscallResult;
    fn pipe(&mut self, fds_va: usize) -> Result<(), SyscallError>;
    fn wait(&mut self, status_va: usize) -> Result<WaitOutcome, SyscallError>;
    fn exec(&mut self, path_va: usize, argc: i64, argv_va: usize) -> SyscallResult;
    fn chdir(&mut self, path_va: usize) -> Result<(), SyscallError>;
    fn lseek(&mut self, fd: Fd, offset: i64, whence: i64) -> Result<i64, SyscallError>;
    fn unlink(&mut self, path_va: usize) -> Result<(), SyscallError>;
    fn mkdir(&mut self, path_va: usize) -> Result<(), SyscallError>;
    fn stat(&mut self, path_va: usize, out_va: usize) -> Result<(), SyscallError>;
    fn getcwd(&mut self, buf_va: usize, max: usize) -> Result<i64, SyscallError>;
    fn getuid(&self) -> i64;
    fn getgid(&self) -> i64;
    fn uptime(&self) -> i64;
    fn uname(&mut self, buf_va: usize) -> Result<(), SyscallError>;
    fn statfs(&mut self, buf_va: usize) -> Result<(), SyscallError>;
    fn ps(&mut self, buf_va: usize, max: usize) -> Result<i64, SyscallError>;
    fn kill(&mut self, pid: i64) -> Result<(), SyscallError>;
    fn dmesg(&mut self, buf_va: usize, max: usize) -> Result<i64, SyscallError>;
    fn date(&mut self, buf_va: usize) -> Result<(), SyscallError>;
    fn nblocks(&mut self, path_va: usize) -> Result<i64, SyscallError>;
    fn argc(&self) -> i64;
    fn arg(&mut self, index: i64, buf_va: usize, max: usize) -> Result<i64, SyscallError>;
    fn sbrk(&mut self, delta: isize) -> Result<usize, SyscallError>;
    fn exit(&mut self, code: i64);
    fn block(&mut self, chan: Chan);
    fn sleep(&mut self, ticks: u64);
    fn yield_now(&mut self);
}

fn arg(args: &[i64], index: usize) -> i64 {
    args.get(index).copied().unwrap_or(0)
}

pub fn dispatch(env: &mut dyn SyscallEnv, number: u16, args: &[i64]) -> SyscallResult {
    match number {
        SYS_EXIT => {
            let code = arg(args, 0);
            env.exit(code);
            SyscallResult::exiting(code)
        }
        SYS_WRITE => {
            let fd = arg(args, 0) as Fd;
            let count = arg(args, 2);
            if count < 0 {
                return SyscallResult::continuing(-1);
            }

            let Ok(data) = env.copyin(arg(args, 1) as usize, count as usize) else {
                return SyscallResult::continuing(-1);
            };
            match env.write(fd, &data) {
                Ok(WriteOutcome::Written(n)) => SyscallResult::continuing(n as i64),
                Ok(WriteOutcome::Blocked(chan)) => {
                    env.block(chan);
                    SyscallResult::retry_block()
                }
                Ok(WriteOutcome::Error) | Err(_) => SyscallResult::continuing(-1),
            }
        }
        SYS_READ => {
            let fd = arg(args, 0) as Fd;
            let count = arg(args, 2);
            if count < 0 {
                return SyscallResult::continuing(-1);
            }

            match env.read(fd, count as usize) {
                Err(_) => SyscallResult::continuing(-1),
                Ok(ReadOutcome::Blocked(chan)) => {
                    env.block(chan);
                    SyscallResult::retry_block()
                }
                Ok(ReadOutcome::Data(data)) => match env.copyout(arg(args, 1) as usize, &data) {
                    Ok(()) => SyscallResult::continuing(data.len() as i64),
                    Err(_) => SyscallResult::continuing(-1),
                },
            }
        }
        SYS_OPEN => match env.open(arg(args, 0) as usize, arg(args, 1)) {
            Ok(fd) => SyscallResult::continuing(fd as i64),
            Err(_) => SyscallResult::continuing(-1),
        },
        SYS_CLOSE => match env.close(arg(args, 0) as Fd) {
            Ok(()) => SyscallResult::continuing(0),
            Err(_) => SyscallResult::continuing(-1),
        },
        SYS_DUP => match env.dup(arg(args, 0) as Fd) {
            Ok(fd) => SyscallResult::continuing(fd as i64),
            Err(_) => SyscallResult::continuing(-1),
        },
        SYS_DUP2 => match env.dup2(arg(args, 0) as Fd, arg(args, 1) as Fd) {
            Ok(fd) => SyscallResult::continuing(fd as i64),
            Err(_) => SyscallResult::continuing(-1),
        },
        SYS_FORK => env.fork(),
        SYS_PIPE => match env.pipe(arg(args, 0) as usize) {
            Ok(()) => SyscallResult::continuing(0),
            Err(_) => SyscallResult::continuing(-1),
        },
        SYS_WAIT => {
            let status = arg(args, 0) as usize;
            match env.wait(status) {
                Ok(WaitOutcome::Done(pid)) => SyscallResult::continuing(pid),
                Ok(WaitOutcome::Blocked(chan)) => {
                    env.block(chan);
                    SyscallResult::retry_block()
                }
                Err(_) => SyscallResult::continuing(-1),
            }
        }
        SYS_EXEC => env.exec(arg(args, 0) as usize, arg(args, 1), arg(args, 2) as usize),
        SYS_CHDIR => match env.chdir(arg(args, 0) as usize) {
            Ok(()) => SyscallResult::continuing(0),
            Err(_) => SyscallResult::continuing(-1),
        },
        SYS_LSEEK => match env.lseek(arg(args, 0) as Fd, arg(args, 1), arg(args, 2)) {
            Ok(off) => SyscallResult::continuing(off),
            Err(_) => SyscallResult::continuing(-1),
        },
        SYS_UNLINK => match env.unlink(arg(args, 0) as usize) {
            Ok(()) => SyscallResult::continuing(0),
            Err(_) => SyscallResult::continuing(-1),
        },
        SYS_MKDIR => match env.mkdir(arg(args, 0) as usize) {
            Ok(()) => SyscallResult::continuing(0),
            Err(_) => SyscallResult::continuing(-1),
        },
        SYS_STAT => match env.stat(arg(args, 0) as usize, arg(args, 1) as usize) {
            Ok(()) => SyscallResult::continuing(0),
            Err(_) => SyscallResult::continuing(-1),
        },
        SYS_GETCWD => {
            let buf = arg(args, 0) as usize;
            let max = arg(args, 1);
            match env.getcwd(buf, max.max(0) as usize) {
                Ok(len) => SyscallResult::continuing(len),
                Err(_) => SyscallResult::continuing(-1),
            }
        }
        SYS_GETUID => SyscallResult::continuing(env.getuid()),
        SYS_GETGID => SyscallResult::continuing(env.getgid()),
        SYS_UPTIME => SyscallResult::continuing(env.uptime()),
        SYS_UNAME => match env.uname(arg(args, 0) as usize) {
            Ok(()) => SyscallResult::continuing(0),
            Err(_) => SyscallResult::continuing(-1),
        },
        SYS_STATFS => match env.statfs(arg(args, 0) as usize) {
            Ok(()) => SyscallResult::continuing(0),
            Err(_) => SyscallResult::continuing(-1),
        },
        SYS_PS => {
            let buf = arg(args, 0) as usize;
            let max = arg(args, 1);
            match env.ps(buf, max.max(0) as usize) {
                Ok(count) => SyscallResult::continuing(count),
                Err(_) => SyscallResult::continuing(-1),
            }
        }
        SYS_KILL => match env.kill(arg(args, 0)) {
            Ok(()) => SyscallResult::continuing(0),
            Err(_) => SyscallResult::continuing(-1),
        },
        SYS_DMESG => {
            let buf = arg(args, 0) as usize;
            let max = arg(args, 1);
            match env.dmesg(buf, max.max(0) as usize) {
                Ok(len) => SyscallResult::continuing(len),
                Err(_) => SyscallResult::continuing(-1),
            }
        }
        SYS_DATE => match env.date(arg(args, 0) as usize) {
            Ok(()) => SyscallResult::continuing(0),
            Err(_) => SyscallResult::continuing(-1),
        },
        SYS_NBLOCKS => match env.nblocks(arg(args, 0) as usize) {
            Ok(blocks) => SyscallResult::continuing(blocks),
            Err(_) => SyscallResult::continuing(-1),
        },
        SYS_ARGC => SyscallResult::continuing(env.argc()),
        SYS_ARG => {
            let index = arg(args, 0);
            let buf = arg(args, 1) as usize;
            let max = arg(args, 2);
            match env.arg(index, buf, max.max(0) as usize) {
                Ok(len) => SyscallResult::continuing(len),
                Err(_) => SyscallResult::continuing(-1),
            }
        }
        SYS_SBRK => match env.sbrk(arg(args, 0) as isize) {
            Ok(old) => SyscallResult::continuing(old as i64),
            Err(_) => SyscallResult::continuing(-1),
        },
        SYS_GETPID => SyscallResult::continuing(env.pid() as i64),
        SYS_YIELD => {
            env.yield_now();
            SyscallResult::continuing(0)
        }
        SYS_SLEEP => {
            env.sleep(arg(args, 0) as u64);
            SyscallResult::sleep_block()
        }
        _ => SyscallResult::continuing(-1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::{STDERR, STDIN, STDOUT};

    #[derive(Default)]
    struct FakeEnv {
        pid: usize,
        memory: Vec<u8>,
        written: Vec<u8>,
        input: Vec<u8>,
        blocked: Option<Chan>,
        slept: Option<u64>,
        exited: Option<i64>,
        yielded: bool,
        brk: usize,
        next_fd: Fd,
        seek: i64,
        argc: i64,
        argv: Vec<Vec<u8>>,
    }

    impl FakeEnv {
        fn cstr(&self, va: usize) -> Vec<u8> {
            self.memory
                .get(va..)
                .unwrap_or_default()
                .iter()
                .take_while(|&&b| b != 0)
                .copied()
                .collect()
        }
    }

    impl SyscallEnv for FakeEnv {
        fn pid(&self) -> usize {
            self.pid
        }
        fn copyin(&self, va: usize, len: usize) -> Result<Vec<u8>, SyscallError> {
            self.memory
                .get(va..va + len)
                .map(|s| s.to_vec())
                .ok_or(SyscallError::Fault)
        }
        fn copyout(&mut self, va: usize, data: &[u8]) -> Result<(), SyscallError> {
            let slice = self
                .memory
                .get_mut(va..va + data.len())
                .ok_or(SyscallError::Fault)?;
            slice.copy_from_slice(data);
            Ok(())
        }
        fn read(&mut self, fd: Fd, _max: usize) -> Result<ReadOutcome, SyscallError> {
            if fd != STDIN {
                return Err(SyscallError::BadFd);
            }
            if self.input.is_empty() {
                return Ok(ReadOutcome::Blocked(Chan::CONSOLE));
            }
            let data = std::mem::take(&mut self.input);
            Ok(ReadOutcome::Data(data))
        }
        fn write(&mut self, fd: Fd, data: &[u8]) -> Result<WriteOutcome, SyscallError> {
            if fd != STDOUT && fd != STDERR {
                return Err(SyscallError::BadFd);
            }
            self.written.extend_from_slice(data);
            Ok(WriteOutcome::Written(data.len()))
        }
        fn open(&mut self, path_va: usize, _flags: i64) -> Result<Fd, SyscallError> {
            match self.cstr(path_va).as_slice() {
                b"console" | b"tty" => {
                    self.next_fd += 1;
                    Ok(2 + self.next_fd)
                }
                _ => Err(SyscallError::NotFound),
            }
        }
        fn close(&mut self, fd: Fd) -> Result<(), SyscallError> {
            if fd < 0 {
                Err(SyscallError::BadFd)
            } else {
                Ok(())
            }
        }
        fn dup(&mut self, fd: Fd) -> Result<Fd, SyscallError> {
            if fd == STDOUT {
                self.next_fd += 1;
                Ok(2 + self.next_fd)
            } else {
                Err(SyscallError::BadFd)
            }
        }
        fn dup2(&mut self, oldfd: Fd, newfd: Fd) -> Result<Fd, SyscallError> {
            if oldfd < 0 || newfd < 0 {
                Err(SyscallError::Inval)
            } else {
                Ok(newfd)
            }
        }
        fn fork(&mut self) -> SyscallResult {
            SyscallResult::continuing_and_stop(0)
        }
        fn pipe(&mut self, _fds_va: usize) -> Result<(), SyscallError> {
            Ok(())
        }
        fn wait(&mut self, _status_va: usize) -> Result<WaitOutcome, SyscallError> {
            Err(SyscallError::Inval)
        }
        fn exec(&mut self, _p: usize, _c: i64, _a: usize) -> SyscallResult {
            SyscallResult::continuing(-1)
        }
        fn chdir(&mut self, _path_va: usize) -> Result<(), SyscallError> {
            Ok(())
        }
        fn lseek(&mut self, fd: Fd, offset: i64, whence: i64) -> Result<i64, SyscallError> {
            if fd < 0 {
                return Err(SyscallError::BadFd);
            }
            let next = match whence {
                SEEK_SET => offset,
                SEEK_CUR => self.seek + offset,
                SEEK_END => 100 + offset,
                _ => return Err(SyscallError::Inval),
            };
            if next < 0 {
                return Err(SyscallError::Inval);
            }
            self.seek = next;
            Ok(next)
        }
        fn unlink(&mut self, _path_va: usize) -> Result<(), SyscallError> {
            Ok(())
        }
        fn mkdir(&mut self, _path_va: usize) -> Result<(), SyscallError> {
            Ok(())
        }
        fn stat(&mut self, _path_va: usize, out_va: usize) -> Result<(), SyscallError> {
            self.copyout(out_va, &[1, 0, 0, 0, 10, 0, 0, 0])
        }
        fn getcwd(&mut self, buf_va: usize, max: usize) -> Result<i64, SyscallError> {
            let path = b"/home/test";
            let take = path.len().min(max);
            self.copyout(buf_va, &path[..take])?;
            Ok(take as i64)
        }
        fn getuid(&self) -> i64 {
            1000
        }
        fn getgid(&self) -> i64 {
            1000
        }
        fn uptime(&self) -> i64 {
            123
        }
        fn uname(&mut self, buf_va: usize) -> Result<(), SyscallError> {
            let mut buf = vec![0u8; bytecode::abi::UTSNAME_LEN];
            buf[..6].copy_from_slice(b"SIGNULL");
            self.copyout(buf_va, &buf)
        }
        fn statfs(&mut self, buf_va: usize) -> Result<(), SyscallError> {
            let mut buf = vec![0u8; bytecode::abi::STATFS_LEN];
            buf[0..4].copy_from_slice(&100u32.to_le_bytes());
            self.copyout(buf_va, &buf)
        }
        fn ps(&mut self, buf_va: usize, max: usize) -> Result<i64, SyscallError> {
            let ent = vec![0u8; bytecode::abi::PROCENT_LEN];
            let count = (max / bytecode::abi::PROCENT_LEN).min(1);
            if count == 0 {
                return Ok(0);
            }
            self.copyout(buf_va, &ent)?;
            Ok(count as i64)
        }
        fn kill(&mut self, _pid: i64) -> Result<(), SyscallError> {
            Ok(())
        }
        fn dmesg(&mut self, buf_va: usize, max: usize) -> Result<i64, SyscallError> {
            let msg = b"kernel ok";
            let take = msg.len().min(max);
            self.copyout(buf_va, &msg[..take])?;
            Ok(take as i64)
        }
        fn date(&mut self, buf_va: usize) -> Result<(), SyscallError> {
            let mut buf = vec![0u8; bytecode::abi::DATE_LEN];
            buf[0..4].copy_from_slice(&1990u32.to_le_bytes());
            self.copyout(buf_va, &buf)
        }
        fn nblocks(&mut self, _path_va: usize) -> Result<i64, SyscallError> {
            Ok(3)
        }
        fn argc(&self) -> i64 {
            self.argc
        }
        fn arg(&mut self, index: i64, buf_va: usize, max: usize) -> Result<i64, SyscallError> {
            let index = usize::try_from(index).map_err(|_| SyscallError::Inval)?;
            let Some(arg) = self.argv.get(index).cloned() else {
                return Err(SyscallError::Inval);
            };
            let take = arg.len().min(max);
            self.copyout(buf_va, &arg[..take])?;
            Ok(take as i64)
        }
        fn sbrk(&mut self, delta: isize) -> Result<usize, SyscallError> {
            let old = self.brk;
            self.brk = (self.brk as isize + delta) as usize;
            Ok(old)
        }
        fn exit(&mut self, code: i64) {
            self.exited = Some(code);
        }
        fn block(&mut self, chan: Chan) {
            self.blocked = Some(chan);
        }
        fn sleep(&mut self, ticks: u64) {
            self.slept = Some(ticks);
        }
        fn yield_now(&mut self) {
            self.yielded = true;
        }
    }

    #[test]
    fn write_copies_into_console() {
        let mut env = FakeEnv {
            memory: vec![0; 16],
            ..Default::default()
        };
        env.memory[4..8].copy_from_slice(b"hey!");
        let result = dispatch(&mut env, SYS_WRITE, &[i64::from(STDOUT), 4, 4]);
        assert_eq!(result.value, 4);
        assert_eq!(env.written, b"hey!");
    }

    #[test]
    fn write_to_bad_fd_fails() {
        let mut env = FakeEnv {
            memory: vec![0; 16],
            ..Default::default()
        };
        assert_eq!(dispatch(&mut env, SYS_WRITE, &[7, 0, 4]).value, -1);
    }

    #[test]
    fn read_blocks_without_input() {
        let mut env = FakeEnv {
            memory: vec![0; 16],
            ..Default::default()
        };
        let result = dispatch(&mut env, SYS_READ, &[i64::from(STDIN), 0, 4]);
        assert_eq!(result.action, bytecode::SyscallAction::Block);
        assert!(result.retry, "a blocked read is retried on wake");
        assert_eq!(env.blocked, Some(Chan::CONSOLE));
    }

    #[test]
    fn read_copies_available_input() {
        let mut env = FakeEnv {
            memory: vec![0; 16],
            input: b"abcd".to_vec(),
            ..Default::default()
        };
        let result = dispatch(&mut env, SYS_READ, &[i64::from(STDIN), 4, 4]);
        assert_eq!(result.value, 4);
        assert_eq!(&env.memory[4..8], b"abcd");
    }

    #[test]
    fn open_and_dup_and_close() {
        let mut env = FakeEnv {
            memory: b"console\0".to_vec(),
            ..Default::default()
        };
        let opened = dispatch(&mut env, SYS_OPEN, &[0, 0]).value;
        assert_eq!(opened, 3);
        assert_eq!(dispatch(&mut env, SYS_DUP, &[i64::from(STDOUT)]).value, 4);
        assert_eq!(dispatch(&mut env, SYS_CLOSE, &[opened]).value, 0);
        assert_eq!(dispatch(&mut env, SYS_DUP, &[i64::from(STDIN)]).value, -1);
    }

    #[test]
    fn open_unknown_path_fails() {
        let mut env = FakeEnv {
            memory: b"nope\0".to_vec(),
            ..Default::default()
        };
        assert_eq!(dispatch(&mut env, SYS_OPEN, &[0, 0]).value, -1);
    }

    #[test]
    fn exit_terminates() {
        let mut env = FakeEnv::default();
        let result = dispatch(&mut env, SYS_EXIT, &[9]);
        assert_eq!(result.action, bytecode::SyscallAction::Exit(9));
        assert_eq!(env.exited, Some(9));
    }

    #[test]
    fn getpid_and_yield_and_sleep() {
        let mut env = FakeEnv {
            pid: 42,
            ..Default::default()
        };
        assert_eq!(dispatch(&mut env, SYS_GETPID, &[]).value, 42);
        dispatch(&mut env, SYS_YIELD, &[]);
        assert!(env.yielded);
        let result = dispatch(&mut env, SYS_SLEEP, &[5]);
        assert_eq!(env.slept, Some(5));
        assert_eq!(result.action, bytecode::SyscallAction::Block);
    }

    #[test]
    fn lseek_moves_the_cursor() {
        let mut env = FakeEnv::default();
        assert_eq!(dispatch(&mut env, SYS_LSEEK, &[0, 10, SEEK_SET]).value, 10);
        assert_eq!(dispatch(&mut env, SYS_LSEEK, &[0, 5, SEEK_CUR]).value, 15);
        assert_eq!(dispatch(&mut env, SYS_LSEEK, &[0, -3, SEEK_END]).value, 97);
        assert_eq!(dispatch(&mut env, SYS_LSEEK, &[0, 0, 99]).value, -1);
    }

    #[test]
    fn stat_writes_kind_and_size() {
        let mut env = FakeEnv {
            memory: vec![0; 16],
            ..Default::default()
        };
        assert_eq!(dispatch(&mut env, SYS_STAT, &[0, 4]).value, 0);
        assert_eq!(&env.memory[4..12], &[1, 0, 0, 0, 10, 0, 0, 0]);
    }

    #[test]
    fn argc_and_arg_expose_argv() {
        let mut env = FakeEnv {
            memory: vec![0; 16],
            argc: 2,
            argv: vec![b"nano".to_vec(), b"notes.txt".to_vec()],
            ..Default::default()
        };
        assert_eq!(dispatch(&mut env, SYS_ARGC, &[]).value, 2);
        let len = dispatch(&mut env, SYS_ARG, &[1, 4, 16]).value;
        assert_eq!(len, 9);
        assert_eq!(&env.memory[4..13], b"notes.txt");
        assert_eq!(dispatch(&mut env, SYS_ARG, &[5, 4, 16]).value, -1);
    }

    #[test]
    fn unknown_syscall_returns_error() {
        let mut env = FakeEnv::default();
        assert_eq!(dispatch(&mut env, 999, &[]).value, -1);
    }
}
