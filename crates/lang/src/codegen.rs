//! Type checking and code generation: `sinu` AST -> `bytecode::ProgramImage`.
//!
//! Globals in image's data region and locals including fixed arrays
//! in stack-frame memory, both reached through the host.

use std::collections::{HashMap, HashSet};

use bytecode::abi::{
    STDERR, STDOUT, SYS_ARG, SYS_ARGC, SYS_CHDIR, SYS_CLOSE, SYS_DUP, SYS_DUP2, SYS_EXEC, SYS_EXIT,
    SYS_FORK, SYS_GETPID, SYS_LSEEK, SYS_MKDIR, SYS_OPEN, SYS_PIPE, SYS_READ, SYS_SBRK, SYS_SLEEP,
    SYS_STAT, SYS_UNLINK, SYS_WAIT, SYS_WRITE, SYS_YIELD, TRAP_EXIT_CODE,
};
use bytecode::{Op, ProgramImage};

use crate::ast::{BinOp, Block, Expr, Func, Global, Item, Program, Stmt, UnOp};
use crate::diag::{CompileError, Span};
use crate::types::Ty;

/// Compile `sinu` source into a bytecode image ready for the kernel.
pub fn compile(source: &str) -> Result<ProgramImage, CompileError> {
    let program = crate::parse_source(source)?;
    Compiler::new(source).run(&program)
}

struct LineIndex {
    newlines: Vec<usize>,
}

impl LineIndex {
    fn new(source: &str) -> Self {
        Self {
            newlines: source
                .bytes()
                .enumerate()
                .filter_map(|(i, b)| (b == b'\n').then_some(i))
                .collect(),
        }
    }

    fn locate(&self, offset: usize) -> (usize, usize) {
        let line = self.newlines.partition_point(|&n| n < offset);
        let start = if line == 0 {
            0
        } else {
            self.newlines[line - 1] + 1
        };
        (line + 1, offset.saturating_sub(start) + 1)
    }
}

#[derive(Clone)]
struct FuncSig {
    params: Vec<Ty>,
    ret: Option<Ty>,
    addr: usize,
}

#[derive(Clone)]
struct GlobalInfo {
    data: u16,
    ty: Ty,
}

#[derive(Clone)]
struct LocalInfo {
    slot: u16,
    ty: Ty,
}

/// A resolved struct: fields with byte offsets under the declared layout.
#[derive(Clone)]
struct StructDef {
    fields: Vec<FieldDef>,
    size: usize,
    align: usize,
}

#[derive(Clone)]
struct FieldDef {
    name: String,
    ty: Ty,
    offset: usize,
}

/// A fieldless enum: each variant is an integer of the underlying type.
struct EnumDef {
    underlying: Ty,
}

/// A `type Name = T` alias with its resolved target.
struct AliasDef {
    target: Ty,
}

/// Result of trying to simplify `Push(c); op`.
enum Identity {
    /// The pair is an identity; drop both ops.
    Drop,
    /// Not an identity pattern.
    None,
}

fn is_signext(op: Op) -> bool {
    matches!(op, Op::SignExt8 | Op::SignExt16 | Op::SignExt32)
}

enum ArrRef {
    Local {
        slot: u16,
        elem: Ty,
        len: usize,
    },
    Global {
        data: u16,
        elem: Ty,
        len: usize,
    },
    /// `(*ptr)[i]`.
    DerefBase {
        inner: Box<Expr>,
        elem: Ty,
        len: usize,
    },
    /// `place.field[i]`.
    FieldBase {
        base: Box<Expr>,
        name: String,
        span: Span,
        elem: Ty,
        len: usize,
    },
}

#[derive(Clone, Copy)]
enum Builtin {
    Write,
    Read,
    Open,
    Close,
    Dup,
    Exit,
    GetPid,
    Yield,
    Sleep,
    Sbrk,
    Alloc,
    Panic,
    Puts,
    Syscall,
    Lseek,
    Unlink,
    Mkdir,
    Stat,
    Argc,
    Arg,
    Fork,
    Pipe,
    Wait,
    Exec,
    Dup2,
    Chdir,
}

fn builtin_of(name: &str) -> Option<Builtin> {
    Some(match name {
        "write" => Builtin::Write,
        "read" => Builtin::Read,
        "open" => Builtin::Open,
        "close" => Builtin::Close,
        "dup" => Builtin::Dup,
        "exit" => Builtin::Exit,
        "getpid" => Builtin::GetPid,
        "yield" => Builtin::Yield,
        "sleep" => Builtin::Sleep,
        "sbrk" => Builtin::Sbrk,
        "alloc" => Builtin::Alloc,
        "panic" => Builtin::Panic,
        "puts" => Builtin::Puts,
        "syscall" => Builtin::Syscall,
        "lseek" => Builtin::Lseek,
        "unlink" => Builtin::Unlink,
        "mkdir" => Builtin::Mkdir,
        "stat" => Builtin::Stat,
        "argc" => Builtin::Argc,
        "arg" => Builtin::Arg,
        "fork" => Builtin::Fork,
        "pipe" => Builtin::Pipe,
        "wait" => Builtin::Wait,
        "exec" => Builtin::Exec,
        "dup2" => Builtin::Dup2,
        "chdir" => Builtin::Chdir,
        _ => return None,
    })
}

impl Builtin {
    fn name(self) -> &'static str {
        match self {
            Builtin::Write => "write",
            Builtin::Read => "read",
            Builtin::Open => "open",
            Builtin::Close => "close",
            Builtin::Dup => "dup",
            Builtin::Exit => "exit",
            Builtin::GetPid => "getpid",
            Builtin::Yield => "yield",
            Builtin::Sleep => "sleep",
            Builtin::Sbrk => "sbrk",
            Builtin::Alloc => "alloc",
            Builtin::Panic => "panic",
            Builtin::Puts => "puts",
            Builtin::Syscall => "syscall",
            Builtin::Lseek => "lseek",
            Builtin::Unlink => "unlink",
            Builtin::Mkdir => "mkdir",
            Builtin::Stat => "stat",
            Builtin::Argc => "argc",
            Builtin::Arg => "arg",
            Builtin::Fork => "fork",
            Builtin::Pipe => "pipe",
            Builtin::Wait => "wait",
            Builtin::Exec => "exec",
            Builtin::Dup2 => "dup2",
            Builtin::Chdir => "chdir",
        }
    }

    fn params(self) -> Vec<Ty> {
        match self {
            Builtin::Write | Builtin::Read => {
                vec![Ty::Sword, Ty::Ptr(Box::new(Ty::U8)), Ty::Sword]
            }
            Builtin::Open => vec![Ty::Str, Ty::Sword],
            Builtin::Close | Builtin::Dup | Builtin::Exit => vec![Ty::Sword],
            Builtin::GetPid | Builtin::Yield => vec![],
            Builtin::Sleep => vec![Ty::Word],
            Builtin::Sbrk => vec![Ty::Sword],
            Builtin::Alloc => vec![Ty::Word],
            Builtin::Panic | Builtin::Puts => vec![Ty::Str],
            Builtin::Syscall => vec![], // variadic; checked at the call site
            Builtin::Lseek => vec![Ty::Sword, Ty::Sword, Ty::Sword],
            Builtin::Unlink | Builtin::Mkdir => vec![Ty::Str],
            Builtin::Stat => vec![Ty::Str, Ty::Ptr(Box::new(Ty::U8))],
            Builtin::Argc => vec![],
            Builtin::Arg => vec![Ty::Sword, Ty::Ptr(Box::new(Ty::U8)), Ty::Sword],
            Builtin::Fork => vec![],
            Builtin::Pipe => vec![Ty::Ptr(Box::new(Ty::Sword))],
            Builtin::Wait => vec![Ty::Ptr(Box::new(Ty::Sword))],
            Builtin::Exec => vec![
                Ty::Ptr(Box::new(Ty::U8)),
                Ty::Sword,
                Ty::Ptr(Box::new(Ty::Word)),
            ],
            Builtin::Dup2 => vec![Ty::Sword, Ty::Sword],
            Builtin::Chdir => vec![Ty::Ptr(Box::new(Ty::U8))],
        }
    }

    fn ret(self) -> Option<Ty> {
        match self {
            Builtin::Write
            | Builtin::Read
            | Builtin::Open
            | Builtin::Close
            | Builtin::Dup
            | Builtin::Exit
            | Builtin::GetPid
            | Builtin::Sbrk
            | Builtin::Syscall
            | Builtin::Lseek
            | Builtin::Unlink
            | Builtin::Mkdir
            | Builtin::Stat
            | Builtin::Argc
            | Builtin::Arg
            | Builtin::Fork
            | Builtin::Pipe
            | Builtin::Wait
            | Builtin::Exec
            | Builtin::Dup2
            | Builtin::Chdir => Some(Ty::Sword),
            Builtin::Alloc => Some(Ty::Ptr(Box::new(Ty::U8))),
            Builtin::Yield | Builtin::Sleep | Builtin::Panic | Builtin::Puts => None,
        }
    }

    /// Discards the value the syscall leaves on the stack.
    fn discards_result(self) -> bool {
        self.ret().is_none() && !matches!(self, Builtin::Panic)
    }
}

/// A break/continue patch site inside the innermost loop.
struct LoopCtx {
    breaks: Vec<usize>,
    continues: Vec<usize>,
}

struct Compiler {
    lines: LineIndex,
    code: Vec<Op>,
    data: Vec<Vec<u8>>,
    funcs: HashMap<String, FuncSig>,
    globals: HashMap<String, GlobalInfo>,
    scopes: Vec<HashMap<String, LocalInfo>>,
    next_slot: u16,
    frame_slots: u16,
    temp_slots: Vec<u16>,
    argc: u8,
    cur_ret: Option<Ty>,
    call_patches: Vec<(usize, String, Span)>,
    structs: HashMap<String, StructDef>,
    pending_structs: HashSet<String>,
    enums: HashMap<String, EnumDef>,
    /// Flat table of bare variant names -> (enum name, value).
    variant_values: HashMap<String, (String, i64)>,
    aliases: HashMap<String, AliasDef>,
    alias_decls: Vec<(String, crate::ast::Type, Span)>,
    loops: Vec<LoopCtx>,
    /// The selected entry function (`entry name;` or fallback `main`).
    entry: Option<(String, Span)>,
}

impl Compiler {
    fn new(source: &str) -> Self {
        Self {
            lines: LineIndex::new(source),
            code: Vec::new(),
            data: Vec::new(),
            funcs: HashMap::new(),
            globals: HashMap::new(),
            scopes: Vec::new(),
            next_slot: 0,
            frame_slots: 0,
            temp_slots: Vec::new(),
            argc: 0,
            cur_ret: None,
            call_patches: Vec::new(),
            structs: HashMap::new(),
            pending_structs: HashSet::new(),
            enums: HashMap::new(),
            variant_values: HashMap::new(),
            aliases: HashMap::new(),
            alias_decls: Vec::new(),
            loops: Vec::new(),
            entry: None,
        }
    }

    fn run(mut self, program: &Program) -> Result<ProgramImage, CompileError> {
        self.collect_aliases(program)?;
        self.collect_types(program)?;
        self.collect_signatures(program)?;
        self.collect_entry(program)?;

        for item in &program.items {
            if let Item::Global(g) = item {
                self.declare_global(g)?;
            }
        }

        let at = self.emit(Op::Call(0));
        let (entry_name, entry_span) = self.program_entry(program)?;
        self.call_patches.push((at, entry_name.clone(), entry_span));
        let entry_returns_int = self
            .funcs
            .get(&entry_name)
            .and_then(|s| s.ret.clone())
            .is_some_and(|t| t.is_int());
        if !entry_returns_int {
            self.emit(Op::Push(0));
        }
        self.emit(Op::Syscall(SYS_EXIT, 1));
        self.emit(Op::Halt);

        for item in &program.items {
            if let Item::Func(f) = item {
                self.compile_func(f)?;
            }
        }

        self.resolve_calls()?;
        self.peephole();

        Ok(ProgramImage {
            code: self.code,
            data: self.data,
            locals: 0,
            entry: 0,
        })
    }

    // type declarations

    fn collect_aliases(&mut self, program: &Program) -> Result<(), CompileError> {
        for item in &program.items {
            if let Item::TypeAlias(a) = item {
                self.alias_decls
                    .push((a.name.clone(), a.ty.clone(), a.span));
            }
        }
        let names: Vec<String> = self.alias_decls.iter().map(|(n, _, _)| n.clone()).collect();
        for name in names {
            let mut visiting = HashSet::new();
            self.resolve_alias(&name, &mut visiting)?;
        }
        Ok(())
    }

    fn resolve_alias(
        &mut self,
        name: &str,
        visiting: &mut HashSet<String>,
    ) -> Result<Ty, CompileError> {
        if let Some(def) = self.aliases.get(name) {
            return Ok(def.target.clone());
        }
        if !visiting.insert(name.to_string()) {
            return Err(CompileError::new(
                format!("type alias `{name}` is defined in terms of itself"),
                1,
                1,
            ));
        }
        let Some((_, ast_ty, _span)) = self.alias_decls.iter().find(|(n, _, _)| n == name).cloned()
        else {
            return Err(CompileError::new(
                format!("unknown type alias `{name}`"),
                1,
                1,
            ));
        };
        let target = self.resolve_ty_with(&ast_ty, visiting)?;
        visiting.remove(name);
        self.aliases.insert(
            name.to_string(),
            AliasDef {
                target: target.clone(),
            },
        );
        Ok(target)
    }

