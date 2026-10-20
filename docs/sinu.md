# `sinu` — the SIGNULL systems language

`sinu` is a small, strictly typed systems language with C-like syntax. 
Source files use the `.sn` extension. 
The `sinuc` compiler, included in the lang crate, compiles them into bytecode images `.snx` that run on the SIGNULL kernel's VM.

The main goals are to keep the language small and predictable:

- Explicit: No implicit conversions, operator overloading, or hidden allocations.
- Low-level: Raw pointers, sized memory access, sizeof, offsetof, and direct syscalls.
- Small: Around 17 keywords, one declaration syntax, no preprocessor, and no C-style casts.

## Hello, world

```c
void main() {
    puts("hello, signull\n");
}
```

Compile with:

```sh
cargo run -p lang --bin sinuc -- hello.sn -o hello.snx
```

Programs run as regular files in the kernel. 
The bundled userland programs live in `crates/kernel/progs/*.sn` and are installed under `/bin` at boot.

## Lexical structure

- **Comments**: `// line` and `/* block */`. Block comments cannot be nested
- **Integer literals**: decimal, `0x` hex, `0b` binary, `0o` octal. `_` separators allowed
  (`0x_d00d_feed`). Literals are range-checked against their target type.
- **Char literals**: `'a'`, `'\n'`, `'\t'`, `'\r'`, `'\0'`, `'\\'`, `'\''`, `'\x1b'`.
  a char is a single byte, usable wherever a `u8`-like integer is expected.
- **String literals**: `"..."` with escapes `\n \t \r \0 \\ \" \' \xHH`. Strings are only
  values of the *literal* type `str`, accepted by `puts`, `panic`, and `open`. In memory
  a string value is a 4-byte little-endian length prefix, then the bytes, then a NUL,
  when passed to `open` the pointer skips the prefix, so the kernel sees a C string.

## Types

| Kind | Types |
|---|---|
| Boolean | `bool` (1 byte, `0`/`1`) |
| Signed | `i8` `i16` `i32` `i64` |
| Unsigned | `u8` `u16` `u32` `u64` |
| Native word | `word` (unsigned) · `sword` (signed) — **32-bit** |
| Alias | `int` = `i32` |
| Void | `void` (return type only) |
| Pointer | `*T`, multi-level `**T`, … |
| Array | `T name[N]`, multi-dim `T name[N][M]` |
| Aggregate | user `struct`, `enum` |
| String literal | `str` |

Notes:

- Pointers are 4 bytes. `null` is the null-pointer constant.
- Uninitialized storage is **zero-filled**.
- Structs use natural alignment unless declared with `layout(packed)`. 
- Arrays and structs cannot be passed or returned by value. Pass a pointer instead.

### Pointers

```c
void main() {
    *u32 p;
    **u32 pp;              // multi-level
    u8  buf[4];
    *u8 bytes = null;

    u32 v = 42;
    p = &v;                // address-of
    *p = 7;                // deref
    u8 *q = &buf[0];
    u8 c = *(q + 2);       // pointer arithmetic scales by sizeof(*q)
}
```

Pointer arithmetic with an integer scales by the size of the pointed-to type. 
For example, `q + 2` advances by two bytes when `q` is a `*u8`. Arithmetic between two pointers is not supported.

## Program structure

Top-level declarations can define functions, global variables, structs, enums, type aliases, and an optional entry point. 
Declarations end with ;, including struct, enum, type, and entry declarations.

```c
static u32 counter = 0;

struct Span { word start; word len; };
struct PacketHeader layout(packed) { u8 version; u16 length; };

enum ProcessState : u32 { Ready, Running = 5, Sleeping };
type ProcessId = u32;

entry start;
void start() { }
```

All top-level storage is static, so the `static` keyword is optional.

### Entry point

The entry point defaults to `main`. An entry `name`; declaration selects a different function. 
The entry function cannot take parameters. If it returns a value, that value becomes the process exit code; otherwise, the exit code is 0.

