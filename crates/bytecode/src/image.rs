//! Versioned binary serialization for [`bytecode::ProgramImage`].
//!
//! This is the on-disk executable format written by `sinuc` and read back by
//! the game.

use crate::{Op, ProgramImage};

/// Magic bytes at the start of every image file.
pub const MAGIC: [u8; 4] = *b"SNUL";
/// Current format version.
pub const VERSION: u32 = 1;

/// Errors while encoding or decoding an image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageError {
    Truncated,
    BadMagic,
    BadVersion(u32),
    BadOp(u8),
    BadWidth(u8),
}

impl std::fmt::Display for ImageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImageError::Truncated => write!(f, "image file is truncated"),
            ImageError::BadMagic => write!(f, "not a sinu image (bad magic)"),
            ImageError::BadVersion(v) => write!(f, "unsupported image version {v}"),
            ImageError::BadOp(tag) => write!(f, "unknown opcode tag {tag}"),
            ImageError::BadWidth(tag) => write!(f, "unknown memory width tag {tag}"),
        }
    }
}

impl std::error::Error for ImageError {}

/// Encode a program image into the versioned binary format.
pub fn encode_image(image: &ProgramImage) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&(image.entry as u32).to_le_bytes());
    out.extend_from_slice(&(image.locals as u32).to_le_bytes());
    out.extend_from_slice(&(image.code.len() as u32).to_le_bytes());
    for op in &image.code {
        encode_op(&mut out, op);
    }
    out.extend_from_slice(&(image.data.len() as u32).to_le_bytes());
    for blob in &image.data {
        out.extend_from_slice(&(blob.len() as u32).to_le_bytes());
        out.extend_from_slice(blob);
    }
    out
}