    fn collect_types(&mut self, program: &Program) -> Result<(), CompileError> {
        for item in &program.items {
            if let Item::Enum(e) = item {
                if self.enums.contains_key(&e.name) {
                    return Err(self.err(e.span, format!("duplicate enum `{}`", e.name)));
                }
                let underlying = self.resolve_ty(&e.underlying)?;
                let Some((lo, hi)) = underlying.int_range() else {
                    return Err(self.err(e.span, "enum underlying type must be an integer"));
                };
                let mut variants = HashMap::new();
                let mut next = 0i64;
                for v in &e.variants {
                    if variants.contains_key(&v.name) {
                        return Err(self.err(v.span, format!("duplicate variant `{}`", v.name)));
                    }
                    if self.variant_values.contains_key(&v.name) {
                        return Err(
                            self.err(v.span, format!("duplicate variant name `{}`", v.name))
                        );
                    }
                    let value = v.value.unwrap_or(next);
                    if value < lo || value > hi {
                        return Err(self.err(
                            v.span,
                            format!("variant value out of range for `{underlying}`"),
                        ));
                    }
                    variants.insert(v.name.clone(), value);
                    self.variant_values
                        .insert(v.name.clone(), (e.name.clone(), value));
                    next = value + 1;
                }
                if variants.is_empty() {
                    return Err(self.err(e.span, format!("enum `{}` has no variants", e.name)));
                }
                self.enums.insert(e.name.clone(), EnumDef { underlying });
            }
        }

        let mut raw: HashMap<String, &crate::ast::StructDecl> = HashMap::new();
        for item in &program.items {
            if let Item::Struct(s) = item {
                if self.pending_structs.contains(&s.name) {
                    return Err(self.err(s.span, format!("duplicate struct `{}`", s.name)));
                }
                self.pending_structs.insert(s.name.clone());
                raw.insert(s.name.clone(), s);
            }
        }

        let names: Vec<String> = raw.keys().cloned().collect();
        let mut visiting = HashSet::new();
        for name in names {
            self.resolve_struct(&name, &raw, &mut visiting)?;
        }
        Ok(())
    }

    fn resolve_struct(
        &mut self,
        name: &str,
        raw: &HashMap<String, &crate::ast::StructDecl>,
        visiting: &mut HashSet<String>,
    ) -> Result<(), CompileError> {
        if self.structs.contains_key(name) {
            return Ok(());
        }
        if !visiting.insert(name.to_string()) {
            return Err(CompileError::new(
                format!("struct `{name}` recursively contains itself (infinite size)"),
                1,
                1,
            ));
        }
        let decl = raw
            .get(name)
            .ok_or_else(|| CompileError::new(format!("unknown struct `{name}`"), 1, 1))?;
        let packed = decl.packed;
        let mut fields = Vec::new();
        let mut offset = 0usize;
        let mut max_align = 1usize;
        for f in &decl.fields {
            if fields.iter().any(|d: &FieldDef| d.name == f.name) {
                return Err(self.err(f.span, format!("duplicate field `{}`", f.name)));
            }
            let ty = self.resolve_ty(&f.ty)?;
            let size = self.layout_size(&ty, raw, visiting)?;
            let align = if packed { 1 } else { self.align_of(&ty) };
            offset = align_up(offset, align);
            fields.push(FieldDef {
                name: f.name.clone(),
                ty,
                offset,
            });
            offset += size;
            max_align = max_align.max(align);
        }
        if !packed {
            offset = align_up(offset, max_align);
        }
        visiting.remove(name);
        self.structs.insert(
            name.to_string(),
            StructDef {
                fields,
                size: offset,
                align: max_align,
            },
        );
        Ok(())
    }

    /// Byte size of a struct field type, resolving by-value struct deps.
    fn layout_size(
        &mut self,
        ty: &Ty,
        raw: &HashMap<String, &crate::ast::StructDecl>,
        visiting: &mut HashSet<String>,
    ) -> Result<usize, CompileError> {
        match ty {
            Ty::Struct(name) => {
                self.resolve_struct(name, raw, visiting)?;
                Ok(self.structs[name].size)
            }
            Ty::Array { elem, len } => self
                .layout_size(elem, raw, visiting)
                .map(|s| s.saturating_mul(*len)),
            _ => Ok(self.size_of(ty)),
        }
    }

    fn resolve_ty_with(
        &mut self,
        ty: &crate::ast::Type,
        visiting: &mut HashSet<String>,
    ) -> Result<Ty, CompileError> {
        Ok(match ty {
            crate::ast::Type::Bool => Ty::Bool,
            crate::ast::Type::I8 => Ty::I8,
            crate::ast::Type::I16 => Ty::I16,
            crate::ast::Type::I32 => Ty::I32,
            crate::ast::Type::I64 => Ty::I64,
            crate::ast::Type::U8 => Ty::U8,
            crate::ast::Type::U16 => Ty::U16,
            crate::ast::Type::U32 => Ty::U32,
            crate::ast::Type::U64 => Ty::U64,
            crate::ast::Type::Word => Ty::Word,
            crate::ast::Type::Sword => Ty::Sword,
            crate::ast::Type::Void => Ty::Void,
            crate::ast::Type::Array { elem, len } => Ty::Array {
                elem: Box::new(self.resolve_ty_with(elem, visiting)?),
                len: *len,
            },
            crate::ast::Type::Struct(name) => {
                if !self.pending_structs.contains(name) && !self.aliases.contains_key(name) {
                    return Err(CompileError::new(
                        format!("unknown struct type `{name}`"),
                        1,
                        1,
                    ));
                }
                if self.aliases.contains_key(name) {
                    return self.resolve_alias(name, visiting);
                }
                Ty::Struct(name.clone())
            }
            crate::ast::Type::Enum(name) => {
                if !self.enums.contains_key(name) {
                    return Err(CompileError::new(
                        format!("unknown enum type `{name}`"),
                        1,
                        1,
                    ));
                }
                Ty::Enum(name.clone())
            }
            crate::ast::Type::Alias(name) => self.resolve_alias(name, visiting)?,
            crate::ast::Type::Ptr(elem) => Ty::Ptr(Box::new(self.resolve_ty_with(elem, visiting)?)),
        })
    }

    /// Resolve an AST type annotation, validating struct/enum/alias names.
    fn resolve_ty(&mut self, ty: &crate::ast::Type) -> Result<Ty, CompileError> {
        let mut visiting = HashSet::new();
        self.resolve_ty_with(ty, &mut visiting)
    }

    /// Byte size of a resolved type; struct sizes come from the layout table.
    fn size_of(&self, ty: &Ty) -> usize {
        match ty {
            Ty::Struct(name) => self.structs.get(name).map(|s| s.size).unwrap_or(0),
            Ty::Array { elem, len } => self.size_of(elem).saturating_mul(*len),
            Ty::Enum(name) => self
                .enums
                .get(name)
                .map(|e| e.underlying.size_bytes())
                .unwrap_or(4),
            other => other.size_bytes(),
        }
    }

    /// Alignment of a resolved type in bytes.
    fn align_of(&self, ty: &Ty) -> usize {
        match ty {
            Ty::Bool => 1,
            Ty::Struct(name) => self.structs.get(name).map(|s| s.align).unwrap_or(1),
            Ty::Array { elem, .. } => self.align_of(elem),
            Ty::Enum(name) => self
                .enums
                .get(name)
                .map(|e| e.underlying.size_bytes())
                .unwrap_or(4),
            other => other.size_bytes().max(1),
        }
    }

    fn collect_signatures(&mut self, program: &Program) -> Result<(), CompileError> {
        for item in &program.items {
            if let Item::Func(f) = item {
                if builtin_of(&f.name).is_some() {
                    return Err(self.err(
                        f.span,
                        format!("function `{}` collides with a runtime builtin", f.name),
                    ));
                }
                if self.funcs.contains_key(&f.name) {
                    return Err(self.err(f.span, format!("duplicate function `{}`", f.name)));
                }
                let mut params = Vec::with_capacity(f.params.len());
                let mut arg_slots = 0u16;
                for p in &f.params {
                    let ty = self.resolve_ty(&p.ty)?;
                    if matches!(ty, Ty::Struct(_)) {
                        return Err(self.err(
                            f.span,
                            format!(
                                "struct parameters are not supported; pass a pointer `*{ty}` instead"
                            ),
                        ));
                    }
                    arg_slots += 1;
                    params.push(ty);
                }
                if arg_slots > u8::MAX as u16 {
                    return Err(self.err(f.span, "too many parameters"));
                }
                let ret = if matches!(f.ret_ty, crate::ast::Type::Void) {
                    None
                } else {
                    let ty = self.resolve_ty(&f.ret_ty)?;
                    match &ty {
                        Ty::Struct(_) => {
                            return Err(self.err(
                                f.span,
                                format!(
                                    "struct returns are not supported; return a pointer `*{ty}`"
                                ),
                            ));
                        }
                        Ty::Void => {
                            return Err(self.err(f.span, "`void` is only valid as a return type"));
                        }
                        _ => {}
                    }
                    Some(ty)
                };
                self.funcs.insert(
                    f.name.clone(),
                    FuncSig {
                        params,
                        ret,
                        addr: 0,
                    },
                );
            }
        }
        Ok(())
    }

    /// Validate `entry name;` declarations and fall back to `main`.
    fn collect_entry(&mut self, program: &Program) -> Result<(), CompileError> {
        let mut chosen: Option<(String, Span)> = None;
        for item in &program.items {
            if let Item::Entry(e) = item {
                if chosen.is_some() {
                    return Err(self.err(e.span, "duplicate `entry` declaration"));
                }
                let sig = self.funcs.get(&e.name).ok_or_else(|| {
                    self.err(
                        e.span,
                        format!("entry point `{}` is not a defined function", e.name),
                    )
                })?;
                if !sig.params.is_empty() {
                    return Err(self.err(e.span, "the entry function must not take parameters"));
                }
                chosen = Some((e.name.clone(), e.span));
            }
        }
        if chosen.is_none() && self.funcs.contains_key("main") {
            chosen = Some(("main".to_string(), Span::new(0, 0)));
        }
        self.entry = chosen;
        Ok(())
    }

    fn program_entry(&self, _program: &Program) -> Result<(String, Span), CompileError> {
        self.entry.clone().ok_or_else(|| {
            CompileError::new(
                "no entry point: declare `entry name;` or define `main`",
                1,
                1,
            )
        })
    }

    fn resolve_calls(&mut self) -> Result<(), CompileError> {
        let patches = std::mem::take(&mut self.call_patches);
        for (idx, name, span) in patches {
            let addr = self
                .funcs
                .get(&name)
                .map(|s| s.addr)
                .ok_or_else(|| self.err(span, format!("unknown function `{name}`")))?;
            self.code[idx] = Op::Call(addr);
        }
        Ok(())
    }

    /// Folds constant expressions, removes identity operations, collapses redundant
    /// sign-extensions, and resolves constant branches. Jump/call targets are
    /// absolute indices, so they are rewritten through an old->new remap.
    fn peephole(&mut self) {
        let old = std::mem::take(&mut self.code);
        let n = old.len();
        let mut map = vec![0usize; n];
        let mut out: Vec<Op> = Vec::with_capacity(n);
        let mut i = 0;
        while i < n {
            let cur = out.len();
            map[i] = cur;

            // Push(a); Push(b); OP  ->  Push(folded)
            if i + 2 < n
                && let (Op::Push(a), Op::Push(b)) = (old[i], old[i + 1])
                && let Some(v) = Self::fold_binop(old[i + 2], a, b)
            {
                out.push(Op::Push(v));
                map[i + 1] = cur;
                map[i + 2] = cur;
                i += 3;
                continue;
            }

            // Push(c); OP  ->  identity removal / simplification
            if i + 1 < n
                && let Op::Push(c) = old[i]
            {
                match Self::identity_fold(old[i + 1], c) {
                    Identity::Drop => {
                        map[i + 1] = cur;
                        i += 2;
                        continue;
                    }
                    Identity::None => {}
                }
            }

            // SignExt; SignExt  ->  one
            if i + 1 < n && old[i] == old[i + 1] && is_signext(old[i]) {
                out.push(old[i]);
                map[i + 1] = cur;
                i += 2;
                continue;
            }

            out.push(old[i]);
            i += 1;
        }

        // Rewrite absolute jump/call targets through the remap.
        let end = out.len();
        for op in &mut out {
            match op {
                Op::Jmp(t) | Op::Jz(t) | Op::Jnz(t) | Op::Call(t) => {
                    *t = map.get(*t).copied().unwrap_or(end);
                }
                _ => {}
            }
        }
        self.code = out;
    }

    /// Fold `Push(a); Push(b); op` into a single constant, if `op` is a
    /// foldable binary operation. Division/modulo by zero is left to the runtime.
    fn fold_binop(op: Op, a: i64, b: i64) -> Option<i64> {
        match op {
            Op::Add => Some(a.wrapping_add(b)),
            Op::Sub => Some(a.wrapping_sub(b)),
            Op::Mul => Some(a.wrapping_mul(b)),
            Op::Div => {
                if b == 0 {
                    None
                } else {
                    Some(a.wrapping_div(b))
                }
            }
            Op::Mod => {
                if b == 0 {
                    None
                } else {
                    Some(a.wrapping_rem(b))
                }
            }
            Op::And => Some(a & b),
            Op::Or => Some(a | b),
            Op::Xor => Some(a ^ b),
            Op::Eq => Some(i64::from(a == b)),
            Op::Ne => Some(i64::from(a != b)),
            Op::Lt => Some(i64::from(a < b)),
            Op::Le => Some(i64::from(a <= b)),
            Op::Gt => Some(i64::from(a > b)),
            Op::Ge => Some(i64::from(a >= b)),
            Op::LtU => Some(i64::from((a as u64) < (b as u64))),
            Op::LeU => Some(i64::from((a as u64) <= (b as u64))),
            Op::GtU => Some(i64::from((a as u64) > (b as u64))),
            Op::GeU => Some(i64::from((a as u64) >= (b as u64))),
            _ => None,
        }
    }

    fn identity_fold(op: Op, c: i64) -> Identity {
        match op {
            Op::Add | Op::Or | Op::Xor | Op::Sub if c == 0 => Identity::Drop,
            Op::Mul if c == 1 => Identity::Drop,
            Op::And if c == -1 => Identity::Drop,
            _ => Identity::None,
        }
    }