### Functions

```c
sword add(sword a, sword b) {
    return a + b;
}

void main() {
    sword x = add(2, 3);
}
```

Functions with a return type must have a provable return path. 
The final statement must be a return, a call to exit() or panic(), or an if/else where both branches return. 

## Statements

```c
Type name = expr;      // declaration with initialiser
Type name;             // declaration, zero-filled
name = expr;           // assignment

expr;                  // expression statement

if (cond) { } else if (cond) { } else { }

while (cond) { }

for (i = 0; i < n; i = i + 1) { }        // C-style; init may also be a declaration
for (u32 i = 0; i < n; i = i + 1) { }    //   or empty: for (;;)
for (;;) { break; continue; }

return;                // or: return expr;
```

- The `for` loop's step is an expression or assignment without a trailing semicolon.
- `break` and `continue` outside a loop are compile errors.
- Whole-array and whole-struct assignment is not supported. Copy elements or fields individually.
- `stack` is accepted as a storage specifier but currently uses ordinary stack-frame storage.
- `static` inside a function is an error. Declare static objects at the top level instead.
- Array accesses with non-constant indices include a bounds check. An out-of-range access traps with exit code 101. Constant, in-range indices skip the check.

## Expressions

Operators, low to high precedence. Operators at the same level are left-associative:

| Prec | Operators |
|---|---|
| 1 | `\|\|` |
| 2 | `&&` |
| 3 | `\|` |
| 4 | `^` |
| 5 | `&` |
| 6 | `==` `!=` |
| 7 | `<` `<=` `>` `>=` |
| 8 | `<<` `>>` |
| 9 | `+` `-` |
| 10 | `*` `/` `%` |

Unary : `-` `!` `~` `&` (addr-of) `*` (deref).

`&&` and `||` short-circuit. Comparisons yield `bool`. Signedness of comparisons and
shifts follows the operand types (`Lt` vs `LtU`, `Sar` vs `ShrU`). Division by zero
traps. Narrow results are normalized (sign-extended or masked) to their type.

### Literals

```c
u32 a = 42;
u32 b = 0x2a;
u32 c = 0b10_1010;
u8  d = '\n';
bool e = true;
*void f = null;
u8  arr[3] = [1, 2, 3];
Span s = { start = 0, len = 8 };
```

Aggregate initialisers use `field = expr` and may nest. Array fields are not allowed
inside aggregates.

## Intrinsics

| Intrinsic | Meaning |
|---|---|
| `cast<T>(x)` | Explicit numeric conversion. Does not support bool, void, str, arrays, or structs, or conversions between pointers and enums. |
| `load<T>(addr)` | Load a value of type T from a raw address. |
| `store<T>(addr, value)` | Store a value of type T at a raw address. |
| `sizeof(T)` | Size in bytes, evaluated at compile time. |
| `alignof(T)` | Alignment in bytes, evaluated at compile time. |
| `offsetof(T, field)` | Field offset in bytes, evaluated at compile time. |
| `addr_of(x)` | Same as `&x`. |
| `syscall(number, args...)` | Call a syscall directly. The syscall number must be a constant integer. |

```c
void main() {
    u8  s  = cast<u8>(200);
    u32 v  = load<u32>(addr);
    store<u32>(addr, v);
    u32 n  = sizeof(u32);
    u32 a  = alignof(u64);
    u32 o  = offsetof(PacketHeader, length);
}
```

`trap(...)` is recognised by the parser but not yet implemented.

## Builtins and syscalls

The following names resolve directly to syscalls and cannot be shadowed by user-defined functions or globals. 
Unless noted otherwise, they return `sword`, with `-1` indicating an error.

Some syscalls, including `uname`, `ps`, `date`, `statfs`, `dmesg`, `nblocks`, `kill`, `getuid`, `getgid`, and `uptime`, are only accessible through raw `syscall(n, ...)` calls. 
Their buffer layouts are documented in `bytecode::abi`.

