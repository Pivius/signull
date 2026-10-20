# `kernel`

The SIGNULL kernel is a Unix-like operating system simulation inspired by [xv6](https://github.com/mit-pdos/xv6-riscv). 
It's implemented as a Rust library rather than a standalone operating system. A frontend, such as a game or terminal emulator, creates a Machine and controls its execution through schedule() and run_steps().

The kernel handles process management, virtual memory, a filesystem backed by a simulated disk, pipes, scheduling, and ~30 sys calls. 
User programs are written in sinu, compiled to bytecode, and executed by the bytecode VM.

## Architecture

```
Machine (machine.rs)       Main entry point
├── Proc / ProcTable       Process lifecycle, fork, exec, wait, kill
├── AddressSpace           Page tables, physical frames, sbrk, memory access
├── VFS + FdTable          Filesystem, files, pipes, file descriptors
├── SchedPolicy            FIFO, round-robin, priority, MLFQ
├── SyscallEnv             System call dispatch and blocking
├── Devices                Console, timer, interrupt controller, disk
├── Trap / Context         VM state capture and preemption
├── sync.rs                Sleep/wakeup channels
├── Hardware / KernelConfig Cores, RAM, disk, quantum, feature flags
└── userland.rs            programs installed to /bin
```

### Scheduling

Each call to `Machine::schedule()` advances the simulation by one tick.

1. The timer advances and wakes processes whose sleep deadlines have passed.
2. Completed disk requests are processed. Disk operations take four ticks, so a process waiting on a read or write blocks while other processes continue running.
3. Pipe waiters are woken and console interrupts are processed.
4. Each configured worker selects a process and runs its VM for up to the configured quantum.

After execution, the kernel handles deferred operations such as replacing a process image during exec, completing fork, reaping children through wait, and marking killed processes as zombies. During fork, the child receives a copy of the parent's memory and stack, with the return value set to zero for the child and the child's PID for the parent.

Processes that use their entire quantum are preempted rather than terminated. If a process encounters a VmError, it becomes a zombie with exit code -1, and the fault is written to the console and kernel log.

### Memory map

Each process has its own address space

```
0x0000_0000 ┌──────────────────────────┐
            │  data blobs (DATA_BASE)  │  mapped from the .snx image
0x0040_0000 ├──────────────────────────┤
            │  heap  (sbrk)            │  grows on demand
            │         ↓                │
            │         ↑                │  16 KiB stack (4 pages)
0x2000_0000 ├──────────────────────────┤  USER_TOP = 1 << 29
            │  (unmapped)              │  VA_LIMIT = 1 << 30
```

The VM's call-frame region is mapped to the process stack using vm.set_frame_region(stack_top, stack_base). If the stack or call frames exceed their limits, the process traps instead of corrupting memory.

### Filesystem

`[ superblock | inode bitmap | data bitmap | inode table | data blocks ]`

- 64-byte inodes with magic value 0x53494E31 (SIN1)
- Root inode number 1
- 12 direct blocks and one indirect block containing 256 u16 block numbers, allowing files of roughly 136 KiB
- 16-byte directory entries with DIRSIZ = 14
- Path resolution through namei and directory lookup through dirlookup
- Link counts through nlink and inode types T_FREE, T_FILE, and T_DIR

The default hardware configuration uses a disk with 2,048 blocks of 512 bytes each.

### Schedulers

Swappable via `KernelConfig { scheduler, … }`:

| Policy | Behavior |
|---|---|
| `Fifo` | first-in-first-out |
| `RoundRobin` | Rotates using quantum preemption |
| `Priority` | static priority ordering |
| `Mlfq` | 3 feedback levels; demote on consuming a full quantum |

### Syscalls

The kernel implements all system calls defined in `bytecode::abi`:

System calls that need to wait for an event return `SyscallResult::Block(Chan)`. 
For example, reading from an empty pipe blocks until data becomes available, while disk operations block until their simulated latency expires.

Multicore execution is also simulated. When `FeatureFlags.multicore` is enabled, multiple processes can run during the same tick, up to the number of configured hardware cores.

## Booting a machine

There is no boot sequence to perform. 
The frontend sets up the filesystem, installs programs, creates a process, and advances the simulation.

```rust
use kernel::{Machine, Credentials, ProcState};

let mut m = Machine::new(1024, 200);                  // 1024 frames, quantum 200
let _ = m.install_dir("/bin");                        // one path per call
let _ = m.install_dir("/home");
let _ = m.install_dir("/home/foo");

// install the bundled userland to /bin
for (name, src) in kernel::USERLAND {
    let image = lang::compile(src).unwrap();
    m.install_file(&format!("/bin/{name}"), &lang::encode_image(&image)).unwrap();
}

// spawn sh, either straight from a compiled image…
let sh = lang::compile(kernel::USERLAND.iter().find(|(n, _)| *n == "sh").unwrap().1).unwrap();
let pid = m.spawn(&sh, "sh").unwrap();

// …or round-tripped through the VFS, like `exec` does
let bytes = m.read_file("/bin/sh").unwrap();
let image = lang::decode_image(&bytes).unwrap();
let pid = m.spawn(&image, "sh").unwrap();

// give it a home directory and a non-root identity, like the front-end does
{
    let vfs = kernel::fs::Vfs::mount(&mut m.disk).unwrap();
    let inum = vfs.namei("/home/foo").unwrap().unwrap();
    let proc = m.procs.get_mut(pid).unwrap();
    proc.cwd = inum;
    proc.cwd_path = "/home/foo".to_owned();
    proc.creds = Credentials { uid: 1000, gid: 1000 };
}

m.console.push_bytes(b"echo hi | cat\n");
for _ in 0..120 {
    m.schedule();                                      // or m.run_steps(n)
    if m.procs.get(pid).map(|p| p.state) == Some(ProcState::Zombie) { break; }
}
let out = String::from_utf8_lossy(&m.console.output()).into_owned();
```

## Userland

The `progs/*.sn` directory contains 26 programs written in sinu. They are embedded through `userland.rs` and installed to `/bin`.

The shell is one of the main integration points. 
It runs commands such as `echo hi | cat` using `fork`, `pipe`, `dup2`, and `exec`.

## Tests

```sh
cargo test -p kernel
```

## Configuration

The main configuration options are:

- `Hardware`: number of cores, RAM frames, disk blocks, disk block size, and NIC ports
- `KernelConfig`: scheduler, worker count, quantum, and feature flags
- `FeatureFlags.multicore`: enable simulated multicore execution
- `FeatureFlags.preemtion`: preemption setting

Other constants control the simulation:

- TICKS_PER_SEC = 1
- DISK_LATENCY = 4 ticks
- DISK_MAX_XFER = 4096
- PIPE_SIZE = 512
- Kernel log ring buffer: 4 KiB, accessible through dmesg

The default hostname is `signull`. The `net.rs` module re-exports the netstack crate for future networking system calls.