    fn emit(&mut self, op: Op) -> usize {
        self.code.push(op);
        self.code.len() - 1
    }

    fn add_data(&mut self, bytes: Vec<u8>) -> u16 {
        self.data.push(bytes);
        (self.data.len() - 1) as u16
    }

    fn err(&self, span: Span, message: impl Into<String>) -> CompileError {
        let (line, col) = self.lines.locate(span.start);
        CompileError::new(message, line, col)
    }

    fn expect_type(&self, actual: &Ty, expected: &Ty, span: Span) -> Result<(), CompileError> {
        if actual != expected {
            return Err(self.err(span, format!("expected `{expected}`, found `{actual}`")));
        }
        Ok(())
    }

    fn check_literal(&self, value: i64, ty: &Ty, span: Span) -> Result<(), CompileError> {
        if let Some((lo, hi)) = ty.int_range()
            && (value < lo || value > hi)
        {
            return Err(self.err(span, format!("integer literal out of range for `{ty}`")));
        }
        Ok(())
    }

    fn normalize(&mut self, ty: &Ty) {
        match ty {
            Ty::I8 => {
                self.emit(Op::SignExt8);
            }
            Ty::I16 => {
                self.emit(Op::SignExt16);
            }
            Ty::I32 | Ty::Sword => {
                self.emit(Op::SignExt32);
            }
            Ty::U8 => {
                self.emit(Op::Push(0xff));
                self.emit(Op::And);
            }
            Ty::U16 => {
                self.emit(Op::Push(0xffff));
                self.emit(Op::And);
            }
            Ty::U32 | Ty::Word => {
                self.emit(Op::Push(0xffff_ffff));
                self.emit(Op::And);
            }
            Ty::Enum(name) => {
                let signed = self.enums.get(name).is_some_and(|e| e.underlying.signed());
                if signed {
                    self.emit(Op::SignExt32);
                } else {
                    self.emit(Op::Push(0xffff_ffff));
                    self.emit(Op::And);
                }
            }
            _ => {}
        }
    }

    fn emit_cast(
        &mut self,
        inner: &Expr,
        target_ty: &crate::ast::Type,
        span: Span,
    ) -> Result<Ty, CompileError> {
        let target = self.resolve_ty(target_ty)?;
        match &target {
            Ty::Bool | Ty::Void | Ty::Str | Ty::Array { .. } | Ty::Struct(_) => {
                return Err(self.err(span, format!("cannot cast to `{target}`")));
            }
            _ => {}
        }
        let vt = self.emit_expr(inner, None)?;
        match &vt {
            Ty::Bool | Ty::Void | Ty::Str | Ty::Array { .. } | Ty::Struct(_) => {
                return Err(self.err(inner.span(), format!("cannot cast a value of type `{vt}`")));
            }
            _ => {}
        }
        if matches!(
            (&vt, &target),
            (Ty::Ptr(_), Ty::Enum(_)) | (Ty::Enum(_), Ty::Ptr(_))
        ) {
            return Err(self.err(span, "cannot cast between a pointer and an enum"));
        }
        self.normalize(&target);
        Ok(target)
    }

    fn emit_load_width(&mut self, ty: &Ty) {
        match self.size_of(ty) {
            1 => {
                self.emit(Op::Load8);
                if ty.signed() {
                    self.emit(Op::SignExt8);
                }
            }
            2 => {
                self.emit(Op::Load16);
                if ty.signed() {
                    self.emit(Op::SignExt16);
                }
            }
            4 => {
                self.emit(Op::Load32);
                if ty.signed() {
                    self.emit(Op::SignExt32);
                }
            }
            _ => {
                self.emit(Op::Load64);
            }
        }
    }

    fn emit_store_width(&mut self, ty: &Ty) {
        self.emit(match self.size_of(ty) {
            1 => Op::Store8,
            2 => Op::Store16,
            4 => Op::Store32,
            _ => Op::Store64,
        });
    }

    fn emit_trap(&mut self) {
        self.emit(Op::Push(TRAP_EXIT_CODE));
        self.emit(Op::Syscall(SYS_EXIT, 1));
    }

    // frame slots

    fn alloc_slots(&mut self, count: u16) -> u16 {
        let base = self.next_slot;
        self.next_slot += count;
        if self.next_slot > self.frame_slots {
            self.frame_slots = self.next_slot;
        }
        base
    }

    fn alloc_slot(&mut self) -> u16 {
        self.alloc_slots(1)
    }

    /// Reserve a scratch slot, allocated lazily and reused within a function.
    fn temp_slot(&mut self, index: usize) -> u16 {
        while self.temp_slots.len() <= index {
            let slot = self.alloc_slot();
            self.temp_slots.push(slot);
        }
        self.temp_slots[index]
    }

    /// Slots needed to hold `len` elements of `elem` (each slot is 8 bytes).
    fn array_slots(&self, elem: &Ty, len: usize) -> u16 {
        let bytes = self.size_of(elem).max(1) * len;
        bytes.div_ceil(8).max(1) as u16
    }

    fn find_local(&self, name: &str) -> Option<LocalInfo> {
        for scope in self.scopes.iter().rev() {
            if let Some(info) = scope.get(name) {
                return Some(info.clone());
            }
        }
        None
    }

    fn lookup_ty(&self, name: &str) -> Option<Ty> {
        self.find_local(name)
            .map(|l| l.ty)
            .or_else(|| self.globals.get(name).map(|g| g.ty.clone()))
    }

    // globals

    fn declare_global(&mut self, g: &Global) -> Result<(), CompileError> {
        if self.globals.contains_key(&g.name) {
            return Err(self.err(g.span, format!("duplicate global `{}`", g.name)));
        }
        if builtin_of(&g.name).is_some() {
            return Err(self.err(
                g.span,
                format!("global `{}` collides with a runtime builtin", g.name),
            ));
        }
        let ty = self.resolve_ty(&g.ty)?;
        self.emit_global_init(&g.name, ty, g)
    }

    fn emit_global_init(&mut self, name: &str, ty: Ty, g: &Global) -> Result<(), CompileError> {
        if let Ty::Array { elem, len } = &ty {
            let (elem, len) = (elem.clone(), *len);
            let esize = self.size_of(&elem);
            let data = self.add_data(vec![0u8; (esize * len).max(1)]);
            if let Some(value) = &g.value {
                let Expr::Array(elems, _) = value else {
                    return Err(self.err(value.span(), "expected an array literal"));
                };
                if !elems.is_empty() && elems.len() != len {
                    return Err(self.err(
                        value.span(),
                        "array initializer length does not match its type",
                    ));
                }
                for (k, e) in elems.iter().enumerate() {
                    self.emit(Op::PushData(data));
                    let off = (k * esize) as i64;
                    if off != 0 {
                        self.emit(Op::Push(off));
                        self.emit(Op::Add);
                    }
                    self.emit_element_init(e, &elem)?;
                }
            }
            self.globals
                .insert(name.to_string(), GlobalInfo { data, ty });
        } else {
            let data = self.add_data(vec![0u8; self.size_of(&ty).max(1)]);
            if let Some(value) = &g.value {
                self.emit(Op::PushData(data));
                if let (Ty::Struct(_), Expr::AggregateLit { .. }) = (&ty, value) {
                    let base = self.struct_base_temp(0);
                    self.emit(Op::Store(base));
                    self.store_struct_fields(value, &ty, 0)?;
                } else {
                    let vt = self.emit_expr(value, Some(&ty))?;
                    self.expect_type(&vt, &ty, value.span())?;
                    self.normalize(&ty);
                    self.emit_store_width(&ty);
                }
            }
            self.globals
                .insert(name.to_string(), GlobalInfo { data, ty });
        }
        Ok(())
    }

    /// The scratch slot holding the base address of struct storage at a nest depth.
    /// Depths 0/1 are the string-write temporaries; struct work starts at 2.
    fn struct_base_temp(&mut self, depth: usize) -> u16 {
        self.temp_slot(2 + depth)
    }

    /// Store `value` into the array element whose address is on top of the stack.
    fn emit_element_init(&mut self, value: &Expr, ty: &Ty) -> Result<(), CompileError> {
        if let (Ty::Struct(_), Expr::AggregateLit { .. }) = (ty, value) {
            let base = self.struct_base_temp(0);
            self.emit(Op::Store(base));
            self.store_struct_fields(value, ty, 0)?;
            return Ok(());
        }
        let vt = self.emit_expr(value, Some(ty))?;
        self.expect_type(&vt, ty, value.span())?;
        self.normalize(ty);
        self.emit_store_width(ty);
        Ok(())
    }

    /// Write an aggregate literal into storage whose base pointer is in
    /// `temp_slot(2+depth)`.
    fn store_struct_fields(
        &mut self,
        lit: &Expr,
        struct_ty: &Ty,
        depth: usize,
    ) -> Result<(), CompileError> {
        let Ty::Struct(name) = struct_ty else {
            unreachable!("store_struct_fields requires a struct type")
        };
        let def = self
            .structs
            .get(name)
            .cloned()
            .unwrap_or_else(|| StructDef {
                fields: Vec::new(),
                size: 0,
                align: 1,
            });
        let fields = match lit {
            Expr::AggregateLit { fields, .. } => fields,
            _ => {
                return Err(self.err(
                    lit.span(),
                    format!("expected an aggregate initializer of type `{name}`"),
                ));
            }
        };
        let base = self.struct_base_temp(depth);
        let mut seen = HashSet::new();
        for (fname, fexpr, fspan) in fields {
            if !seen.insert(fname.clone()) {
                return Err(self.err(*fspan, format!("duplicate field `{fname}` in initializer")));
            }
            let Some(field) = def.fields.iter().find(|d| &d.name == fname) else {
                return Err(self.err(*fspan, format!("struct `{name}` has no field `{fname}`")));
            };
            let fty = field.ty.clone();
            let off = field.offset as i64;
            self.emit(Op::Load(base));
            if off != 0 {
                self.emit(Op::Push(off));
                self.emit(Op::Add);
            }
            match &fty {
                Ty::Struct(sname) => {
                    if !matches!(fexpr, Expr::AggregateLit { .. }) {
                        return Err(
                            self.err(fexpr.span(), format!("expected a `{sname}` initializer"))
                        );
                    }
                    let next = self.struct_base_temp(depth + 1);
                    self.emit(Op::Store(next));
                    self.store_struct_fields(fexpr, &fty, depth + 1)?;
                }
                Ty::Array { .. } => {
                    return Err(self.err(
                        fexpr.span(),
                        "array fields cannot be initialized in an aggregate initializer",
                    ));
                }
                _ => {
                    let vt = self.emit_expr(fexpr, Some(&fty))?;
                    self.expect_type(&vt, &fty, fexpr.span())?;
                    self.normalize(&fty);
                    self.emit_store_width(&fty);
                }
            }
        }
        Ok(())
    }

    // functions

    fn compile_func(&mut self, f: &Func) -> Result<(), CompileError> {
        self.scopes = vec![HashMap::new()];
        self.next_slot = 0;
        self.frame_slots = 0;
        self.temp_slots.clear();
        self.loops.clear();
        self.cur_ret = if matches!(f.ret_ty, crate::ast::Type::Void) {
            None
        } else {
            Some(self.resolve_ty(&f.ret_ty)?)
        };
        self.argc = f.params.len() as u8;

        for p in &f.params {
            let ty = self.resolve_ty(&p.ty)?;
            let slot = self.alloc_slot();
            self.scopes[0].insert(p.name.clone(), LocalInfo { slot, ty });
        }
        assert_eq!(self.next_slot, self.argc as u16, "arg slots");

        let enter = self.emit(Op::Enter(self.argc, 0));
        if let Some(sig) = self.funcs.get_mut(&f.name) {
            sig.addr = enter;
        }

        self.compile_block(&f.body)?;

        match self.cur_ret.clone() {
            None => {
                self.emit(Op::Ret);
            }
            Some(_) => {
                if !guarantees_return(&f.body) {
                    return Err(self.err(
                        f.span,
                        format!(
                            "function `{}` with a return type must end with `return`",
                            f.name
                        ),
                    ));
                }
            }
        }

        self.code[enter] = Op::Enter(self.argc, self.frame_slots);
        Ok(())
    }

    // statements

    fn compile_block(&mut self, block: &Block) -> Result<(), CompileError> {
        self.scopes.push(HashMap::new());
        for stmt in &block.stmts {
            self.compile_stmt(stmt)?;
        }
        self.scopes.pop();
        Ok(())
    }

    fn compile_stmt(&mut self, stmt: &Stmt) -> Result<(), CompileError> {
        match stmt {
            Stmt::Decl {
                name,
                ty,
                value,
                storage,
                span,
            } => {
                let _ = storage; // stack storage is ordinary frame storage
                self.compile_decl(name, ty, value.as_ref(), *span)
            }
            Stmt::Assign {
                target,
                value,
                span,
            } => self.compile_assign(target, value, *span),
            Stmt::Expr(expr) => {
                let ty = self.emit_expr(expr, None)?;
                if ty != Ty::Void {
                    self.emit(Op::Pop);
                }
                Ok(())
            }
            Stmt::If {
                cond,
                then_branch,
                else_branch,
                ..
            } => self.compile_if(cond, then_branch, else_branch.as_ref()),
            Stmt::While { cond, body, .. } => self.compile_while(cond, body),
            Stmt::For {
                init,
                cond,
                step,
                body,
                ..
            } => self.compile_for(init.as_deref(), cond.as_ref(), step.as_deref(), body),
            Stmt::Break(span) => self.compile_break(*span),
            Stmt::Continue(span) => self.compile_continue(*span),
            Stmt::Return { value, span } => self.compile_return(value.as_ref(), *span),
        }
    }