| Builtin | Signature |
|---|---|
| `write` | `write(fd: sword, buf: *u8 \| str, len: sword) -> sword` |
| `read` | `read(fd: sword, buf: *u8, len: sword) -> sword` |
| `open` | `open(path: str, flags: sword) -> sword` |
| `close` | `close(fd: sword) -> sword` |
| `dup` | `dup(fd: sword) -> sword` |
| `dup2` | `dup2(old: sword, new: sword) -> sword` |
| `lseek` | `lseek(fd: sword, offset: sword, whence: sword) -> sword` |
| `unlink` | `unlink(path: str) -> sword` |
| `mkdir` | `mkdir(path: str) -> sword` |
| `stat` | `stat(path: str, buf: *u8) -> sword` |
| `chdir` | `chdir(path: *u8) -> sword` |
| `exit` | `exit(code: sword)` |
| `panic` | `panic(msg: str)` — writes to stderr, exits with code **101** |
| `puts` | `puts(msg: str)` — writes to stdout |
| `getpid` | `getpid() -> sword` |
| `yield` | `yield()` |
| `sleep` | `sleep(ticks: word)` |
| `sbrk` | `sbrk(delta: sword) -> sword` |
| `alloc` | `alloc(n: word) -> *u8` — `sbrk` wrapper |
| `argc` | `argc() -> sword` |
| `arg` | `arg(i: sword, buf: *u8, len: sword) -> sword` |
| `fork` | `fork() -> sword` — child returns `0` |
| `pipe` | `pipe(fds: *sword) -> sword` |
| `wait` | `wait(status: *sword) -> sword` |
| `exec` | `exec(path: *u8, argc: sword, argv: *word) -> sword` |
| `syscall` | `syscall(number, args...) -> sword` |

## Example: fork, pipes, and exec

```c
void main() {
    sword fds[2];
    if (pipe(&fds[0]) != 0) { panic("pipe failed\n"); }

    sword pid = fork();
    if (pid == 0) {
        close(fds[0]);
        dup2(fds[1], 1);          // child's stdout -> pipe
        close(fds[1]);
        exec("/bin/echo", 2, argv);   // argv: *word of pointers
    } else {
        close(fds[1]);
        u8 buf[64];
        sword n = read(fds[0], &buf[0], 64);
        close(fds[0]);
        wait(&status);
    }
}
```

Here, `argv` is an array of pointers represented as `*word`. 
The full pipeline shell implementation is in `crates/kernel/progs/sh.sn`.

## Type rules

The compiler requires exact type matches for assignments, function arguments, and return values. There are no implicit conversions.

```c
u32 x = 0;            // Valid: the literal fits u32
u32 y = some_i32;     // Error: types do not match
u32 z = cast<u32>(some_i32);
```

Other rules:

- Integer literals are typed using their context and checked for overflow.
- Pointer types must match exactly. For example, `*u8` and `**u8` are different types.
- Structs and arrays cannot be passed or returned by value. Use pointers instead.
- `bool` does not convert to an integer. Use explicit conditional logic when needed.

## Optimization

The compiler applies a peephole optimization pass that includes:

- Constant folding, such as `Push`; `Push`; `op`.
- Removing identity operations, such as `x + 0`, `x * 1`, and `x & -1`.
- Collapsing duplicate sign extensions.
- Eliminating constant branches and constant bounds checks.
- Remapping jump and call targets after optimization.

## Compilation model

`sinuc` produces a `bytecode::ProgramImage` containing the code, data blobs, local-slot count, and entry point. 
The compiler serializes this image to an `.snx` file with the magic value `SNUL`.

When the kernel executes a program, it decodes the image, maps the data and a 16 KiB stack into a fresh address space, and runs the program on the VM. 
Syscall numbers are defined in `bytecode::abi` and shared by the compiler and kernel.

See also: [bytecode ISA & ABI](../crates/bytecode/README.md) ·
[kernel](../crates/kernel/README.md)
