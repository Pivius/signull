//! Checked types for `sinu`.

use std::fmt;

/// A resolved type. `word`/`sword` are 32-bit on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ty {
    Bool,
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
    Word,
    Sword,
    /// String literal type, valid only as a `puts`/`panic`/`open` argument.
    Str,
    /// Raw pointer to `T`.
    Ptr(Box<Ty>),
    /// User-defined struct.
    Struct(String),
    /// User-defined enum.
    Enum(String),
    Array {
        elem: Box<Ty>,
        len: usize,
    },
    Void,
}

impl Ty {
    pub fn is_int(&self) -> bool {
        matches!(
            self,
            Ty::I8
                | Ty::I16
                | Ty::I32
                | Ty::I64
                | Ty::U8
                | Ty::U16
                | Ty::U32
                | Ty::U64
                | Ty::Word
                | Ty::Sword
        )
    }

    pub fn signed(&self) -> bool {
        matches!(self, Ty::I8 | Ty::I16 | Ty::I32 | Ty::I64 | Ty::Sword)
    }

    pub fn bits(&self) -> u8 {
        match self {
            Ty::I8 | Ty::U8 => 8,
            Ty::I16 | Ty::U16 => 16,
            Ty::I32 | Ty::U32 | Ty::Word | Ty::Sword => 32,
            Ty::I64 | Ty::U64 => 64,
            _ => 0,
        }
    }

    /// In-memory size. Struct sizes require the compiler's layout
    /// table, see `codegen::Compiler::size_of`.
    pub fn size_bytes(&self) -> usize {
        match self {
            Ty::Bool => 1,
            Ty::Str | Ty::Ptr(_) => 4,
            Ty::Enum(_) => 4,   // default underlying u32
            Ty::Struct(_) => 0, // see `Compiler::size_of`
            Ty::Array { elem, len } => elem.size_bytes() * len,
            Ty::Void => 0,
            other => (other.bits() / 8) as usize,
        }
    }

    /// Inclusive range of representable values, for integer literal checking.
    pub fn int_range(&self) -> Option<(i64, i64)> {
        Some(match self {
            Ty::I8 => (i64::from(i8::MIN), i64::from(i8::MAX)),
            Ty::I16 => (i64::from(i16::MIN), i64::from(i16::MAX)),
            Ty::I32 => (i64::from(i32::MIN), i64::from(i32::MAX)),
            Ty::I64 => (i64::MIN, i64::MAX),
            Ty::U8 => (0, i64::from(u8::MAX)),
            Ty::U16 => (0, i64::from(u16::MAX)),
            Ty::U32 | Ty::Word => (0, i64::from(u32::MAX)),
            Ty::Sword => (i64::from(i32::MIN), i64::from(i32::MAX)),
            Ty::U64 => (0, i64::MAX),
            _ => return None,
        })
    }
}

impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ty::Bool => write!(f, "bool"),
            Ty::I8 => write!(f, "i8"),
            Ty::I16 => write!(f, "i16"),
            Ty::I32 => write!(f, "i32"),
            Ty::I64 => write!(f, "i64"),
            Ty::U8 => write!(f, "u8"),
            Ty::U16 => write!(f, "u16"),
            Ty::U32 => write!(f, "u32"),
            Ty::U64 => write!(f, "u64"),
            Ty::Word => write!(f, "word"),
            Ty::Sword => write!(f, "sword"),
            Ty::Str => write!(f, "str"),
            Ty::Ptr(inner) => write!(f, "*{inner}"),
            Ty::Struct(name) => write!(f, "{name}"),
            Ty::Enum(name) => write!(f, "{name}"),
            Ty::Array { elem, len } => write!(f, "[{elem}; {len}]"),
            Ty::Void => write!(f, "void"),
        }
    }
}