    fn compile_decl(
        &mut self,
        name: &str,
        declared: &crate::ast::Type,
        value: Option<&Expr>,
        span: Span,
    ) -> Result<(), CompileError> {
        let ty = self.resolve_ty(declared)?;
        match &ty {
            Ty::Array { elem, len } => {
                let (elem, len) = ((**elem).clone(), *len);
                let slot = self.declare_array(name, &elem, len);
                match value {
                    None => self.zero_fill(slot, self.array_slots(&elem, len)),
                    Some(value) => {
                        let Expr::Array(elems, _) = value else {
                            return Err(self.err(value.span(), "expected an array literal"));
                        };
                        if !elems.is_empty() && elems.len() != len {
                            return Err(
                                self.err(span, "array initializer length does not match its type")
                            );
                        }
                        let esize = self.size_of(&elem);
                        for (k, e) in elems.iter().enumerate() {
                            self.emit(Op::Lea(slot));
                            let off = (k * esize) as i64;
                            if off != 0 {
                                self.emit(Op::Push(off));
                                self.emit(Op::Add);
                            }
                            self.emit_element_init(e, &elem)?;
                        }
                    }
                }
                Ok(())
            }
            Ty::Struct(sname) => self.declare_local_struct(name, sname, value),
            _ => self.declare_local_value(name, ty, value),
        }
    }

    /// Store zero into `slots` consecutive frame slots starting at `slot`.
    fn zero_fill(&mut self, slot: u16, slots: u16) {
        for k in 0..slots {
            self.emit(Op::Push(0));
            self.emit(Op::Store(slot + k));
        }
    }

    fn declare_local_struct(
        &mut self,
        name: &str,
        sname: &str,
        value: Option<&Expr>,
    ) -> Result<(), CompileError> {
        let ty = Ty::Struct(sname.to_string());
        let bytes = self.size_of(&ty);
        let slots = bytes.div_ceil(8).max(1) as u16;
        let slot = self.alloc_slots(slots);
        match value {
            None => self.zero_fill(slot, slots),
            Some(value) => {
                self.emit(Op::Lea(slot));
                let base = self.struct_base_temp(0);
                self.emit(Op::Store(base));
                self.store_struct_fields(value, &ty, 0)?;
            }
        }
        self.scopes
            .last_mut()
            .expect("scope")
            .insert(name.to_string(), LocalInfo { slot, ty });
        Ok(())
    }

    fn declare_local_value(
        &mut self,
        name: &str,
        ty: Ty,
        value: Option<&Expr>,
    ) -> Result<(), CompileError> {
        let slot = self.alloc_slot();
        match value {
            None => self.zero_fill(slot, 1),
            Some(value) => {
                let vt = self.emit_expr(value, Some(&ty))?;
                self.expect_type(&vt, &ty, value.span())?;
                self.normalize(&ty);
                self.emit(Op::Store(slot));
            }
        }
        self.scopes
            .last_mut()
            .expect("scope")
            .insert(name.to_string(), LocalInfo { slot, ty });
        Ok(())
    }

    fn declare_array(&mut self, name: &str, elem: &Ty, len: usize) -> u16 {
        let slot = self.alloc_slots(self.array_slots(elem, len));
        self.scopes.last_mut().expect("scope").insert(
            name.to_string(),
            LocalInfo {
                slot,
                ty: Ty::Array {
                    elem: Box::new(elem.clone()),
                    len,
                },
            },
        );
        slot
    }

    fn compile_assign(
        &mut self,
        target: &Expr,
        value: &Expr,
        span: Span,
    ) -> Result<(), CompileError> {
        match target {
            Expr::Ident(name, tspan) => {
                if let Some(local) = self.find_local(name) {
                    match &local.ty {
                        Ty::Array { .. } => {
                            return Err(self.err(*tspan, "cannot assign to an array as a whole"));
                        }
                        Ty::Struct(_) => {
                            return Err(self.err(*tspan, "cannot assign to a struct as a whole"));
                        }
                        _ => {}
                    }
                    let vt = self.emit_expr(value, Some(&local.ty))?;
                    self.expect_type(&vt, &local.ty, value.span())?;
                    self.normalize(&local.ty);
                    self.emit(Op::Store(local.slot));
                    return Ok(());
                }
                if let Some(global) = self.globals.get(name).cloned() {
                    match &global.ty {
                        Ty::Array { .. } => {
                            return Err(self.err(*tspan, "cannot assign to an array as a whole"));
                        }
                        Ty::Struct(_) => {
                            return Err(self.err(*tspan, "cannot assign to a struct as a whole"));
                        }
                        _ => {}
                    }
                    self.emit(Op::PushData(global.data));
                    let vt = self.emit_expr(value, Some(&global.ty))?;
                    self.expect_type(&vt, &global.ty, value.span())?;
                    self.normalize(&global.ty);
                    self.emit_store_width(&global.ty);
                    return Ok(());
                }
                if self.variant_values.contains_key(name) {
                    return Err(self.err(
                        *tspan,
                        format!("`{name}` is an enum variant, not a variable"),
                    ));
                }
                Err(self.err(*tspan, format!("unknown variable `{name}`")))
            }
            Expr::Field {
                base,
                name: fname,
                span: fspan,
            } => {
                let (off, fty) = self.field_layout(base, fname, *fspan)?;
                match &fty {
                    Ty::Struct(_) => {
                        return Err(self.err(*fspan, "cannot assign to a struct field as a whole"));
                    }
                    Ty::Array { .. } => {
                        return Err(self.err(
                            *fspan,
                            format!("cannot assign to field `{fname}` as a whole"),
                        ));
                    }
                    _ => {}
                }
                self.emit_base_address(base, *fspan)?;
                self.emit(Op::Push(off as i64));
                self.emit(Op::Add);
                let vt = self.emit_expr(value, Some(&fty))?;
                self.expect_type(&vt, &fty, value.span())?;
                self.normalize(&fty);
                self.emit_store_width(&fty);
                Ok(())
            }
            Expr::Unary {
                op: UnOp::Deref,
                expr: inner,
                span: dspan,
            } => {
                let vt = self.emit_expr(inner, None)?;
                let Ty::Ptr(pointee) = &vt else {
                    return Err(self.err(*dspan, "cannot write through a non-pointer"));
                };
                match &**pointee {
                    Ty::Struct(_) => {
                        return Err(self.err(*dspan, "cannot assign to a dereferenced struct"));
                    }
                    Ty::Array { .. } => {
                        return Err(self.err(*dspan, "cannot assign to a dereferenced array"));
                    }
                    _ => {}
                }
                let vt2 = self.emit_expr(value, Some(pointee))?;
                self.expect_type(&vt2, pointee, value.span())?;
                self.normalize(pointee);
                self.emit_store_width(pointee);
                Ok(())
            }
            Expr::Index { base, index, .. } => self.compile_index_store(base, index, value, span),
            other => Err(self.err(other.span(), "invalid assignment target")),
        }
    }

    fn compile_if(
        &mut self,
        cond: &Expr,
        then_branch: &Block,
        else_branch: Option<&Block>,
    ) -> Result<(), CompileError> {
        let ct = self.emit_expr(cond, Some(&Ty::Bool))?;
        self.expect_type(&ct, &Ty::Bool, cond.span())?;
        let jz = self.emit(Op::Jz(0));
        self.compile_block(then_branch)?;
        if let Some(else_branch) = else_branch {
            let jmp = self.emit(Op::Jmp(0));
            let here = self.code.len();
            self.code[jz] = Op::Jz(here);
            self.compile_block(else_branch)?;
            let end = self.code.len();
            self.code[jmp] = Op::Jmp(end);
        } else {
            let here = self.code.len();
            self.code[jz] = Op::Jz(here);
        }
        Ok(())
    }

    fn compile_while(&mut self, cond: &Expr, body: &Block) -> Result<(), CompileError> {
        let start = self.code.len();
        let ct = self.emit_expr(cond, Some(&Ty::Bool))?;
        self.expect_type(&ct, &Ty::Bool, cond.span())?;
        let jz = self.emit(Op::Jz(0));
        self.loops.push(LoopCtx {
            breaks: Vec::new(),
            continues: Vec::new(),
        });
        self.compile_block(body)?;
        let ctx = self.loops.pop().expect("loop context");
        self.emit(Op::Jmp(start));
        let here = self.code.len();
        self.code[jz] = Op::Jz(here);
        for site in ctx.breaks {
            self.code[site] = Op::Jmp(here);
        }
        for site in ctx.continues {
            self.code[site] = Op::Jmp(start);
        }
        Ok(())
    }

    fn compile_for(
        &mut self,
        init: Option<&Stmt>,
        cond: Option<&Expr>,
        step: Option<&Stmt>,
        body: &Block,
    ) -> Result<(), CompileError> {
        self.scopes.push(HashMap::new());
        if let Some(init) = init {
            self.compile_stmt(init)?;
        }
        let cond_start = self.code.len();
        let jz = match cond {
            Some(cond) => {
                let ct = self.emit_expr(cond, Some(&Ty::Bool))?;
                self.expect_type(&ct, &Ty::Bool, cond.span())?;
                Some(self.emit(Op::Jz(0)))
            }
            None => None,
        };
        self.loops.push(LoopCtx {
            breaks: Vec::new(),
            continues: Vec::new(),
        });
        self.compile_block(body)?;
        let ctx = self.loops.pop().expect("loop context");
        let step_start = self.code.len();
        if let Some(step) = step {
            self.compile_stmt(step)?;
        }
        self.emit(Op::Jmp(cond_start));
        let here = self.code.len();
        if let Some(jz) = jz {
            self.code[jz] = Op::Jz(here);
        }
        for site in ctx.breaks {
            self.code[site] = Op::Jmp(here);
        }
        let continue_target = if step.is_some() {
            step_start
        } else {
            cond_start
        };
        for site in ctx.continues {
            self.code[site] = Op::Jmp(continue_target);
        }
        self.scopes.pop();
        Ok(())
    }

    fn compile_break(&mut self, span: Span) -> Result<(), CompileError> {
        if self.loops.is_empty() {
            return Err(self.err(span, "`break` outside of a loop"));
        }
        let site = self.emit(Op::Jmp(0));
        self.loops
            .last_mut()
            .expect("loop context")
            .breaks
            .push(site);
        Ok(())
    }

    fn compile_continue(&mut self, span: Span) -> Result<(), CompileError> {
        if self.loops.is_empty() {
            return Err(self.err(span, "`continue` outside of a loop"));
        }
        let site = self.emit(Op::Jmp(0));
        self.loops
            .last_mut()
            .expect("loop context")
            .continues
            .push(site);
        Ok(())
    }

    fn compile_return(&mut self, value: Option<&Expr>, span: Span) -> Result<(), CompileError> {
        match (self.cur_ret.clone(), value) {
            (Some(ty), Some(expr)) => {
                let vt = self.emit_expr(expr, Some(&ty))?;
                self.expect_type(&vt, &ty, expr.span())?;
                self.normalize(&ty);
                self.emit(Op::Ret);
            }
            (Some(_), None) => return Err(self.err(span, "expected a return value")),
            (None, Some(expr)) => return Err(self.err(expr.span(), "unexpected return value")),
            (None, None) => {
                self.emit(Op::Ret);
            }
        }
        Ok(())
    }

    // expressions

    fn expr_hint(&mut self, expr: &Expr) -> Option<Ty> {
        match expr {
            Expr::Int(..) => None,
            Expr::Bool(..) => Some(Ty::Bool),
            Expr::Str(..) => Some(Ty::Str),
            Expr::Null(_) => None,
            Expr::Ident(name, _) => self.lookup_ty(name).or_else(|| {
                self.variant_values
                    .get(name)
                    .map(|(e, _)| Ty::Enum(e.clone()))
            }),
            Expr::Array(..) => None,
            Expr::AggregateLit { .. } => None,
            Expr::Unary { op, expr, .. } => match op {
                UnOp::Not => Some(Ty::Bool),
                UnOp::Neg | UnOp::BitNot => self.expr_hint(expr),
                UnOp::AddrOf => None,
                UnOp::Deref => match self.expr_hint(expr)? {
                    Ty::Ptr(ptr_to) => Some(*ptr_to),
                    _ => None,
                },
            },
            Expr::Binary { op, lhs, rhs, .. } => match op {
                BinOp::AndAnd
                | BinOp::OrOr
                | BinOp::Eq
                | BinOp::Ne
                | BinOp::Lt
                | BinOp::Le
                | BinOp::Gt
                | BinOp::Ge => Some(Ty::Bool),
                BinOp::Add | BinOp::Sub => self.expr_hint(lhs).or_else(|| self.expr_hint(rhs)),
                _ => self.expr_hint(lhs).or_else(|| self.expr_hint(rhs)),
            },
            Expr::Call { callee, .. } => match &**callee {
                Expr::Ident(name, _) => match builtin_of(name) {
                    Some(b) => b.ret(),
                    None => self.funcs.get(name).and_then(|s| s.ret.clone()),
                },
                _ => None,
            },
            Expr::Cast { ty, .. } => self.resolve_ty(ty).ok(),
            Expr::Load { ty, .. } => self.resolve_ty(ty).ok(),
            Expr::Store { .. } => Some(Ty::Void),
            Expr::SizeOf { .. } => Some(Ty::Word),
            Expr::OffsetOf { .. } => Some(Ty::Word),
            Expr::Index { base, .. } => self.collection_elem(base),
            Expr::Field { base, name, .. } => self.field_static(base, name),
        }
    }

    /// The static element type of an array-indexable expression.
    fn collection_elem(&mut self, base: &Expr) -> Option<Ty> {
        match base {
            Expr::Ident(name, _) => match self.lookup_ty(name)? {
                Ty::Array { elem, .. } => Some(*elem),
                _ => None,
            },
            Expr::Unary {
                op: UnOp::Deref,
                expr,
                ..
            } => match self.expr_hint(expr)? {
                Ty::Ptr(ptr_to) => match *ptr_to {
                    Ty::Array { elem, .. } => Some(*elem),
                    _ => None,
                },
                _ => None,
            },
            Expr::Field { base, name, .. } => match self.field_static(base, name)? {
                Ty::Array { elem, .. } => Some(*elem),
                _ => None,
            },
            _ => None,
        }
    }

