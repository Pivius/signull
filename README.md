# SIGNULL Engine

A self-contained systems stack built around the `sinu` systems language, a bytecode VM, an xv6-inspired Unix-like kernel, and a network stack.

The kernel runs as part of the program rather than as a standalone operating system. 
A frontend, such as a game or terminal emulator, creates a Machine and interacts with it through its API. 
The kernel handles processes, virtual memory, the filesystem, pipes, and scheduling.

## Crates

| Crate | Description |
|---|---|
| [`bytecode`](crates/bytecode/README.md) | Stack-machine ISA, VM interpreter, syscall ABI, `.snx` image format |
| [`lang`](crates/lang/README.md) | The `sinu` compiler: lexer, parser, type checker, codegen to bytecode (`sinuc` binary) |
| [`kernel`](crates/kernel/README.md) | Unix-like kernel simulation: processes, memory, VFS, pipes, syscalls, schedulers |
| [`netstack`](crates/netstack/README.md) | Ethernet / IPv4 / TCP header parse & serialize |

## Systems Language

```c
// crates/kernel/progs.
struct Span { word start; word len; };

int fact(int n) {
    if (n <= 1) { return 1; }
    return n * fact(n - 1);
}

void main() {
    u8 buf[16];
    buf[0] = 0x2a;
    puts("hello\n");
    exit(cast<sword>(fact(5)));   // 120
}
```

A C-like language with strict typing. It has no implicit conversions or C-style casts, using `cast<T>(x)` instead. 
Pointers use explicit *T syntax, uninitialized storage is zero-filled, and built-in system calls like write, fork, and exec cannot be redefined.
Full reference: **[docs/sinu.md](docs/sinu.md)**.

## Quick start

```sh
cargo build            # build the workspace
cargo test             # run everything, including end-to-end OS simulation tests

# compile a program to a .snx image
cargo run -p lang --bin sinuc -- crates/kernel/progs/echo.sn -o echo.snx

# compile & run one program on the kernel in a test harness — see crates/kernel
cargo test -p kernel
```

The tests in crates/kernel/src/machine.rs cover things like running a shell pipeline (echo hi | cat), fork/wait, pipes, dup2 redirection, 
disk latency blocking one process while another keeps running, scheduler switching, faults, and installing the full userland with user accounts.

## Kernel overview

- **Based on xv6**: sleep/wakeup channels, a physical frame allocator, an inode-based filesystem with direct and indirect blocks, process creation and cleanup, trap frames, and reference-counted pipe ends.
- **Memory**: 4 KiB pages, two-level page tables with RISC-V-style permission flags, per-process address spaces, copyin/copyout, an sbrk heap, and a 16 KiB stack.
- **Scheduler**: pluggable FIFO, round-robin, priority, MLFQ.
- **Userland**: 26 programs written in `sinu`, installed to `/bin` at boot, including a
  pipeline shell and a modeless editor (`nano`).

## License

MIT.
