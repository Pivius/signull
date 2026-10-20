//! Versioned binary serialization for [`bytecode::ProgramImage`].
//!
//! Re-exported from [`bytecode::image`].

pub use bytecode::image::{ImageError, MAGIC, VERSION, decode_image, encode_image};