    /// The static type of a place (`Ident`, `Field`, `Index`, deref).
    fn place_ty_static(&mut self, expr: &Expr) -> Option<Ty> {
        match expr {
            Expr::Ident(name, _) => self.lookup_ty(name).or_else(|| {
                self.variant_values
                    .get(name)
                    .map(|(e, _)| Ty::Enum(e.clone()))
            }),
            Expr::Unary {
                op: UnOp::Deref,
                expr,
                ..
            } => self.expr_hint(expr).and_then(|t| match t {
                Ty::Ptr(ptr_to) => Some(*ptr_to),
                _ => None,
            }),
            Expr::Index { base, .. } => self.collection_elem(base),
            Expr::Field { base, name, .. } => self.field_static(base, name),
            _ => None,
        }
    }

    /// The static type of a struct field, following pointer auto-deref.
    fn field_static(&mut self, base: &Expr, name: &str) -> Option<Ty> {
        let struct_name = match self.place_ty_static(base)? {
            Ty::Struct(n) => n,
            Ty::Ptr(ptr_to) => match *ptr_to {
                Ty::Struct(n) => n,
                _ => return None,
            },
            _ => return None,
        };
        let def = self.structs.get(&struct_name)?;
        def.fields
            .iter()
            .find(|d| d.name == name)
            .map(|d| d.ty.clone())
    }

    /// Field offset and type, with errors for non-struct bases and unknown fields.
    fn field_layout(
        &mut self,
        base: &Expr,
        name: &str,
        span: Span,
    ) -> Result<(usize, Ty), CompileError> {
        let struct_name = match self.place_ty_static(base) {
            Some(Ty::Struct(n)) => n,
            Some(Ty::Ptr(ptr_to)) => match *ptr_to {
                Ty::Struct(n) => n,
                _ => return Err(self.err(span, "cannot access a field of a non-struct pointer")),
            },
            Some(other) => {
                return Err(self.err(span, format!("cannot access a field of `{other}`")));
            }
            None => return Err(self.err(span, "cannot access a field of this expression")),
        };
        let def = self.structs.get(&struct_name).expect("struct resolved");
        let field = def.fields.iter().find(|d| d.name == name).ok_or_else(|| {
            self.err(
                span,
                format!("struct `{struct_name}` has no field `{name}`"),
            )
        })?;
        Ok((field.offset, field.ty.clone()))
    }

    fn emit_expr(&mut self, expr: &Expr, expected: Option<&Ty>) -> Result<Ty, CompileError> {
        match expr {
            Expr::Int(value, span) => {
                let ty = expected.filter(|t| t.is_int()).cloned().unwrap_or(Ty::I32);
                self.check_literal(*value, &ty, *span)?;
                self.emit(Op::Push(*value));
                Ok(ty)
            }
            Expr::Bool(value, _) => {
                self.emit(Op::Push(i64::from(*value)));
                Ok(Ty::Bool)
            }
            Expr::Str(bytes, _) => {
                let mut blob = Vec::with_capacity(bytes.len() + 5);
                blob.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
                blob.extend_from_slice(bytes);
                blob.push(0);
                let idx = self.add_data(blob);
                self.emit(Op::PushData(idx));
                Ok(Ty::Str)
            }
            Expr::Null(_) => {
                self.emit(Op::Push(0));
                match expected {
                    Some(Ty::Ptr(inner)) => Ok(Ty::Ptr(inner.clone())),
                    _ => Ok(Ty::Word),
                }
            }
            Expr::Ident(name, span) => self.emit_ident(name, *span),
            Expr::Unary { op, expr, span } => self.emit_unary(*op, expr, *span, expected),
            Expr::Binary { op, lhs, rhs, span } => self.emit_binary(*op, lhs, rhs, *span, expected),
            Expr::Call { callee, args, span } => self.emit_call(callee, args, *span),
            Expr::Index { base, index, span } => self.emit_index_load(base, index, *span),
            Expr::Field { base, name, span } => self.emit_field_value(base, name, *span),
            Expr::AggregateLit { span, .. } => Err(self.err(
                *span,
                "an aggregate initializer is only allowed as a declaration initializer",
            )),
            Expr::Cast {
                expr: inner,
                ty,
                span,
            } => self.emit_cast(inner, ty, *span),
            Expr::Load { ty, addr, span } => self.emit_intrinsic_load(ty, addr, *span),
            Expr::Store {
                ty,
                addr,
                value,
                span,
            } => self.emit_intrinsic_store(ty, addr, value, *span),
            Expr::SizeOf { ty, align, span } => {
                let ty = self.resolve_ty(ty)?;
                if matches!(ty, Ty::Void | Ty::Str) {
                    return Err(self.err(*span, format!("`{ty}` has no size")));
                }
                let n = if *align {
                    self.align_of(&ty)
                } else {
                    self.size_of(&ty)
                };
                self.emit(Op::Push(n as i64));
                Ok(Ty::Word)
            }
            Expr::OffsetOf { ty, field, span } => {
                let ty = self.resolve_ty(ty)?;
                let Ty::Struct(name) = &ty else {
                    return Err(self.err(*span, "`offsetof` requires a struct type"));
                };
                let def = self.structs.get(name).expect("struct resolved");
                let Some(f) = def.fields.iter().find(|d| d.name == *field) else {
                    return Err(self.err(*span, format!("struct `{name}` has no field `{field}`")));
                };
                self.emit(Op::Push(f.offset as i64));
                Ok(Ty::Word)
            }
            Expr::Array(_, span) => Err(self.err(
                *span,
                "an array literal is only allowed as a declaration initializer",
            )),
        }
    }

    fn emit_ident(&mut self, name: &str, span: Span) -> Result<Ty, CompileError> {
        if let Some(local) = self.find_local(name) {
            return self.emit_ident_value(local, name, span);
        }
        if let Some(global) = self.globals.get(name).cloned() {
            return self.emit_global_value(global, name, span);
        }
        if self.funcs.contains_key(name) {
            return Err(self.err(span, format!("`{name}` is a function; call it with `()`")));
        }
        if let Some((enum_name, value)) = self.variant_values.get(name).cloned() {
            self.emit(Op::Push(value));
            return Ok(Ty::Enum(enum_name));
        }
        Err(self.err(span, format!("unknown variable `{name}`")))
    }

    fn emit_ident_value(
        &mut self,
        local: LocalInfo,
        name: &str,
        span: Span,
    ) -> Result<Ty, CompileError> {
        match &local.ty {
            Ty::Array { .. } => {
                Err(self.err(span, format!("array `{name}` cannot be used as a value")))
            }
            Ty::Struct(_) => Err(self.err(
                span,
                format!(
                    "struct `{name}` cannot be used as a value; access a field or take its address"
                ),
            )),
            _ => {
                self.emit(Op::Load(local.slot));
                Ok(local.ty)
            }
        }
    }

    fn emit_global_value(
        &mut self,
        global: GlobalInfo,
        name: &str,
        span: Span,
    ) -> Result<Ty, CompileError> {
        match &global.ty {
            Ty::Struct(_) => Err(self.err(
                span,
                format!(
                    "struct `{name}` cannot be used as a value; access a field or take its address"
                ),
            )),
            other => {
                self.emit(Op::PushData(global.data));
                self.emit_load_width(other);
                Ok(global.ty)
            }
        }
    }

    fn emit_unary(
        &mut self,
        op: UnOp,
        expr: &Expr,
        span: Span,
        expected: Option<&Ty>,
    ) -> Result<Ty, CompileError> {
        match op {
            UnOp::Not => {
                let vt = self.emit_expr(expr, Some(&Ty::Bool))?;
                self.expect_type(&vt, &Ty::Bool, expr.span())?;
                self.emit(Op::Not);
                Ok(Ty::Bool)
            }
            UnOp::Neg => {
                let ty = self
                    .expr_hint(expr)
                    .or_else(|| expected.filter(|t| t.is_int()).cloned())
                    .unwrap_or(Ty::I32);
                if let Expr::Int(value, ispan) = expr
                    && !ty.signed()
                    && *value > 0
                {
                    return Err(self.err(*ispan, "negative value for an unsigned type"));
                }
                let vt = self.emit_expr(expr, Some(&ty))?;
                self.expect_type(&vt, &ty, expr.span())?;
                self.emit(Op::Neg);
                self.normalize(&ty);
                Ok(ty)
            }
            UnOp::BitNot => {
                let ty = self
                    .expr_hint(expr)
                    .or_else(|| expected.filter(|t| t.is_int()).cloned())
                    .unwrap_or(Ty::I32);
                let vt = self.emit_expr(expr, Some(&ty))?;
                self.expect_type(&vt, &ty, expr.span())?;
                self.emit(Op::BitNot);
                self.normalize(&ty);
                Ok(ty)
            }
            UnOp::AddrOf => self.emit_addr_of(expr, span),
            UnOp::Deref => self.emit_deref(expr, span),
        }
    }

    /// `*inner`: the value behind a raw pointer.
    fn emit_deref(&mut self, inner: &Expr, span: Span) -> Result<Ty, CompileError> {
        let vt = self.emit_expr(inner, None)?;
        let Ty::Ptr(ptr_to) = &vt else {
            return Err(self.err(span, "cannot dereference a non-pointer"));
        };
        match &**ptr_to {
            Ty::Struct(_) => Ok((**ptr_to).clone()),
            Ty::Array { .. } => Err(self.err(span, "cannot use a dereferenced array as a value")),
            scalar => {
                self.emit_load_width(scalar);
                Ok(scalar.clone())
            }
        }
    }

    /// `&expr` / `addr_of(expr)`: a raw pointer to a place. Arrays decay to a
    /// pointer to their first element.
    fn emit_addr_of(&mut self, expr: &Expr, span: Span) -> Result<Ty, CompileError> {
        let ty = self.emit_place_addr(expr, span)?;
        match &ty {
            Ty::Array { elem, .. } => Ok(Ty::Ptr(elem.clone())),
            _ => Ok(Ty::Ptr(Box::new(ty))),
        }
    }

    /// Emit the address of a place, returning the type stored there. A dereferenced
    /// struct (by-reference) leaves its pointer on the stack unchanged.
    fn emit_place_addr(&mut self, expr: &Expr, span: Span) -> Result<Ty, CompileError> {
        match expr {
            Expr::Ident(name, ispan) => {
                if let Some(local) = self.find_local(name) {
                    self.emit(Op::Lea(local.slot));
                    return Ok(local.ty);
                }
                if let Some(global) = self.globals.get(name).cloned() {
                    self.emit(Op::PushData(global.data));
                    return Ok(global.ty);
                }
                Err(self.err(*ispan, format!("unknown variable `{name}`")))
            }
            Expr::Unary {
                op: UnOp::Deref,
                expr: inner,
                span: dspan,
            } => {
                let vt = self.emit_expr(inner, None)?;
                let Ty::Ptr(ptr_to) = &vt else {
                    return Err(self.err(*dspan, "cannot dereference a non-pointer"));
                };
                if matches!(**ptr_to, Ty::Array { .. }) {
                    return Err(self.err(*dspan, "cannot use an array behind a pointer as a value"));
                }
                Ok((**ptr_to).clone())
            }
            Expr::Index { base, index, .. } => self.emit_element_addr(base, index, span),
            Expr::Field { base, name, .. } => {
                let (off, fty) = self.field_layout(base, name, span)?;
                self.emit_base_address(base, span)?;
                self.emit(Op::Push(off as i64));
                self.emit(Op::Add);
                Ok(fty)
            }
            other => Err(self.err(other.span(), "expression is not a place")),
        }
    }

    /// The address of a field base: the pointer value for pointer bases,
    /// otherwise the address of the base place.
    fn emit_base_address(&mut self, base: &Expr, span: Span) -> Result<(), CompileError> {
        match self.place_ty_static(base) {
            Some(Ty::Ptr(_)) => {
                self.emit_expr(base, None)?;
                Ok(())
            }
            _ => {
                self.emit_place_addr(base, span)?;
                Ok(())
            }
        }
    }

    /// `base.name` as a value (or a by-reference struct place).
    fn emit_field_value(
        &mut self,
        base: &Expr,
        name: &str,
        span: Span,
    ) -> Result<Ty, CompileError> {
        let (off, fty) = self.field_layout(base, name, span)?;
        self.emit_base_address(base, span)?;
        self.emit(Op::Push(off as i64));
        self.emit(Op::Add);
        match &fty {
            Ty::Struct(_) => Ok(fty),
            Ty::Array { .. } => Err(self.err(
                span,
                format!("field `{name}` cannot be used as a value; index it or take its address"),
            )),
            _ => {
                self.emit_load_width(&fty);
                Ok(fty)
            }
        }
    }

    fn emit_array_base(&mut self, arr: &ArrRef) -> Result<(), CompileError> {
        match arr {
            ArrRef::Local { slot, .. } => {
                self.emit(Op::Lea(*slot));
            }
            ArrRef::Global { data, .. } => {
                self.emit(Op::PushData(*data));
            }
            ArrRef::DerefBase { inner, .. } => {
                self.emit_expr(inner, None)?;
            }
            ArrRef::FieldBase {
                base, name, span, ..
            } => {
                let (off, _) = self.field_layout(base, name, *span)?;
                self.emit_base_address(base, *span)?;
                self.emit(Op::Push(off as i64));
                self.emit(Op::Add);
            }
        }
        Ok(())
    }

    /// Given an integer index on the stack, leave the element address there,
    /// trapping on an out-of-range index. Emits a forward jump over the trap.
    fn emit_checked_element_addr(
        &mut self,
        arr: &ArrRef,
        elem: &Ty,
        len: usize,
        const_index: Option<i64>,
    ) -> Result<(), CompileError> {
        if let Some(k) = const_index
            && k >= 0
            && (k as usize) < len
        {
            self.emit(Op::Push(self.size_of(elem).max(1) as i64));
            self.emit(Op::Mul);
            self.emit_array_base(arr)?;
            self.emit(Op::Add);
            return Ok(());
        }
        self.emit(Op::Dup);
        self.emit(Op::Push(len as i64));
        self.emit(Op::LtU);
        let jz = self.emit(Op::Jz(0));
        self.emit(Op::Push(self.size_of(elem).max(1) as i64));
        self.emit(Op::Mul);
        self.emit_array_base(arr)?;
        self.emit(Op::Add);
        let over = self.emit(Op::Jmp(0));
        let trap = self.code.len();
        self.code[jz] = Op::Jz(trap);
        self.emit(Op::Pop);
        self.emit_trap();
        let end = self.code.len();
        self.code[over] = Op::Jmp(end);
        Ok(())
    }

