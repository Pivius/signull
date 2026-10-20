# `lang`

The `sinu` compiler. 
It turns source code into bytecode through four stages: lexing, parsing, type checking, and code generation. 
The output is a `bytecode::ProgramImage` that can be loaded by the kernel. The crate also provides the `sinuc` command-line compiler.

Full language reference: **[docs/sinu.md](../../docs/sinu.md)**

```
lexer.rs      Tokenizer with line and column information
ast.rs        AST definitions for programs, statements, expressions, and types
parser.rs     Recursive-descent parser with type-name prescan
types.rs      Resolved types, sizes, and integer ranges
codegen.rs    Type checker, code generator, and peephole optimizer
diag.rs       Source spans and compilation errors
bin/sinuc.rs  Compiler CLI
```

## Usage

### As a library

```rust
use lang::compile;

let image = compile(source)?;               // bytecode::ProgramImage
let bytes = bytecode::image::encode_image(&image)?;
```

compile returns a `bytecode::ProgramImage`. The image can then be encoded as a `.snx` file for execution by the kernel.

The crate also exposes `parse_source`, `lex`, `parse`, the AST types, `Ty`, and `CompileError`.

### As a CLI

```sh
cargo run -p lang --bin sinuc -- path/to/prog.sn -o prog.snx
```

This outputs a `.snx` bytecode image with the `SNUL` magic header, ready to be loaded by the kernel.

## Type checking

The compiler enforces strict typing and rejects programs that violate the language rules.

- Exact type matching. Assignments, function arguments, and return values must match their declared types. There are no implicit conversions. Integer literals are checked against the expected type and its range.
- Explicit casts. C-style casts such as `(u32)x` are rejected. Use `cast<u32>(x)` instead.
- Pointers for aggregates. Structs and arrays cannot be passed or returned by value. Use pointers `(*T)`.
- Guaranteed returns. Functions with a return type must return a value on every path. This can be a final return, a terminating `exit()` or `panic()`, or an if/else where both branches return or terminate.
- Valid assignment targets. Assignments must target a variable, array index, struct field, or dereferenced pointer. Whole structs and arrays cannot be assigned.
- Array bounds checks. Runtime checks are generated for non-constant array indices. A failed check traps with exit code 101.
- Built-in system calls. Names such as `write`, `fork`, `exec`, and puts compile directly to their corresponding operations and cannot be redefined by user code.

## Tests

```sh
cargo test -p lang
```