//! User-visible syscall ABI.

pub const SYS_EXIT: u16 = 1;
pub const SYS_WRITE: u16 = 2;
pub const SYS_READ: u16 = 3;
pub const SYS_SBRK: u16 = 4;
pub const SYS_GETPID: u16 = 5;
pub const SYS_YIELD: u16 = 6;
pub const SYS_SLEEP: u16 = 7;
pub const SYS_OPEN: u16 = 8;
pub const SYS_CLOSE: u16 = 9;
pub const SYS_DUP: u16 = 10;
pub const SYS_LSEEK: u16 = 11;
pub const SYS_UNLINK: u16 = 12;
pub const SYS_MKDIR: u16 = 13;
pub const SYS_STAT: u16 = 14;
pub const SYS_ARGC: u16 = 15;
pub const SYS_ARG: u16 = 16;
pub const SYS_FORK: u16 = 17;
pub const SYS_PIPE: u16 = 18;
pub const SYS_WAIT: u16 = 19;
pub const SYS_EXEC: u16 = 20;
pub const SYS_DUP2: u16 = 21;
pub const SYS_CHDIR: u16 = 22;
pub const SYS_GETCWD: u16 = 23;
pub const SYS_GETUID: u16 = 24;
pub const SYS_GETGID: u16 = 25;
pub const SYS_UPTIME: u16 = 26;
pub const SYS_UNAME: u16 = 27;
pub const SYS_STATFS: u16 = 28;
pub const SYS_PS: u16 = 29;
pub const SYS_KILL: u16 = 30;
pub const SYS_DMESG: u16 = 31;
pub const SYS_DATE: u16 = 32;
pub const SYS_NBLOCKS: u16 = 33;

/// Standard descriptors.
pub const STDIN: i64 = 0;
pub const STDOUT: i64 = 1;
pub const STDERR: i64 = 2;

/// Reserved exit code used by the language runtime for a trap (bounds, panic).
pub const TRAP_EXIT_CODE: i64 = 101;

/// `lseek` whence values.
pub const SEEK_SET: i64 = 0;
pub const SEEK_CUR: i64 = 1;
pub const SEEK_END: i64 = 2;

/// `open` flags.
pub const O_RDONLY: i64 = 0;
pub const O_WRONLY: i64 = 1;
pub const O_RDWR: i64 = 2;
pub const O_ACCMODE: i64 = 3;
pub const O_CREAT: i64 = 0x100;
pub const O_TRUNC: i64 = 0x200;

// Fixed layouts for the buffer-returning syscalls (SYS_UNAME, SYS_STATFS,
// SYS_PS, SYS_DATE). These are the byte layouts the kernel writes into the
// user buffer and that `sinu` programs unpack with `load<T>`.

/// `SYS_UNAME`: five NUL-padded 64-byte fields (sysname, nodename, release,
/// version, machine). Total 320 bytes.
pub const UTS_FIELD_LEN: usize = 64;
pub const UTSNAME_LEN: usize = UTS_FIELD_LEN * 5;

/// `SYS_STATFS`: 5 little-endian u32s (total_blocks, free_blocks,
/// total_inodes, free_inodes, block_size). Total 20 bytes.
pub const STATFS_LEN: usize = 20;

/// `SYS_PS`: one packed `ProcEnt` record: pid(i32), ppid(i32), uid(u32),
/// state(u8), name[16] = 29 bytes, padded to 32.
pub const PROC_NAME_LEN: usize = 16;
pub const PROCENT_LEN: usize = 32;

/// `SYS_DATE`: 6 little-endian u32s (year, month, day, hour, min, sec).
pub const DATE_LEN: usize = 24;

/// Process states reported in a `ProcEnt`.
pub const PS_UNKNOWN: u8 = 0;
pub const PS_RUNNABLE: u8 = 1;
pub const PS_RUNNING: u8 = 2;
pub const PS_SLEEPING: u8 = 3;
pub const PS_ZOMBIE: u8 = 4;