    fn emit_binary(
        &mut self,
        op: BinOp,
        lhs: &Expr,
        rhs: &Expr,
        span: Span,
        expected: Option<&Ty>,
    ) -> Result<Ty, CompileError> {
        match op {
            BinOp::AndAnd | BinOp::OrOr => self.emit_logic(op == BinOp::AndAnd, lhs, rhs, span),
            BinOp::Shl | BinOp::Shr => {
                let ty = self
                    .expr_hint(lhs)
                    .or_else(|| expected.filter(|t| t.is_int()).cloned())
                    .unwrap_or(Ty::I32);
                let a = self.emit_expr(lhs, Some(&ty))?;
                self.expect_type(&a, &ty, span)?;
                let b = self.emit_expr(rhs, None)?;
                if !b.is_int() {
                    return Err(self.err(rhs.span(), "shift amount must be an integer"));
                }
                let opcode = if op == BinOp::Shl {
                    Op::Shl
                } else if ty.signed() {
                    Op::Sar
                } else {
                    Op::ShrU
                };
                self.emit(opcode);
                self.normalize(&ty);
                Ok(ty)
            }
            BinOp::Add | BinOp::Sub => {
                if let Some(ty) = self.try_pointer_arith(op, lhs, rhs, expected)? {
                    return Ok(ty);
                }
                self.emit_binary_arith(op, lhs, rhs, span, expected)
            }
            _ => self.emit_binary_arith(op, lhs, rhs, span, expected),
        }
    }

    /// Typed pointer arithmetic (spec §6.1): `ptr + index` advances by
    /// `index * sizeof(*ptr)` bytes. Returns `None` when neither side is a
    /// pointer.
    fn try_pointer_arith(
        &mut self,
        op: BinOp,
        lhs: &Expr,
        rhs: &Expr,
        _expected: Option<&Ty>,
    ) -> Result<Option<Ty>, CompileError> {
        let lhs_ty = self.expr_hint(lhs);
        let rhs_ty = self.expr_hint(rhs);
        let (ptr_side, int_side, swap) = match (&lhs_ty, &rhs_ty) {
            (Some(Ty::Ptr(_)), t) if !matches!(t, Some(Ty::Ptr(_))) => (lhs, rhs, false),
            (t, Some(Ty::Ptr(_))) if !matches!(t, Some(Ty::Ptr(_))) => (rhs, lhs, true),
            _ => return Ok(None),
        };
        let Some(Ty::Ptr(pointee)) = self.expr_hint(ptr_side) else {
            return Ok(None);
        };
        // Only `ptr + int` (add) and `ptr - int` (sub); `int + ptr` for add.
        match op {
            BinOp::Add => {}
            BinOp::Sub if !swap => {}
            _ => return Ok(None),
        }
        let pt = self.emit_expr(ptr_side, None)?;
        let expect_ptr = Ty::Ptr(pointee.clone());
        self.expect_type(&pt, &expect_ptr, ptr_side.span())?;
        let it = self.emit_expr(int_side, None)?;
        if !it.is_int() {
            return Err(self.err(int_side.span(), "pointer offset must be an integer"));
        }
        self.emit(Op::Push(self.size_of(&pointee).max(1) as i64));
        self.emit(Op::Mul);
        self.emit(if op == BinOp::Add { Op::Add } else { Op::Sub });
        Ok(Some(Ty::Ptr(pointee)))
    }

    fn emit_binary_arith(
        &mut self,
        op: BinOp,
        lhs: &Expr,
        rhs: &Expr,
        span: Span,
        expected: Option<&Ty>,
    ) -> Result<Ty, CompileError> {
        let ty = self
            .expr_hint(lhs)
            .or_else(|| self.expr_hint(rhs))
            .or_else(|| {
                expected
                    .filter(|t| t.is_int() || matches!(t, Ty::Ptr(_)))
                    .cloned()
            })
            .unwrap_or(Ty::I32);
        let a = self.emit_expr(lhs, Some(&ty))?;
        self.expect_type(&a, &ty, span)?;
        let b = self.emit_expr(rhs, Some(&ty))?;
        self.expect_type(&b, &ty, span)?;
        match op {
            BinOp::Add => {
                self.emit(Op::Add);
                self.normalize(&ty);
                Ok(ty)
            }
            BinOp::Sub => {
                self.emit(Op::Sub);
                self.normalize(&ty);
                Ok(ty)
            }
            BinOp::Mul => {
                self.emit(Op::Mul);
                self.normalize(&ty);
                Ok(ty)
            }
            BinOp::Div => {
                self.emit(Op::Div);
                self.normalize(&ty);
                Ok(ty)
            }
            BinOp::Mod => {
                self.emit(Op::Mod);
                self.normalize(&ty);
                Ok(ty)
            }
            BinOp::And => {
                self.emit(Op::And);
                Ok(ty)
            }
            BinOp::Or => {
                self.emit(Op::Or);
                Ok(ty)
            }
            BinOp::Xor => {
                self.emit(Op::Xor);
                Ok(ty)
            }
            BinOp::Eq => {
                self.emit(Op::Eq);
                Ok(Ty::Bool)
            }
            BinOp::Ne => {
                self.emit(Op::Ne);
                Ok(Ty::Bool)
            }
            BinOp::Lt => self.emit_compare(Op::Lt, Op::LtU, &ty),
            BinOp::Le => self.emit_compare(Op::Le, Op::LeU, &ty),
            BinOp::Gt => self.emit_compare(Op::Gt, Op::GtU, &ty),
            BinOp::Ge => self.emit_compare(Op::Ge, Op::GeU, &ty),
            BinOp::AndAnd | BinOp::OrOr | BinOp::Shl | BinOp::Shr => unreachable!(),
        }
    }

    fn emit_compare(&mut self, signed: Op, unsigned: Op, ty: &Ty) -> Result<Ty, CompileError> {
        self.emit(if ty.signed() { signed } else { unsigned });
        Ok(Ty::Bool)
    }

    fn emit_logic(
        &mut self,
        is_and: bool,
        lhs: &Expr,
        rhs: &Expr,
        span: Span,
    ) -> Result<Ty, CompileError> {
        let lt = self.emit_expr(lhs, Some(&Ty::Bool))?;
        self.expect_type(&lt, &Ty::Bool, span)?;
        let jump = self.emit(if is_and { Op::Jz(0) } else { Op::Jnz(0) });
        let rt = self.emit_expr(rhs, Some(&Ty::Bool))?;
        self.expect_type(&rt, &Ty::Bool, span)?;
        let over = self.emit(Op::Jmp(0));
        let short = self.code.len();
        self.code[jump] = if is_and {
            Op::Jz(short)
        } else {
            Op::Jnz(short)
        };
        self.emit(Op::Push(i64::from(!is_and)));
        let end = self.code.len();
        self.code[over] = Op::Jmp(end);
        Ok(Ty::Bool)
    }

    fn emit_call(&mut self, callee: &Expr, args: &[Expr], span: Span) -> Result<Ty, CompileError> {
        let Expr::Ident(name, _) = callee else {
            return Err(self.err(span, "only direct function calls are supported"));
        };
        if let Some(builtin) = builtin_of(name) {
            return self.emit_builtin(builtin, args, span);
        }
        if let Some((enum_name, _)) = self.variant_values.get(name).cloned() {
            return Err(self.err(
                span,
                format!("`{name}` is a variant of enum `{enum_name}`, not a function"),
            ));
        }
        let sig = self
            .funcs
            .get(name)
            .cloned()
            .ok_or_else(|| self.err(span, format!("unknown function `{name}`")))?;
        if args.len() != sig.params.len() {
            return Err(self.err(
                span,
                format!(
                    "`{name}` expects {} argument(s), found {}",
                    sig.params.len(),
                    args.len()
                ),
            ));
        }
        for (arg, pty) in args.iter().zip(sig.params.iter()) {
            let at = self.emit_expr(arg, Some(pty))?;
            self.expect_type(&at, pty, arg.span())?;
            self.normalize(pty);
        }
        let at = self.emit(Op::Call(0));
        self.call_patches.push((at, name.clone(), span));
        Ok(sig.ret.unwrap_or(Ty::Void))
    }

    fn emit_builtin(
        &mut self,
        builtin: Builtin,
        args: &[Expr],
        span: Span,
    ) -> Result<Ty, CompileError> {
        if let Builtin::Syscall = builtin {
            return self.emit_syscall_intrinsic(args, span);
        }
        let params = builtin.params();
        if args.len() != params.len() {
            return Err(self.err(
                span,
                format!(
                    "`{}` expects {} argument(s), found {}",
                    builtin.name(),
                    params.len(),
                    args.len()
                ),
            ));
        }

        match builtin {
            Builtin::Puts => self.emit_str_write(STDOUT, &args[0])?,
            Builtin::Panic => {
                self.emit_str_write(STDERR, &args[0])?;
                self.emit(Op::Pop);
                self.emit(Op::Push(TRAP_EXIT_CODE));
                self.emit(Op::Syscall(SYS_EXIT, 1));
            }
            Builtin::Write => {
                let fd = self.emit_expr(&args[0], Some(&Ty::Sword))?;
                self.expect_type(&fd, &Ty::Sword, args[0].span())?;
                self.normalize(&Ty::Sword);
                let bt = self.expr_hint(&args[1]).unwrap_or(Ty::I32);
                if bt == Ty::Str {
                    let st = self.emit_expr(&args[1], Some(&Ty::Str))?;
                    self.expect_type(&st, &Ty::Str, args[1].span())?;
                    self.emit(Op::Dup);
                    self.emit(Op::Load32);
                    let len_slot = self.temp_slot(0);
                    self.emit(Op::Store(len_slot));
                    self.emit(Op::Push(4));
                    self.emit(Op::Add);
                    let ptr_slot = self.temp_slot(1);
                    self.emit(Op::Store(ptr_slot));
                    self.emit(Op::Load(ptr_slot));
                    self.emit(Op::Load(len_slot));
                } else {
                    let pt = self.emit_expr(&args[1], Some(&Ty::Ptr(Box::new(Ty::U8))))?;
                    self.expect_type(&pt, &Ty::Ptr(Box::new(Ty::U8)), args[1].span())?;
                    let lt = self.emit_expr(&args[2], Some(&Ty::Sword))?;
                    self.expect_type(&lt, &Ty::Sword, args[2].span())?;
                    self.normalize(&Ty::Sword);
                }
                self.emit(Op::Syscall(SYS_WRITE, 3));
            }
            Builtin::Open | Builtin::Unlink | Builtin::Mkdir => {
                // Path argument: skip the 4-byte string length prefix.
                let pt = self.emit_expr(&args[0], Some(&Ty::Str))?;
                self.expect_type(&pt, &Ty::Str, args[0].span())?;
                self.emit(Op::Push(4));
                self.emit(Op::Add);
                match builtin {
                    Builtin::Open => {
                        let ft = self.emit_expr(&args[1], Some(&Ty::Sword))?;
                        self.expect_type(&ft, &Ty::Sword, args[1].span())?;
                        self.normalize(&Ty::Sword);
                        self.emit(Op::Syscall(SYS_OPEN, 2));
                    }
                    _ => {
                        let number = if matches!(builtin, Builtin::Unlink) {
                            SYS_UNLINK
                        } else {
                            SYS_MKDIR
                        };
                        self.emit(Op::Syscall(number, 1));
                    }
                }
            }
            Builtin::Exec => {
                // exec(path_ptr, argc, argv_ptr): path is a raw *u8 pointer.
                let pt = self.emit_expr(&args[0], Some(&Ty::Ptr(Box::new(Ty::U8))))?;
                self.expect_type(&pt, &Ty::Ptr(Box::new(Ty::U8)), args[0].span())?;
                let ct = self.emit_expr(&args[1], Some(&Ty::Sword))?;
                self.expect_type(&ct, &Ty::Sword, args[1].span())?;
                self.normalize(&Ty::Sword);
                let at = self.emit_expr(&args[2], None)?;
                if !matches!(at, Ty::Ptr(_)) {
                    return Err(self.err(args[2].span(), "exec argv must be a pointer"));
                }
                self.emit(Op::Syscall(SYS_EXEC, 3));
            }
            Builtin::Stat => {
                let pt = self.emit_expr(&args[0], Some(&Ty::Str))?;
                self.expect_type(&pt, &Ty::Str, args[0].span())?;
                self.emit(Op::Push(4));
                self.emit(Op::Add);
                let ot = self.emit_expr(&args[1], None)?;
                if !matches!(ot, Ty::Ptr(_)) {
                    return Err(self.err(args[1].span(), "stat output must be a pointer"));
                }
                self.emit(Op::Syscall(SYS_STAT, 2));
            }
            _ => {
                for (arg, pty) in args.iter().zip(params.iter()) {
                    let at = self.emit_expr(arg, Some(pty))?;
                    self.expect_type(&at, pty, arg.span())?;
                    self.normalize(pty);
                }
                let number = match builtin {
                    Builtin::Write => SYS_WRITE,
                    Builtin::Read => SYS_READ,
                    Builtin::Close => SYS_CLOSE,
                    Builtin::Dup => SYS_DUP,
                    Builtin::Exit => SYS_EXIT,
                    Builtin::GetPid => SYS_GETPID,
                    Builtin::Yield => SYS_YIELD,
                    Builtin::Sleep => SYS_SLEEP,
                    Builtin::Sbrk | Builtin::Alloc => SYS_SBRK,
                    Builtin::Lseek => SYS_LSEEK,
                    Builtin::Argc => SYS_ARGC,
                    Builtin::Arg => SYS_ARG,
                    Builtin::Fork => SYS_FORK,
                    Builtin::Pipe => SYS_PIPE,
                    Builtin::Wait => SYS_WAIT,
                    Builtin::Dup2 => SYS_DUP2,
                    Builtin::Chdir => SYS_CHDIR,
                    Builtin::Open
                    | Builtin::Unlink
                    | Builtin::Mkdir
                    | Builtin::Stat
                    | Builtin::Exec
                    | Builtin::Panic
                    | Builtin::Puts
                    | Builtin::Syscall => {
                        unreachable!()
                    }
                };
                self.emit(Op::Syscall(number, params.len() as u8));
            }
        }

        if builtin.discards_result() {
            self.emit(Op::Pop);
        }
        Ok(builtin.ret().unwrap_or(Ty::Void))
    }

