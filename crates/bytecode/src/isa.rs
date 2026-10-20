//! Instruction set for the stack machine.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemWidth {
    B8,
    B16,
    B32,
    B64,
}

impl MemWidth {
    pub fn bytes(self) -> usize {
        match self {
            MemWidth::B8 => 1,
            MemWidth::B16 => 2,
            MemWidth::B32 => 4,
            MemWidth::B64 => 8,
        }
    }
}

/// Memory access errors
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemError {
    Unmapped,
    NotWritable,
}

/// Instruction operands
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Push(i64),
    Pop,
    Dup,
    Swap,
    Load(u16),
    Store(u16),
    /// Push the address of frame slot `u16`
    Lea(u16),

    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Neg,
    And,
    Or,
    Xor,
    Shl,
    ShrU,
    Sar,
    BitNot,

    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    LtU,
    LeU,
    GtU,
    GeU,
    Not,

    Jmp(usize),
    Jz(usize),
    Jnz(usize),

    /// Enter a stack frame. Pops `argc` arguments into locals, then allocates
    /// `nlocals` slots for the new frame.
    Enter(u8, u16),
    Call(usize),
    Ret,

    Load8,
    Load16,
    Load32,
    Load64,

    Store8,
    Store16,
    Store32,
    Store64,

    SignExt8,
    SignExt16,
    SignExt32,

    PushData(u16),

    Syscall(u16, u8),
    Halt,
}

/// Actions after a syscall returns
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SyscallAction {
    #[default]
    Continue,
    Block,
    Exit(i64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SyscallResult {
    pub value: i64,
    pub action: SyscallAction,
    pub retry: bool,
    /// End the current quantum immediately after this syscall (used in
    /// `fork`/`exec`, deferred work happens once `Vm::run` returns).
    pub stop: bool,
}

impl SyscallResult {
    pub fn continuing(value: i64) -> Self {
        Self {
            value,
            action: SyscallAction::Continue,
            retry: false,
            stop: false,
        }
    }

    /// Continue but stop the quantum after this syscall.
    pub fn continuing_and_stop(value: i64) -> Self {
        Self {
            value,
            action: SyscallAction::Continue,
            retry: false,
            stop: true,
        }
    }

    pub fn retry_block() -> Self {
        Self {
            value: 0,
            action: SyscallAction::Block,
            retry: true,
            stop: false,
        }
    }

    pub fn sleep_block() -> Self {
        Self {
            value: 0,
            action: SyscallAction::Block,
            retry: false,
            stop: false,
        }
    }

    pub fn exiting(code: i64) -> Self {
        Self {
            value: code,
            action: SyscallAction::Exit(code),
            retry: false,
            stop: false,
        }
    }
}

/// Executable user program
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Program {
    pub code: Vec<Op>,
    pub data: Vec<Vec<u8>>,
    pub data_addrs: Vec<usize>,
}

impl Program {
    pub fn new(code: Vec<Op>) -> Self {
        Self {
            code,
            data: Vec::new(),
            data_addrs: Vec::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.code.len()
    }

    pub fn is_empty(&self) -> bool {
        self.code.is_empty()
    }
}

/// Non mapped program image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProgramImage {
    pub code: Vec<Op>,
    pub data: Vec<Vec<u8>>,
    pub locals: usize,
    pub entry: usize,
}

impl ProgramImage {
    pub fn new(code: Vec<Op>, locals: usize) -> Self {
        Self {
            code,
            data: Vec::new(),
            locals,
            entry: 0,
        }
    }

    pub fn with_data(mut self, data: Vec<Vec<u8>>) -> Self {
        self.data = data;
        self
    }

    pub fn resolve(&self, data_addrs: Vec<usize>) -> Program {
        Program {
            code: self.code.clone(),
            data: self.data.clone(),
            data_addrs,
        }
    }
}

/// Errors while executing a program
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VmError {
    StackUnderflow,
    StackOverflow,
    CallDepthExceeded,
    FrameOverflow,
    DivideByZero,
    BadLocal(u16),
    BadFrame,
    BadJump(usize),
    BadData(u16),
    EmptyCallStack,
    OutOfBounds,
    Halted,
    Blocked,
    Memory(MemError),
}

impl std::fmt::Display for VmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VmError::StackUnderflow => write!(f, "stack underflow"),
            VmError::StackOverflow => write!(f, "stack overflow"),
            VmError::CallDepthExceeded => write!(f, "call depth limit reached"),
            VmError::FrameOverflow => write!(f, "out of stack space"),
            VmError::DivideByZero => write!(f, "division by zero"),
            VmError::BadLocal(slot) => write!(f, "bad local slot {slot}"),
            VmError::BadFrame => write!(f, "malformed stack frame"),
            VmError::BadJump(target) => write!(f, "bad jump target {target}"),
            VmError::BadData(index) => write!(f, "bad data blob {index}"),
            VmError::EmptyCallStack => write!(f, "return with empty call stack"),
            VmError::OutOfBounds => write!(f, "program counter out of bounds"),
            VmError::Halted => write!(f, "vm is already halted"),
            VmError::Blocked => write!(f, "vm is blocked"),
            VmError::Memory(MemError::Unmapped) => write!(f, "unmapped memory access"),
            VmError::Memory(MemError::NotWritable) => write!(f, "write to read-only memory"),
        }
    }
}

impl std::error::Error for VmError {}