/// Decode a program image from the versioned binary format.
pub fn decode_image(bytes: &[u8]) -> Result<ProgramImage, ImageError> {
    let mut r = Reader { bytes, pos: 0 };
    let magic = r.array4()?;
    if magic != MAGIC {
        return Err(ImageError::BadMagic);
    }
    let version = r.u32()?;
    if version != VERSION {
        return Err(ImageError::BadVersion(version));
    }
    let entry = r.u32()? as usize;
    let locals = r.u32()? as usize;
    let code_len = r.u32()? as usize;
    let mut code = Vec::with_capacity(code_len.min(1 << 20));
    for _ in 0..code_len {
        code.push(decode_op(&mut r)?);
    }
    let data_len = r.u32()? as usize;
    let mut data = Vec::with_capacity(data_len.min(1 << 16));
    for _ in 0..data_len {
        let len = r.u32()? as usize;
        data.push(r.take(len)?.to_vec());
    }
    Ok(ProgramImage {
        code,
        data,
        locals,
        entry,
    })
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], ImageError> {
        let end = self.pos.checked_add(n).ok_or(ImageError::Truncated)?;
        let slice = self.bytes.get(self.pos..end).ok_or(ImageError::Truncated)?;
        self.pos = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, ImageError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, ImageError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn u32(&mut self) -> Result<u32, ImageError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn i64(&mut self) -> Result<i64, ImageError> {
        Ok(i64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn array4(&mut self) -> Result<[u8; 4], ImageError> {
        Ok(self.take(4)?.try_into().unwrap())
    }
}

fn push_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_i64(out: &mut Vec<u8>, v: i64) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn encode_op(out: &mut Vec<u8>, op: &Op) {
    match op {
        Op::Push(v) => {
            out.push(0);
            push_i64(out, *v);
        }
        Op::Pop => out.push(1),
        Op::Dup => out.push(2),
        Op::Swap => out.push(3),
        Op::Load(slot) => {
            out.push(4);
            push_u16(out, *slot);
        }
        Op::Store(slot) => {
            out.push(5);
            push_u16(out, *slot);
        }
        Op::Lea(slot) => {
            out.push(6);
            push_u16(out, *slot);
        }
        Op::Add => out.push(7),
        Op::Sub => out.push(8),
        Op::Mul => out.push(9),
        Op::Div => out.push(10),
        Op::Mod => out.push(11),
        Op::Neg => out.push(12),
        Op::And => out.push(13),
        Op::Or => out.push(14),
        Op::Xor => out.push(15),
        Op::Shl => out.push(16),
        Op::ShrU => out.push(17),
        Op::Sar => out.push(18),
        Op::BitNot => out.push(19),
        Op::Eq => out.push(20),
        Op::Ne => out.push(21),
        Op::Lt => out.push(22),
        Op::Le => out.push(23),
        Op::Gt => out.push(24),
        Op::Ge => out.push(25),
        Op::LtU => out.push(26),
        Op::LeU => out.push(27),
        Op::GtU => out.push(28),
        Op::GeU => out.push(29),
        Op::Not => out.push(30),
        Op::Jmp(addr) => {
            out.push(31);
            out.extend_from_slice(&(*addr as u32).to_le_bytes());
        }
        Op::Jz(addr) => {
            out.push(32);
            out.extend_from_slice(&(*addr as u32).to_le_bytes());
        }
        Op::Jnz(addr) => {
            out.push(33);
            out.extend_from_slice(&(*addr as u32).to_le_bytes());
        }
        Op::Enter(argc, nlocals) => {
            out.push(34);
            out.push(*argc);
            push_u16(out, *nlocals);
        }
        Op::Call(addr) => {
            out.push(35);
            out.extend_from_slice(&(*addr as u32).to_le_bytes());
        }
        Op::Ret => out.push(36),
        Op::Load8 => out.push(37),
        Op::Load16 => out.push(38),
        Op::Load32 => out.push(39),
        Op::Load64 => out.push(40),
        Op::Store8 => out.push(41),
        Op::Store16 => out.push(42),
        Op::Store32 => out.push(43),
        Op::Store64 => out.push(44),
        Op::SignExt8 => out.push(45),
        Op::SignExt16 => out.push(46),
        Op::SignExt32 => out.push(47),
        Op::PushData(idx) => {
            out.push(48);
            push_u16(out, *idx);
        }
        Op::Syscall(number, argc) => {
            out.push(49);
            push_u16(out, *number);
            out.push(*argc);
        }
        Op::Halt => out.push(50),
    }
}

fn decode_op(r: &mut Reader<'_>) -> Result<Op, ImageError> {
    let tag = r.u8()?;
    Ok(match tag {
        0 => Op::Push(r.i64()?),
        1 => Op::Pop,
        2 => Op::Dup,
        3 => Op::Swap,
        4 => Op::Load(r.u16()?),
        5 => Op::Store(r.u16()?),
        6 => Op::Lea(r.u16()?),
        7 => Op::Add,
        8 => Op::Sub,
        9 => Op::Mul,
        10 => Op::Div,
        11 => Op::Mod,
        12 => Op::Neg,
        13 => Op::And,
        14 => Op::Or,
        15 => Op::Xor,
        16 => Op::Shl,
        17 => Op::ShrU,
        18 => Op::Sar,
        19 => Op::BitNot,
        20 => Op::Eq,
        21 => Op::Ne,
        22 => Op::Lt,
        23 => Op::Le,
        24 => Op::Gt,
        25 => Op::Ge,
        26 => Op::LtU,
        27 => Op::LeU,
        28 => Op::GtU,
        29 => Op::GeU,
        30 => Op::Not,
        31 => Op::Jmp(r.u32()? as usize),
        32 => Op::Jz(r.u32()? as usize),
        33 => Op::Jnz(r.u32()? as usize),
        34 => {
            let argc = r.u8()?;
            let nlocals = r.u16()?;
            Op::Enter(argc, nlocals)
        }
        35 => Op::Call(r.u32()? as usize),
        36 => Op::Ret,
        37 => Op::Load8,
        38 => Op::Load16,
        39 => Op::Load32,
        40 => Op::Load64,
        41 => Op::Store8,
        42 => Op::Store16,
        43 => Op::Store32,
        44 => Op::Store64,
        45 => Op::SignExt8,
        46 => Op::SignExt16,
        47 => Op::SignExt32,
        48 => Op::PushData(r.u16()?),
        49 => {
            let number = r.u16()?;
            let argc = r.u8()?;
            Op::Syscall(number, argc)
        }
        50 => Op::Halt,
        other => return Err(ImageError::BadOp(other)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_a_program() {
        let image = ProgramImage {
            code: vec![Op::Push(2), Op::Push(3), Op::Add, Op::Halt],
            data: vec![b"payload".to_vec()],
            locals: 2,
            entry: 0,
        };
        let bytes = encode_image(&image);
        let back = decode_image(&bytes).expect("decodes");
        assert_eq!(back, image);
    }

    #[test]
    fn rejects_bad_magic_and_truncation() {
        assert_eq!(decode_image(b"NOPE"), Err(ImageError::BadMagic));
        assert_eq!(decode_image(&MAGIC), Err(ImageError::Truncated));
    }
}