    /// `syscall(number, args...)`: the number must be a constant integer.
    fn emit_syscall_intrinsic(&mut self, args: &[Expr], span: Span) -> Result<Ty, CompileError> {
        if args.is_empty() {
            return Err(self.err(span, "`syscall` expects a number argument"));
        }
        let Expr::Int(number, nspan) = &args[0] else {
            return Err(self.err(args[0].span(), "syscall number must be a constant integer"));
        };
        if !(0..=i64::from(u16::MAX)).contains(number) {
            return Err(self.err(*nspan, "syscall number out of range"));
        }
        let argc = args.len() - 1;
        if argc > usize::from(u8::MAX) {
            return Err(self.err(span, "too many syscall arguments"));
        }
        for arg in &args[1..] {
            let at = self.emit_expr(arg, None)?;
            if !at.is_int() && !matches!(at, Ty::Ptr(_)) {
                return Err(self.err(arg.span(), "syscall arguments must be integers or pointers"));
            }
        }
        self.emit(Op::Syscall(*number as u16, argc as u8));
        Ok(Ty::Sword)
    }

    /// Evaluate a `str` and write its bytes to `fd`.
    fn emit_str_write(&mut self, fd: i64, arg: &Expr) -> Result<(), CompileError> {
        let st = self.emit_expr(arg, Some(&Ty::Str))?;
        self.expect_type(&st, &Ty::Str, arg.span())?;
        self.emit(Op::Dup);
        self.emit(Op::Load32);
        let len_slot = self.temp_slot(0);
        self.emit(Op::Store(len_slot));
        self.emit(Op::Push(4));
        self.emit(Op::Add);
        let ptr_slot = self.temp_slot(1);
        self.emit(Op::Store(ptr_slot));
        self.emit(Op::Push(fd));
        self.emit(Op::Load(ptr_slot));
        self.emit(Op::Load(len_slot));
        self.emit(Op::Syscall(SYS_WRITE, 3));
        Ok(())
    }

    /// `load<T>(addr)`: read a typed value from memory.
    fn emit_intrinsic_load(
        &mut self,
        declared: &crate::ast::Type,
        addr: &Expr,
        span: Span,
    ) -> Result<Ty, CompileError> {
        let ty = self.resolve_ty(declared)?;
        match &ty {
            Ty::Bool | Ty::Ptr(_) | Ty::Enum(_) => {}
            t if t.is_int() => {}
            other => {
                return Err(self.err(span, format!("cannot `load` a value of type `{other}`")));
            }
        }
        let at = self.emit_expr(addr, None)?;
        if !at.is_int() && !matches!(at, Ty::Ptr(_)) {
            return Err(self.err(addr.span(), "load address must be a word or pointer"));
        }
        self.emit_load_width(&ty);
        Ok(ty)
    }

    /// `store<T>(addr, value)`: write a typed value to memory.
    fn emit_intrinsic_store(
        &mut self,
        declared: &crate::ast::Type,
        addr: &Expr,
        value: &Expr,
        span: Span,
    ) -> Result<Ty, CompileError> {
        let ty = self.resolve_ty(declared)?;
        match &ty {
            Ty::Bool | Ty::Ptr(_) | Ty::Enum(_) => {}
            t if t.is_int() => {}
            other => {
                return Err(self.err(span, format!("cannot `store` a value of type `{other}`")));
            }
        }
        let at = self.emit_expr(addr, None)?;
        if !at.is_int() && !matches!(at, Ty::Ptr(_)) {
            return Err(self.err(addr.span(), "store address must be a word or pointer"));
        }
        let vt = self.emit_expr(value, Some(&ty))?;
        self.expect_type(&vt, &ty, value.span())?;
        self.normalize(&ty);
        self.emit_store_width(&ty);
        Ok(Ty::Void)
    }

    /// The static shape of a base for indexing: a fixed array.
    fn collection_of(&mut self, base: &Expr, span: Span) -> Result<ArrRef, CompileError> {
        match base {
            Expr::Ident(name, _) => {
                if let Some(local) = self.find_local(name) {
                    return match &local.ty {
                        Ty::Array { elem, len } => Ok(ArrRef::Local {
                            slot: local.slot,
                            elem: (**elem).clone(),
                            len: *len,
                        }),
                        other => {
                            Err(self.err(span, format!("`{name}` is not an array (got `{other}`)")))
                        }
                    };
                }
                if let Some(global) = self.globals.get(name) {
                    if let Ty::Array { elem, len } = &global.ty {
                        return Ok(ArrRef::Global {
                            data: global.data,
                            elem: (**elem).clone(),
                            len: *len,
                        });
                    }
                    return Err(self.err(span, format!("`{name}` is not an array")));
                }
                Err(self.err(span, format!("unknown variable `{name}`")))
            }
            Expr::Unary {
                op: UnOp::Deref,
                expr: inner,
                span: dspan,
            } => {
                let vt = self
                    .expr_hint(inner)
                    .ok_or_else(|| self.err(*dspan, "cannot index this expression"))?;
                let Ty::Ptr(ptr_to) = &vt else {
                    return Err(self.err(*dspan, "cannot index a non-pointer"));
                };
                let Ty::Array { elem, len } = &**ptr_to else {
                    return Err(self.err(*dspan, "pointer does not point to an array"));
                };
                Ok(ArrRef::DerefBase {
                    inner: inner.clone(),
                    elem: (**elem).clone(),
                    len: *len,
                })
            }
            Expr::Field {
                base: fb,
                name: fname,
                span: fspan,
            } => {
                let fty = self
                    .field_static(fb, fname)
                    .ok_or_else(|| self.err(*fspan, "cannot index this field"))?;
                match fty {
                    Ty::Array { elem, len } => Ok(ArrRef::FieldBase {
                        base: fb.clone(),
                        name: fname.clone(),
                        span: *fspan,
                        elem: *elem,
                        len,
                    }),
                    other => Err(self.err(
                        *fspan,
                        format!("field `{fname}` is not an array (got `{other}`)"),
                    )),
                }
            }
            other => Err(self.err(other.span(), "array indexing requires an array")),
        }
    }

    fn arr_elem_len(arr: &ArrRef) -> (Ty, usize) {
        match arr {
            ArrRef::Local { elem, len, .. }
            | ArrRef::Global { elem, len, .. }
            | ArrRef::DerefBase { elem, len, .. }
            | ArrRef::FieldBase { elem, len, .. } => (elem.clone(), *len),
        }
    }

    /// Leave the element address of `base[index]` on the stack.
    fn emit_element_addr(
        &mut self,
        base: &Expr,
        index: &Expr,
        span: Span,
    ) -> Result<Ty, CompileError> {
        let arr = self.collection_of(base, span)?;
        let it = self.emit_expr(index, None)?;
        if !it.is_int() {
            return Err(self.err(index.span(), "index must be an integer"));
        }
        let (elem, len) = Self::arr_elem_len(&arr);
        // A constant index that is provably in range needs no bounds check.
        let k = match index {
            Expr::Int(v, _) => Some(*v),
            _ => None,
        };
        self.emit_checked_element_addr(&arr, &elem, len, k)?;
        Ok(elem)
    }

    fn emit_index_load(
        &mut self,
        base: &Expr,
        index: &Expr,
        span: Span,
    ) -> Result<Ty, CompileError> {
        let elem = self.emit_element_addr(base, index, span)?;
        match &elem {
            Ty::Struct(_) => Ok(elem),
            _ => {
                self.emit_load_width(&elem);
                Ok(elem)
            }
        }
    }

    fn compile_index_store(
        &mut self,
        base: &Expr,
        index: &Expr,
        value: &Expr,
        span: Span,
    ) -> Result<(), CompileError> {
        let elem = self.emit_element_addr(base, index, span)?;
        let vt = self.emit_expr(value, Some(&elem))?;
        self.expect_type(&vt, &elem, value.span())?;
        self.normalize(&elem);
        self.emit_store_width(&elem);
        Ok(())
    }
}

fn align_up(value: usize, align: usize) -> usize {
    if align <= 1 {
        value
    } else {
        value.div_ceil(align) * align
    }
}

