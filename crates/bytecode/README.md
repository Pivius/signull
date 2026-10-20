# `bytecode`

The bytecode VM used by SIGNULL. It defines the instruction set, interpreter, syscall interface, and .snx executable format. 
Programs compiled from sinu source run on the VM. The kernel implements the Host trait to connect system calls and memory access to the simulated machine.

```
isa.rs      Opcodes, Program, ProgramImage, VmError
vm.rs       Interpreter, Host trait, NullHost, Frame
abi.rs      Syscall numbers, file flags, buffer layouts, process states
image.rs    .snx encoding and decoding (magic "SNUL", version 1)
```

## Execution model

- The operand stack holds i64 values.
- Local variables are stored in host memory in 8-byte slots below frame_base. Load, Store, and Lea use a slot index to access frame_base + slot * 8.
- Enter(argc, nlocals) creates a stack frame, moves the arguments into the first slots, and zeroes the remaining locals. Call saves the return address and frame base, and Ret restores them.
- Frames grow downward toward a configurable frame_limit. Exceeding the limit raises a trap rather than corrupting memory.
- Comparisons return 0 or 1. Not performs logical negation, while BitNot inverts the bits.
- Division by zero returns a VmError.
- The VM accesses memory only through Host, so it does not manage or access memory directly.

### Limits

All limits have configurable defaults.

| Constant | Default | Meaning |
|---|---|---|
| `DEFAULT_MAX_OPERAND` | 4096 | operand stack depth |
| `DEFAULT_MAX_CALL_DEPTH` | 256 | `Call` nesting |
| `DEFAULT_FRAME_TOP` | `0x0001_0000` | frame region top |
| `DEFAULT_FRAME_LIMIT` | `0` | frame region bottom |

## Instruction set

Each instruction starts with a one-byte opcode followed by its operands in little-endian format.

| Tag | Op | Operand | | Tag | Op | Operand |
|---|---|---|---|---|---|---|
| 0 | `Push(i64)` | i64 | | 30 | `Not` | — |
| 1 | `Pop` | — | | 31–33 | `Jmp`/`Jz`/`Jnz` | u32 target |
| 2 | `Dup` | — | | 34 | `Enter` | u8 argc, u16 nlocals |
| 3 | `Swap` | — | | 35 | `Call` | u32 target |
| 4–6 | `Load`/`Store`/`Lea` | u16 slot | | 36 | `Ret` | — |
| 7–12 | `Add`/`Sub`/`Mul`/`Div`/`Mod`/`Neg` | — | | 37–40 | `Load8`/`16`/`32`/`64` | — |
| 13–19 | `And`/`Or`/`Xor`/`Shl`/`ShrU`/`Sar`/`BitNot` | — | | 41–44 | `Store8`/`16`/`32`/`64` | — |
| 20–25 | `Eq`/`Ne`/`Lt`/`Le`/`Gt`/`Ge` | — | | 45–47 | `SignExt8`/`16`/`32` | — |
| 26–29 | `LtU`/`LeU`/`GtU`/`GeU` | — | | 48 | `PushData` | u16 blob index |
| | | | | 49 | `Syscall` | u16 number, u8 argc |
| | | | | 50 | `Halt` | — |

`Load8`/`Store8` along with their variants, access memory at arbitrary addresses. 
They are used for structs, arrays, and pointer dereferences. Load, Store, and Lea instead operate on local variable slots.

## Image format

The `.snx` format stores the program entry point, local variable count, instructions and data blobs

```
"SNUL" | version:u32 | entry:u32 | locals:u32 | code_len:u32
      | ops…
      | data_len:u32
      | ( blob_len:u32 | blob bytes )…
```

All integer fields and instruction operands use little-endian encoding.

`ProgramImage { code, data, locals, entry }` is the serializable representation before memory mapping. 
`Program { code, data, data_addrs }` is the mapped representation used by the VM.

`encode_image` and `decode_image` handle conversion between the two formats. Invalid magic bytes and truncated images are rejected.

## `Host` trait

The VM delegates system calls and memory access to its host. This allows the same interpreter to run inside the kernel, in tests, or in other environments.

```rust
pub trait Host {
    fn syscall(&mut self, number: u16, args: &[i64]) -> SyscallResult;
    fn load(&mut self, addr: u64, width: MemWidth) -> Result<u64, MemError>;
    fn store(&mut self, addr: u64, width: MemWidth, value: u64) -> Result<(), MemError>;
}
```

`SyscallResult { value, action, retry, stop }` tells the VM what happened after a system call. 
The host can let execution continue, block the process while waiting for I/O, exit with a code, or stop the VM.

`NullHost` provides a no-op host for tests that only need to run computations. `MemWidth` supports B8, B16, B32, and B64 memory accesses.

## Syscall ABI

`abi.rs` defines the syscall numbers shared by the compiler, kernel, and userland.

```
1 exit    2 write   3 read    4 sbrk    5 getpid  6 yield   7 sleep
8 open    9 close  10 dup    11 lseek  12 unlink 13 mkdir  14 stat
15 argc   16 arg   17 fork   18 pipe   19 wait   20 exec   21 dup2
22 chdir  23 getcwd 24 getuid 25 getgid 26 uptime 27 uname 28 statfs
29 ps     30 kill  31 dmesg  32 date   33 nblocks
```

The ABI also defines standard file descriptor numbers, `O_*` and `SEEK_*` flags, process state codes (`PS_*`), and the packed memory layouts used by several system calls.

- uname: 320 bytes (`UTSNAME_LEN`)
- statfs: 20 bytes
- ps: 32-byte `ProcEnt` containing the PID, PPID, UID, process state, and a 16-byte name
- date: 24 bytes
- TRAP_EXIT_CODE: 101, used for bounds-check failures and panics

## Usage

```rust
use bytecode::{Op, Program, Vm, NullHost};

let program = Program {
    code: vec![
        Op::Push(5), Op::Store(0),
        Op::Load(0), Op::Halt,
    ],
    data: vec![],
    data_addrs: vec![],
};
let mut vm = Vm::new(program, 1);
let steps = vm.run(&mut NullHost, 1_000).unwrap();
```

## Tests

```sh
cargo test -p bytecode
```

The tests cover image encoding and decoding, invalid and truncated images, iterative and recursive factorial, syscall argument ordering, memory operations through a test host, frame isolation, Lea, bitwise and unsigned operations, and the configured limits for call depth, operand stack size, and frame memory.
