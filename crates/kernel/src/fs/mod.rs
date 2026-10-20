//! Kernel file system: descriptor tables plus the on-disk VFS.

pub mod fd;
pub mod vfs;

pub use fd::{
    FILE_STDIN, FILE_STDOUT, Fd, FdTable, File, FileId, FileTable, PipeEnd, STDERR, STDIN, STDOUT,
};
pub use vfs::{
    DIRENT_SIZE, DIRSIZ, Dirent, FsError, MAGIC, MAX_FILE, MAX_PATH, ROOT_INO, Stat, T_DIR, T_FILE,
    Vfs,
};