fn guarantees_return(block: &Block) -> bool {
    match block.stmts.last() {
        Some(Stmt::Return { .. }) => true,
        Some(Stmt::Expr(Expr::Call { callee, .. })) => {
            matches!(&**callee, Expr::Ident(name, _) if name == "exit" || name == "panic")
        }
        Some(Stmt::If {
            then_branch,
            else_branch: Some(else_branch),
            ..
        }) => guarantees_return(then_branch) && guarantees_return(else_branch),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytecode::abi::{SYS_EXIT, SYS_OPEN, SYS_SBRK, SYS_WRITE, TRAP_EXIT_CODE};
    use bytecode::{Host, MemError, MemWidth, SyscallResult, Vm};

    struct MemHost {
        bytes: Vec<u8>,
        writes: Vec<(i64, Vec<u8>)>,
        syscalls: Vec<(u16, Vec<i64>)>,
        brk: i64,
    }

    impl MemHost {
        fn new() -> Self {
            Self {
                bytes: vec![0u8; 0x4_0000],
                writes: Vec::new(),
                syscalls: Vec::new(),
                brk: 0x2_0000,
            }
        }
    }

    impl Host for MemHost {
        fn syscall(&mut self, number: u16, args: &[i64]) -> SyscallResult {
            self.syscalls.push((number, args.to_vec()));
            match number {
                SYS_EXIT => SyscallResult::exiting(args.first().copied().unwrap_or(0)),
                SYS_WRITE => {
                    let fd = args.first().copied().unwrap_or(0);
                    let ptr = args.get(1).copied().unwrap_or(0) as usize;
                    let len = args.get(2).copied().unwrap_or(0) as usize;
                    let end = (ptr + len).min(self.bytes.len());
                    self.writes.push((fd, self.bytes[ptr..end].to_vec()));
                    SyscallResult::continuing(len as i64)
                }
                SYS_SBRK => {
                    let old = self.brk;
                    self.brk += args.first().copied().unwrap_or(0);
                    SyscallResult::continuing(old)
                }
                _ => SyscallResult::continuing(0),
            }
        }

        fn load(&mut self, addr: usize, width: MemWidth) -> Result<u64, MemError> {
            let n = width.bytes();
            let slice = self.bytes.get(addr..addr + n).ok_or(MemError::Unmapped)?;
            let mut buf = [0u8; 8];
            buf[..n].copy_from_slice(slice);
            Ok(u64::from_le_bytes(buf))
        }

        fn store(&mut self, addr: usize, width: MemWidth, value: u64) -> Result<(), MemError> {
            let n = width.bytes();
            let slice = self
                .bytes
                .get_mut(addr..addr + n)
                .ok_or(MemError::NotWritable)?;
            slice.copy_from_slice(&value.to_le_bytes()[..n]);
            Ok(())
        }
    }

    fn data_addrs(image: &ProgramImage) -> Vec<usize> {
        let mut addrs = Vec::new();
        let mut next = 0x1000usize;
        for blob in &image.data {
            addrs.push(next);
            next += blob.len().max(1);
        }
        addrs
    }

    fn run(src: &str) -> (Vm, MemHost) {
        let image = compile(src).expect("should compile");
        let addrs = data_addrs(&image);
        let mut host = MemHost::new();
        for (blob, &addr) in image.data.iter().zip(addrs.iter()) {
            host.bytes[addr..addr + blob.len()].copy_from_slice(blob);
        }
        let mut vm = Vm::from_image(&image, addrs);
        vm.run(&mut host, 1_000_000).expect("should run");
        (vm, host)
    }

    fn ret(src: &str) -> i64 {
        let (vm, _) = run(src);
        vm.exit_code
    }

    #[test]
    fn return_constant() {
        assert_eq!(ret("int main() { return 42; }"), 42);
    }

    #[test]
    fn arithmetic_and_precedence() {
        assert_eq!(ret("int main() { return 2 + 3 * 4; }"), 14);
        assert_eq!(ret("int main() { return (2 + 3) * 4; }"), 20);
        assert_eq!(ret("int main() { return 17 % 5; }"), 2);
    }

    #[test]
    fn declaration_and_assignment() {
        let src = "int main() { int x = 10; x = x - 3; return x; }";
        assert_eq!(ret(src), 7);
    }

    #[test]
    fn if_else_branch() {
        let src = r#"
            int main() {
                int x = 3;
                if (x < 5) { return 100; } else { return 200; }
            }
        "#;
        assert_eq!(ret(src), 100);
    }

    #[test]
    fn while_loop_sums() {
        let src = r#"
            int main() {
                int i = 1;
                int sum = 0;
                while (i <= 10) { sum = sum + i; i = i + 1; }
                return sum;
            }
        "#;
        assert_eq!(ret(src), 55);
    }

    #[test]
    fn for_loop_with_break_and_continue() {
        let src = r#"
            int main() {
                int sum = 0;
                for (int i = 0; i < 10; i = i + 1) {
                    if (i == 3) { continue; }
                    if (i == 7) { break; }
                    sum = sum + i;
                }
                return sum;
            }
        "#;
        assert_eq!(ret(src), 1 + 2 + 4 + 5 + 6);
    }

    #[test]
    fn recursion_works() {
        let src = r#"
            int fact(int n) {
                if (n <= 1) { return 1; }
                return n * fact(n - 1);
            }
            int main() { return fact(5); }
        "#;
        assert_eq!(ret(src), 120);
    }

    #[test]
    fn local_array_read_and_write() {
        let src = r#"
            int main() {
                int a[3] = [10, 20, 30];
                int i = 1;
                a[2] = 7;
                return a[i] + a[0] + a[2];
            }
        "#;
        assert_eq!(ret(src), 20 + 10 + 7);
    }

    #[test]
    fn unsigned_compare_uses_unsigned_ops() {
        let src = r#"
            int main() {
                u8 a = 200;
                u8 b = 100;
                if (a > b) { return 1; }
                return 0;
            }
        "#;
        assert_eq!(ret(src), 1);
    }

    #[test]
    fn signed_shift_and_negative() {
        assert_eq!(ret("int main() { return -8 >> 1; }"), -4);
        assert_eq!(ret("u16 main() { u16 x = 0x8000; return x >> 12; }"), 8);
    }

    #[test]
    fn short_circuit_avoids_call() {
        let src = r#"
            int n = 0;
            bool bump() { n = n + 1; return true; }
            int main() { bool skipped = false && bump(); return n; }
        "#;
        assert_eq!(ret(src), 0);
    }

    #[test]
    fn string_literal_lands_in_data() {
        let image = compile(r#"void main() { puts("hi"); }"#).unwrap();
        assert!(
            image
                .data
                .iter()
                .any(|blob| blob == &[2, 0, 0, 0, b'h', b'i', 0])
        );
    }

    #[test]
    fn write_local_byte_to_stdout() {
        let src = r#"
            void main() {
                u8 c = 65;
                write(1, &c, 1);
            }
        "#;
        let (_, host) = run(src);
        assert_eq!(host.writes, vec![(1, vec![65u8])]);
    }

    #[test]
    fn write_local_array_slice() {
        let src = r#"
            void main() {
                u8 a[4] = [104, 105, 10, 0];
                write(1, &a[0], 3);
            }
        "#;
        let (_, host) = run(src);
        assert_eq!(host.writes, vec![(1, b"hi\n".to_vec())]);
    }

    #[test]
    fn puts_writes_string() {
        let src = r#"void main() { puts("ok\n"); }"#;
        let (_, host) = run(src);
        assert_eq!(host.writes, vec![(1, b"ok\n".to_vec())]);
    }

    #[test]
    fn panic_writes_stderr_and_exits() {
        let src = r#"void main() { panic("bad\n"); }"#;
        let (vm, host) = run(src);
        assert_eq!(vm.exit_code, TRAP_EXIT_CODE);
        assert_eq!(host.writes, vec![(2, b"bad\n".to_vec())]);
    }

    #[test]
    fn alloc_returns_previous_break() {
        assert_eq!(
            ret("word main() { return cast<word>(alloc(16)); }"),
            0x2_0000
        );
    }

    #[test]
    fn open_passes_nul_terminated_path() {
        let src = r#"void main() { open("abc", 0); }"#;
        let (_, host) = run(src);
        let (_, args) = host
            .syscalls
            .iter()
            .find(|(n, _)| *n == SYS_OPEN)
            .expect("open syscall");
        let ptr = args[0] as usize;
        assert_eq!(&host.bytes[ptr..ptr + 4], b"abc\0");
    }

    #[test]
    fn global_scalar_mutation() {
        let src = r#"
            int g = 5;
            int bump() { g = g + 1; return g; }
            int main() { bump(); bump(); return g; }
        "#;
        assert_eq!(ret(src), 7);
    }

    #[test]
    fn global_array_index() {
        let src = r#"
            int a[3] = [1, 2, 3];
            int main() { a[1] = 20; return a[0] + a[1]; }
        "#;
        assert_eq!(ret(src), 21);
    }

    #[test]
    fn void_calls_and_statements() {
        let src = r#"
            int helper() { return 3; }
            void nothing() {}
            void main() { helper(); nothing(); }
        "#;
        let (vm, _) = run(src);
        assert_eq!(vm.exit_code, 0);
    }

    #[test]
    fn local_bounds_violation_traps() {
        let src = "int main() { int a[2] = [10, 20]; return a[5]; }";
        let (vm, _) = run(src);
        assert_eq!(vm.exit_code, TRAP_EXIT_CODE);
        assert!(vm.halted);
    }

    #[test]
    fn global_bounds_violation_traps() {
        let src = "int a[2] = [1, 2]; int main() { return a[9]; }";
        let (vm, _) = run(src);
        assert_eq!(vm.exit_code, TRAP_EXIT_CODE);
    }

    #[test]
    fn struct_field_read_write() {
        let src = r#"
            struct Point { int x; int y; };
            int main() {
                Point p = { x = 3, y = 4 };
                p.x = p.x + 10;
                return p.x + p.y;
            }
        "#;
        assert_eq!(ret(src), 17);
    }

    #[test]
    fn nested_struct_field() {
        let src = r#"
            struct Inner { int a; int b; };
            struct Outer { int head; Inner inner; };
            int main() {
                Outer o = { head = 1, inner = { a = 2, b = 3 } };
                return o.head + o.inner.a + o.inner.b;
            }
        "#;
        assert_eq!(ret(src), 6);
    }

    #[test]
    fn global_struct_mutation() {
        let src = r#"
            struct P { int x; int y; };
            P p = { x = 1, y = 2 };
            int main() { p.y = 20; return p.x + p.y; }
        "#;
        assert_eq!(ret(src), 21);
    }

    #[test]
    fn natural_alignment_and_packed_layout() {
        let src = r#"
            struct A { u8 a; u32 b; };
            struct B layout(packed) { u8 a; u32 b; };
            int main() {
                if (sizeof(A) != 8) { return 1; }
                if (offsetof(A, b) != 4) { return 2; }
                if (alignof(A) != 4) { return 3; }
                if (sizeof(B) != 5) { return 4; }
                if (offsetof(B, b) != 1) { return 5; }
                if (alignof(B) != 1) { return 6; }
                return 0;
            }
        "#;
        assert_eq!(ret(src), 0);
    }

    #[test]
    fn pointer_param_and_deref_write() {
        let src = r#"
            struct P { int x; };
            void bump(*P p) { (*p).x = (*p).x + 5; }
            int main() {
                P p = { x = 1 };
                bump(&p);
                return p.x;
            }
        "#;
        assert_eq!(ret(src), 6);
    }

    #[test]
    fn pointer_arithmetic_advances_by_element_size() {
        let src = r#"
            int sum(*int s, int n) {
                int t = 0;
                for (int i = 0; i < n; i = i + 1) {
                    t = t + *(s + i);
                }
                return t;
            }
            int main() {
                int a[3] = [1, 2, 3];
                return sum(&a, 3);
            }
        "#;
        assert_eq!(ret(src), 6);
    }

    #[test]
    fn load_and_store_intrinsics() {
        let src = r#"
            int main() {
                int x = 0;
                *int addr = addr_of(x);
                store<int>(addr, 41);
                return load<int>(addr) + 1;
            }
        "#;
        assert_eq!(ret(src), 42);
    }

    #[test]
    fn syscall_intrinsic_emits_syscall() {
        let src = r#"void main() { u8 b[1] = [120]; sword n = syscall(2, 1, &b[0], 1); }"#;
        let (_, host) = run(src);
        assert!(
            host.syscalls
                .iter()
                .any(|(n, args)| *n == SYS_WRITE && args[0] == 1)
        );
    }

    #[test]
    fn enum_values_auto_and_explicit() {
        let src = r#"
            enum Color { Red, Green = 5, Blue };
            int main() { return cast<int>(Blue); }
        "#;
        assert_eq!(ret(src), 6);
    }

    #[test]
    fn enum_comparison_and_underlying_type() {
        let src = r#"
            enum Dir : u8 { Up, Down };
            int main() {
                Dir d = Down;
                if (d == Down) { return 1; }
                return 0;
            }
        "#;
        assert_eq!(ret(src), 1);
    }

    #[test]
    fn type_aliases_resolve() {
        let src = r#"
            type Count = u32;
            type CountPtr = *Count;
            Count main() {
                Count c = 3;
                CountPtr p = &c;
                return *p;
            }
        "#;
        assert_eq!(ret(src), 3);
    }

    #[test]
    fn entry_point_replaces_main() {
        let src = r#"
            entry start;
            void start() { puts("hi\n"); }
        "#;
        let (_, host) = run(src);
        assert_eq!(host.writes, vec![(1, b"hi\n".to_vec())]);
    }

    #[test]
    fn null_pointer_constant() {
        let src = r#"
            int main() {
                *u8 p = null;
                if (p == null) { return 1; }
                return 0;
            }
        "#;
        assert_eq!(ret(src), 1);
    }

    #[test]
    fn uninitialized_declarations_are_zeroed() {
        let src = r#"
            int main() {
                int x;
                int a[3];
                return x + a[0] + a[2];
            }
        "#;
        assert_eq!(ret(src), 0);
    }

    #[test]
    fn rejects_bad_phase_f_programs() {
        assert!(
            compile("struct S { int x; }; void f(*S s) { (*s).x = 1; } void main() {}").is_ok()
        );
        assert!(
            compile("struct S { int x; }; S f() { S s = { x = 1 }; return s; } void main() {}")
                .is_err()
        );
        assert!(compile("void f(int a[4]) {} void main() {}").is_err());
        assert!(compile("void main() { Nope v = { }; }").is_err());
        assert!(compile("struct S { int x; }; void main() { S s = { x = 1, x = 2 }; }").is_err());
        assert!(compile("struct S { int x; }; void main() { S s = { z = 1 }; }").is_err());
        assert!(compile("struct S { int x; }; void main() { S a = { x = 1 }; S b = a; }").is_err());
        assert!(compile("enum E { A }; void main() { E e = B; }").is_err());
        assert!(compile("void main() { int x = 5; int y = 0; y = *x; }").is_err());
    }

    #[test]
    fn rejects_bad_programs() {
        assert!(compile("int main() { u8 x = 300; return 0; }").is_err());
        assert!(compile("int main() { return true; }").is_err());
        assert!(compile("void main() { return 1; }").is_err());
        assert!(compile("int main() { return missing; }").is_err());
        assert!(compile("int main() { int a[2] = [1, 2]; a = 5; return 0; }").is_err());
        assert!(compile("int main() { return 0; }").is_ok());
        assert!(compile("int main() { u8 x[2] = [1, 2, 3]; return 0; }").is_err());
        assert!(compile("int main() { u8 x[0] = []; return 0; }").is_ok());
    }

    #[test]
    fn rejects_arity_and_unknown_function() {
        assert!(compile("int f(int a) { return a; } int main() { return f(); }").is_err());
        assert!(compile("int main() { return nope(); }").is_err());
    }

    #[test]
    fn requires_return_on_typed_function() {
        assert!(compile("int f() { int x = 1; } int main() { return 0; }").is_err());
        assert!(compile("int f() { return 1; } int main() { return f(); }").is_ok());
    }

    #[test]
    fn rejects_break_outside_loop() {
        assert!(compile("void main() { break; }").is_err());
        assert!(compile("void main() { continue; }").is_err());
    }

    #[test]
    fn requires_entry_or_main() {
        assert!(compile("void helper() {}").is_err());
        assert!(compile("entry missing; void main() {}").is_err());
    }

    // optimization

    #[test]
    fn constant_folds_arithmetic() {
        let image = compile("int main() { return 2 * 3; }").unwrap();
        // Peephole turned `Push(2); Push(3); Mul` into `Push(6)`.
        assert!(
            !image.code.contains(&Op::Mul),
            "2*3 should fold away the multiply"
        );
        assert!(
            image.code.contains(&Op::Push(6)),
            "2*3 should fold to Push(6)"
        );
        // And it still evaluates correctly.
        assert_eq!(ret("int main() { return 2 * 3; }"), 6);
        assert_eq!(ret("int main() { return 10 - 4 - 3; }"), 3);
    }

    #[test]
    fn identity_add_zero_elided() {
        let image = compile("int main() { int x = 5; return x + 0; }").unwrap();
        // `Load(x); Push(0); Add` -> the Add is dropped.
        assert!(!image.code.contains(&Op::Add), "x+0 should drop the add");
        assert_eq!(ret("int main() { int x = 5; return x + 0; }"), 5);
        assert_eq!(ret("int main() { int x = 7; return x * 1; }"), 7);
    }

    #[test]
    fn constant_index_skips_bounds_check() {
        // All array indices are constant and in range -> no `LtU` bounds check.
        let const_idx = compile("int main() { int a[8]; a[3] = 1; return a[3]; }").unwrap();
        assert!(
            !const_idx.code.contains(&Op::LtU),
            "constant in-range index should skip the bounds check"
        );
        // A variable index still needs the check.
        let var_idx = compile("int main() { int a[8]; int i = 3; a[i] = 1; return 0; }").unwrap();
        assert!(
            var_idx.code.contains(&Op::LtU),
            "variable index must keep the bounds check"
        );
        // Behavior is unchanged.
        assert_eq!(ret("int main() { int a[8]; a[3] = 42; return a[3]; }"), 42);
    }

    #[test]
    fn optimizations_preserve_loop_and_array() {
        // Sum 1..=5 in a loop with an array; exercises folding, elision, and
        // stores/loads together.
        let src = "int main() { int a[5]; int i = 0; while (i < 5) { a[i] = i + 1; i = i + 1; } int s = 0; i = 0; while (i < 5) { s = s + a[i]; i = i + 1; } return s; }";
        assert_eq!(ret(src), 15);
        // Recursion (fib 10) still correct after the peephole.
        assert_eq!(
            ret(
                "int fib(int n) { if (n < 2) { return n; } return fib(n-1) + fib(n-2); } int main() { return fib(10); }"
            ),
            55
        );
    }
}
