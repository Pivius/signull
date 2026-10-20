//! Inode filesystem on the simulated disk.
//!
//! Layout (block = 512 bytes):
//!
//! ```text
//! [ superblock | inode bitmap | data bitmap | inode table | data blocks ]
//! ```
//!
//! Inodes hold up to `NDIRECT` direct block pointers plus one indirect block
//! (256 entries), so a file can reach ~136 KiB. Directories are ordinary
//! inodes whose data is an array of fixed-size dirents; reading a directory
//! yields raw dirent records, so `ls` is just `read` + unpack.

use crate::dev::Disk;

/// Magic number in the superblock.
pub const MAGIC: u32 = 0x5349_4E31; // "SIN1"
pub const BLOCK_SIZE: usize = 512;
pub const NINODES: u16 = 64;
pub const INODE_SIZE: usize = 64;
pub const INODES_PER_BLOCK: usize = BLOCK_SIZE / INODE_SIZE;
pub const ITABLE_BLOCKS: usize = (NINODES as usize).div_ceil(INODES_PER_BLOCK);
pub const NDIRECT: usize = 12;
pub const INDIRECT_ENTRIES: usize = BLOCK_SIZE / 2;
pub const MAX_FILE_BLOCKS: usize = NDIRECT + INDIRECT_ENTRIES;
pub const MAX_FILE: u32 = (MAX_FILE_BLOCKS * BLOCK_SIZE) as u32;
/// Reserved: inode 0 is never allocated; inode 1 is the root directory.
pub const ROOT_INO: u16 = 1;
/// Maximum bytes in a path component / dirent name.
pub const DIRSIZ: usize = 14;
/// Maximum path length handled by the walker.
pub const MAX_PATH: usize = 128;

pub const T_FREE: u8 = 0;
pub const T_FILE: u8 = 1;
pub const T_DIR: u8 = 2;

/// On-disk inode (64 bytes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiskInode {
    pub kind: u8,
    pub nlink: u8,
    pub size: u32,
    pub direct: [u16; NDIRECT],
    pub indirect: u16,
}

impl DiskInode {
    fn free() -> Self {
        Self {
            kind: T_FREE,
            nlink: 0,
            size: 0,
            direct: [0; NDIRECT],
            indirect: 0,
        }
    }

    fn encode(&self, out: &mut [u8]) {
        out[..INODE_SIZE].fill(0);
        out[0] = self.kind;
        out[1] = self.nlink;
        out[4..8].copy_from_slice(&self.size.to_le_bytes());
        for (i, block) in self.direct.iter().enumerate() {
            out[8 + i * 2..10 + i * 2].copy_from_slice(&block.to_le_bytes());
        }
        out[32..34].copy_from_slice(&self.indirect.to_le_bytes());
    }

    fn decode(buf: &[u8]) -> Self {
        let mut direct = [0u16; NDIRECT];
        for (i, slot) in direct.iter_mut().enumerate() {
            *slot = u16::from_le_bytes([buf[8 + i * 2], buf[9 + i * 2]]);
        }
        Self {
            kind: buf[0],
            nlink: buf[1],
            size: u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]),
            direct,
            indirect: u16::from_le_bytes([buf[32], buf[33]]),
        }
    }
}

/// On-disk directory entry (16 bytes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dirent {
    pub inum: u16,
    pub name: [u8; DIRSIZ],
}

impl Dirent {
    pub fn new(inum: u16, name: &str) -> Self {
        let mut bytes = [0u8; DIRSIZ];
        let len = name.len().min(DIRSIZ);
        bytes[..len].copy_from_slice(&name.as_bytes()[..len]);
        Self { inum, name: bytes }
    }

    pub fn name_str(&self) -> &str {
        let len = self.name.iter().position(|&b| b == 0).unwrap_or(DIRSIZ);
        std::str::from_utf8(&self.name[..len]).unwrap_or("")
    }

    pub fn encode(&self, out: &mut [u8]) {
        out[0..2].copy_from_slice(&self.inum.to_le_bytes());
        out[2..2 + DIRSIZ].copy_from_slice(&self.name);
    }

    pub fn decode(buf: &[u8]) -> Self {
        let mut name = [0u8; DIRSIZ];
        name.copy_from_slice(&buf[2..2 + DIRSIZ]);
        Self {
            inum: u16::from_le_bytes([buf[0], buf[1]]),
            name,
        }
    }
}

pub const DIRENT_SIZE: usize = 2 + DIRSIZ;

/// Result of a successful `stat`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stat {
    pub kind: u8,
    pub size: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FsError {
    NotFound,
    Exist,
    NotDir,
    IsDir,
    NoSpace,
    Inval,
    Corrupt,
}

impl std::fmt::Display for FsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FsError::NotFound => write!(f, "no such file or directory"),
            FsError::Exist => write!(f, "file exists"),
            FsError::NotDir => write!(f, "not a directory"),
            FsError::IsDir => write!(f, "is a directory"),
            FsError::NoSpace => write!(f, "no space left on device"),
            FsError::Inval => write!(f, "invalid argument"),
            FsError::Corrupt => write!(f, "filesystem corrupt"),
        }
    }
}

impl std::error::Error for FsError {}

/// Write a fresh filesystem onto a blank (or previously used) disk.
pub fn format(disk: &mut Disk) -> Result<(), FsError> {
    let total = u32::try_from(disk.block_count()).map_err(|_| FsError::NoSpace)?;
    let itable_start = 3u32;
    let data_start = itable_start + ITABLE_BLOCKS as u32;
    if total <= data_start {
        return Err(FsError::NoSpace);
    }
    let data_blocks = total - data_start;

    // Wipe metadata blocks.
    let zero = vec![0u8; BLOCK_SIZE];
    for b in 0..data_start {
        disk.write_block(b as usize, &zero)
            .ok_or(FsError::Corrupt)?;
    }
    // Superblock.
    let mut sb = [0u8; BLOCK_SIZE];
    sb[0..4].copy_from_slice(&MAGIC.to_le_bytes());
    sb[4..8].copy_from_slice(&total.to_le_bytes());
    sb[8..12].copy_from_slice(&u32::from(NINODES).to_le_bytes());
    sb[12..16].copy_from_slice(&itable_start.to_le_bytes());
    sb[16..20].copy_from_slice(&data_start.to_le_bytes());
    disk.write_block(0, &sb).ok_or(FsError::Corrupt)?;

    // Inode bitmap: reserve inode 0 and the root inode 1.
    let mut ibitmap = [0u8; BLOCK_SIZE];
    ibitmap[0] = 0b11;
    disk.write_block(1, &ibitmap).ok_or(FsError::Corrupt)?;

    // Data bitmap: reserve the first data block for the root directory.
    let mut dbitmap = [0u8; BLOCK_SIZE];
    dbitmap[0] = 0b1;
    disk.write_block(2, &dbitmap).ok_or(FsError::Corrupt)?;

    // Zero the inode table.
    for b in 0..ITABLE_BLOCKS {
        disk.write_block(itable_start as usize + b, &zero)
            .ok_or(FsError::Corrupt)?;
    }

    // Root inode.
    let root = DiskInode {
        kind: T_DIR,
        nlink: 2,
        size: (2 * DIRENT_SIZE) as u32,
        direct: {
            let mut d = [0u16; NDIRECT];
            d[0] = data_start as u16;
            d
        },
        indirect: 0,
    };
    write_inode(disk, &Layout::current(disk)?, ROOT_INO, &root)?;

    // Root data: `.` and `..`.
    let mut block = vec![0u8; BLOCK_SIZE];
    Dirent::new(ROOT_INO, ".").encode(&mut block[0..DIRENT_SIZE]);
    Dirent::new(ROOT_INO, "..").encode(&mut block[DIRENT_SIZE..2 * DIRENT_SIZE]);
    disk.write_block(data_start as usize, &block)
        .ok_or(FsError::Corrupt)?;

    debug_assert!(data_blocks > 0);
    Ok(())
}

/// Superblock-derived geometry, read from the disk on mount.
#[derive(Clone, Copy, Debug)]
struct Layout {
    total_blocks: u32,
    itable_start: u32,
    data_start: u32,
}

impl Layout {
    fn current(disk: &Disk) -> Result<Self, FsError> {
        let sb = disk.read_block(0).ok_or(FsError::Corrupt)?;
        let magic = u32::from_le_bytes([sb[0], sb[1], sb[2], sb[3]]);
        if magic != MAGIC {
            return Err(FsError::Corrupt);
        }
        Ok(Self {
            total_blocks: u32::from_le_bytes([sb[4], sb[5], sb[6], sb[7]]),
            itable_start: u32::from_le_bytes([sb[12], sb[13], sb[14], sb[15]]),
            data_start: u32::from_le_bytes([sb[16], sb[17], sb[18], sb[19]]),
        })
    }
}

fn write_inode(
    disk: &mut Disk,
    layout: &Layout,
    inum: u16,
    inode: &DiskInode,
) -> Result<(), FsError> {
    let block = layout.itable_start + (inum as u32) / INODES_PER_BLOCK as u32;
    let index = inum as usize % INODES_PER_BLOCK;
    let mut buf = [0u8; BLOCK_SIZE];
    let existing = disk.read_block(block as usize).ok_or(FsError::Corrupt)?;
    buf.copy_from_slice(existing);
    let mut encoded = [0u8; INODE_SIZE];
    inode.encode(&mut encoded);
    let start = index * INODE_SIZE;
    buf[start..start + INODE_SIZE].copy_from_slice(&encoded);
    disk.write_block(block as usize, &buf)
        .ok_or(FsError::Corrupt)
}

fn read_inode(disk: &Disk, layout: &Layout, inum: u16) -> Result<DiskInode, FsError> {
    let block = layout.itable_start + (inum as u32) / INODES_PER_BLOCK as u32;
    let index = inum as usize % INODES_PER_BLOCK;
    let buf = disk.read_block(block as usize).ok_or(FsError::Corrupt)?;
    let start = index * INODE_SIZE;
    Ok(DiskInode::decode(&buf[start..start + INODE_SIZE]))
}

/// Bitmap helpers over a block-sized buffer.
fn bit_test(buf: &[u8], bit: usize) -> bool {
    buf[bit / 8] & (1 << (bit % 8)) != 0
}

fn bit_set(buf: &mut [u8], bit: usize) {
    buf[bit / 8] |= 1 << (bit % 8);
}

fn bit_clear(buf: &mut [u8], bit: usize) {
    buf[bit / 8] &= !(1 << (bit % 8));
}

/// Mounted filesystem operations over a disk.
pub struct Vfs<'a> {
    disk: &'a mut Disk,
    layout: Layout,
}

impl<'a> Vfs<'a> {
    /// Mount an already-formatted disk.
    pub fn mount(disk: &'a mut Disk) -> Result<Self, FsError> {
        let layout = Layout::current(disk)?;
        Ok(Self { disk, layout })
    }

    /// Format if the disk is blank; mount either way.
    pub fn format_if_needed(disk: &'a mut Disk) -> Result<Self, FsError> {
        let blank = disk
            .read_block(0)
            .map(|sb| u32::from_le_bytes([sb[0], sb[1], sb[2], sb[3]]) != MAGIC)
            .unwrap_or(true);
        if blank {
            format(disk)?;
        }
        Self::mount(disk)
    }

    fn balloc(&mut self) -> Result<u16, FsError> {
        let mut buf = [0u8; BLOCK_SIZE];
        let existing = self.disk.read_block(2).ok_or(FsError::Corrupt)?;
        buf.copy_from_slice(existing);
        let start = self.layout.data_start as usize;
        let total = self.layout.total_blocks as usize;
        for bit in 0..(total - start) {
            if !bit_test(&buf, bit) {
                bit_set(&mut buf, bit);
                self.disk.write_block(2, &buf).ok_or(FsError::Corrupt)?;
                let block = (start + bit) as u16;
                let zero = vec![0u8; BLOCK_SIZE];
                self.disk
                    .write_block(block as usize, &zero)
                    .ok_or(FsError::Corrupt)?;
                return Ok(block);
            }
        }
        Err(FsError::NoSpace)
    }

    fn bfree(&mut self, block: u16) -> Result<(), FsError> {
        let bit = block as usize - self.layout.data_start as usize;
        let mut buf = [0u8; BLOCK_SIZE];
        let existing = self.disk.read_block(2).ok_or(FsError::Corrupt)?;
        buf.copy_from_slice(existing);
        bit_clear(&mut buf, bit);
        self.disk.write_block(2, &buf).ok_or(FsError::Corrupt)
    }

    fn ialloc(&mut self, kind: u8) -> Result<u16, FsError> {
        let mut buf = [0u8; BLOCK_SIZE];
        let existing = self.disk.read_block(1).ok_or(FsError::Corrupt)?;
        buf.copy_from_slice(existing);
        for inum in 1..NINODES {
            if !bit_test(&buf, inum as usize) {
                bit_set(&mut buf, inum as usize);
                self.disk.write_block(1, &buf).ok_or(FsError::Corrupt)?;
                let inode = DiskInode {
                    kind,
                    nlink: 1,
                    size: 0,
                    direct: [0; NDIRECT],
                    indirect: 0,
                };
                write_inode(self.disk, &self.layout, inum, &inode)?;
                return Ok(inum);
            }
        }
        Err(FsError::NoSpace)
    }

    fn inode_mut(&mut self, inum: u16) -> Result<(DiskInode, u32), FsError> {
        let inode = read_inode(self.disk, &self.layout, inum)?;
        Ok((inode, self.layout.itable_start))
    }

    fn inode_write(&mut self, inum: u16, inode: &DiskInode) -> Result<(), FsError> {
        write_inode(self.disk, &self.layout, inum, inode)
    }

    /// Map a logical file block to a disk block, allocating on demand.
    fn bmap(&mut self, mut inode: DiskInode, file_block: u32) -> Result<(u16, DiskInode), FsError> {
        if file_block < NDIRECT as u32 {
            let idx = file_block as usize;
            if inode.direct[idx] == 0 {
                inode.direct[idx] = self.balloc()?;
            }
            return Ok((inode.direct[idx], inode));
        }
        let indirect_idx = file_block as usize - NDIRECT;
        if indirect_idx >= INDIRECT_ENTRIES {
            return Err(FsError::NoSpace);
        }
        if inode.indirect == 0 {
            inode.indirect = self.balloc()?;
        }
        let iblock = inode.indirect;
        let mut buf = [0u8; BLOCK_SIZE];
        let existing = self
            .disk
            .read_block(iblock as usize)
            .ok_or(FsError::Corrupt)?;
        buf.copy_from_slice(existing);
        let mut entry = u16::from_le_bytes([buf[indirect_idx * 2], buf[indirect_idx * 2 + 1]]);
        if entry == 0 {
            entry = self.balloc()?;
            buf[indirect_idx * 2..indirect_idx * 2 + 2].copy_from_slice(&entry.to_le_bytes());
            self.disk
                .write_block(iblock as usize, &buf)
                .ok_or(FsError::Corrupt)?;
        }
        Ok((entry, inode))
    }

    fn inode_blocks_free(&mut self, inode: &DiskInode) -> Result<(), FsError> {
        for &block in inode.direct.iter().filter(|&&b| b != 0) {
            self.bfree(block)?;
        }
        if inode.indirect != 0 {
            let entries: Vec<u16> = {
                let buf = self
                    .disk
                    .read_block(inode.indirect as usize)
                    .ok_or(FsError::Corrupt)?;
                (0..INDIRECT_ENTRIES)
                    .map(|i| u16::from_le_bytes([buf[i * 2], buf[i * 2 + 1]]))
                    .collect()
            };
            for entry in entries.into_iter().filter(|&e| e != 0) {
                self.bfree(entry)?;
            }
            self.bfree(inode.indirect)?;
        }
        Ok(())
    }

    /// Read up to `buf.len()` bytes starting at `offset`. Returns bytes read
    /// (short at EOF).
    pub fn read(&mut self, inum: u16, offset: u32, buf: &mut [u8]) -> Result<usize, FsError> {
        let (mut inode, _) = self.inode_mut(inum)?;
        if inode.kind == T_FREE {
            return Err(FsError::Inval);
        }
        if offset >= inode.size {
            return Ok(0);
        }
        let end = (offset as usize + buf.len()).min(inode.size as usize);
        let mut done = 0usize;
        let mut pos = offset as usize;
        while pos < end {
            let file_block = (pos / BLOCK_SIZE) as u32;
            let (block, next) = self.bmap(inode, file_block)?;
            inode = next;
            let sec = self
                .disk
                .read_block(block as usize)
                .ok_or(FsError::Corrupt)?;
            let start = pos % BLOCK_SIZE;
            let take = (BLOCK_SIZE - start).min(end - pos);
            buf[done..done + take].copy_from_slice(&sec[start..start + take]);
            done += take;
            pos += take;
        }
        Ok(done)
    }

    /// Write `data` at `offset`, extending the file (zero-filled holes) as
    /// needed. Returns bytes written.
    pub fn write(&mut self, inum: u16, offset: u32, data: &[u8]) -> Result<usize, FsError> {
        let (mut inode, _) = self.inode_mut(inum)?;
        if inode.kind == T_FREE {
            return Err(FsError::Inval);
        }
        let mut pos = offset as usize;
        let end = pos + data.len();
        if end > MAX_FILE as usize {
            return Err(FsError::NoSpace);
        }
        let mut done = 0usize;
        while pos < end {
            let file_block = (pos / BLOCK_SIZE) as u32;
            let (block, next) = self.bmap(inode, file_block)?;
            inode = next;
            let mut sec = [0u8; BLOCK_SIZE];
            if !pos.is_multiple_of(BLOCK_SIZE) || (end - pos) < BLOCK_SIZE {
                let existing = self
                    .disk
                    .read_block(block as usize)
                    .ok_or(FsError::Corrupt)?;
                sec.copy_from_slice(existing);
            }
            let start = pos % BLOCK_SIZE;
            let take = (BLOCK_SIZE - start).min(end - pos);
            sec[start..start + take].copy_from_slice(&data[done..done + take]);
            self.disk
                .write_block(block as usize, &sec)
                .ok_or(FsError::Corrupt)?;
            done += take;
            pos += take;
        }
        if end as u32 > inode.size {
            inode.size = end as u32;
        }
        self.inode_write(inum, &inode)?;
        Ok(done)
    }

    /// Drop all data, keeping the inode.
    pub fn truncate(&mut self, inum: u16) -> Result<(), FsError> {
        let (mut inode, _) = self.inode_mut(inum)?;
        if inode.kind == T_FREE {
            return Err(FsError::Inval);
        }
        self.inode_blocks_free(&inode)?;
        inode.size = 0;
        inode.direct = [0; NDIRECT];
        inode.indirect = 0;
        self.inode_write(inum, &inode)
    }

    /// Truncate then write — used by `O_TRUNC`.
    pub fn truncate_write(&mut self, inum: u16, data: &[u8]) -> Result<usize, FsError> {
        self.truncate(inum)?;
        self.write(inum, 0, data)
    }

    pub fn stat(&self, inum: u16) -> Result<Stat, FsError> {
        let inode = read_inode(self.disk, &self.layout, inum)?;
        if inode.kind == T_FREE {
            return Err(FsError::Inval);
        }
        Ok(Stat {
            kind: inode.kind,
            size: inode.size,
        })
    }

    /// Filesystem totals and free counts. Returns
    /// `(total_blocks, free_blocks, total_inodes, free_inodes)`.
    pub fn statfs(&self) -> Result<(u32, u32, u32, u32), FsError> {
        let total_blocks = self.layout.total_blocks;
        let data_start = self.layout.data_start as usize;
        let data_bits = (total_blocks as usize).saturating_sub(data_start);

        let dbitmap = self.disk.read_block(2).ok_or(FsError::Corrupt)?;
        let free_blocks = (0..data_bits)
            .filter(|&bit| !bit_test(dbitmap, bit))
            .count() as u32;

        let total_inodes = u32::from(NINODES);
        let ibitmap = self.disk.read_block(1).ok_or(FsError::Corrupt)?;
        // Bit 0 is reserved; bits 1..NINODES map to usable inodes.
        let free_inodes = (1..NINODES as usize)
            .filter(|&bit| !bit_test(ibitmap, bit))
            .count() as u32;

        Ok((total_blocks, free_blocks, total_inodes, free_inodes))
    }

    /// Number of disk blocks a file or directory occupies (data blocks plus
    /// the indirect pointer block if present). For `du`.
    pub fn path_blocks(&self, path: &str) -> Result<u32, FsError> {
        let inum = self.namei(path)?.ok_or(FsError::NotFound)?;
        let inode = read_inode(self.disk, &self.layout, inum)?;
        if inode.kind == T_FREE {
            return Err(FsError::Inval);
        }
        let mut blocks = inode.direct.iter().filter(|&&b| b != 0).count() as u32;
        if inode.indirect != 0 {
            blocks += 1;
            let buf = self
                .disk
                .read_block(inode.indirect as usize)
                .ok_or(FsError::Corrupt)?;
            blocks += (0..INDIRECT_ENTRIES)
                .filter(|&i| u16::from_le_bytes([buf[i * 2], buf[i * 2 + 1]]) != 0)
                .count() as u32;
        }
        Ok(blocks)
    }

    pub fn kind(&self, inum: u16) -> Result<u8, FsError> {
        Ok(read_inode(self.disk, &self.layout, inum)?.kind)
    }

    /// Look up `name` in directory `dir`. Returns the child inum.
    pub fn dirlookup(&self, dir: u16, name: &str) -> Result<Option<u16>, FsError> {
        let inode = read_inode(self.disk, &self.layout, dir)?;
        if inode.kind != T_DIR {
            return Err(FsError::NotDir);
        }
        let mut buf = vec![0u8; inode.size as usize];
        let read = self.read_raw(&inode, 0, &mut buf)?;
        buf.truncate(read);
        for chunk in buf.chunks(DIRENT_SIZE) {
            if chunk.len() < DIRENT_SIZE {
                break;
            }
            let dent = Dirent::decode(chunk);
            if dent.inum != 0 && dent.name_str() == name {
                return Ok(Some(dent.inum));
            }
        }
        Ok(None)
    }

    /// Reverse lookup: the name of `inum` within directory `dir`.
    fn name_in(&self, dir: u16, inum: u16) -> Result<Option<String>, FsError> {
        let inode = read_inode(self.disk, &self.layout, dir)?;
        if inode.kind != T_DIR {
            return Err(FsError::NotDir);
        }
        let mut buf = vec![0u8; inode.size as usize];
        let read = self.read_raw(&inode, 0, &mut buf)?;
        buf.truncate(read);
        for chunk in buf.chunks(DIRENT_SIZE) {
            if chunk.len() < DIRENT_SIZE {
                break;
            }
            let dent = Dirent::decode(chunk);
            if dent.inum == inum {
                return Ok(Some(dent.name_str().to_string()));
            }
        }
        Ok(None)
    }

    /// Absolute path of an inode, resolved by walking up via `".."` to the
    /// root. The root is `"/"`.
    pub fn path_of(&self, inum: u16) -> Result<String, FsError> {
        if inum == ROOT_INO {
            return Ok("/".to_string());
        }
        let mut parts: Vec<String> = Vec::new();
        let mut cur = inum;
        while cur != ROOT_INO {
            let parent = self.dirlookup(cur, "..")?.ok_or(FsError::Corrupt)?;
            let name = self.name_in(parent, cur)?.ok_or(FsError::Corrupt)?;
            parts.push(name);
            cur = parent;
        }
        parts.reverse();
        Ok(format!("/{}", parts.join("/")))
    }

    /// Read helper that works from a raw inode .
    fn read_raw(&self, inode: &DiskInode, offset: u32, buf: &mut [u8]) -> Result<usize, FsError> {
        if offset >= inode.size {
            return Ok(0);
        }
        let end = (offset as usize + buf.len()).min(inode.size as usize);
        let mut done = 0usize;
        let mut pos = offset as usize;
        while pos < end {
            let file_block = (pos / BLOCK_SIZE) as u32;
            let block = self.map_read(inode, file_block)?;
            let sec = self
                .disk
                .read_block(block as usize)
                .ok_or(FsError::Corrupt)?;
            let start = pos % BLOCK_SIZE;
            let take = (BLOCK_SIZE - start).min(end - pos);
            buf[done..done + take].copy_from_slice(&sec[start..start + take]);
            done += take;
            pos += take;
        }
        Ok(done)
    }

    fn map_read(&self, inode: &DiskInode, file_block: u32) -> Result<u16, FsError> {
        if file_block < NDIRECT as u32 {
            let block = inode.direct[file_block as usize];
            if block == 0 {
                return Err(FsError::Corrupt);
            }
            return Ok(block);
        }
        let idx = file_block as usize - NDIRECT;
        if inode.indirect == 0 {
            return Err(FsError::Corrupt);
        }
        let buf = self
            .disk
            .read_block(inode.indirect as usize)
            .ok_or(FsError::Corrupt)?;
        let entry = u16::from_le_bytes([buf[idx * 2], buf[idx * 2 + 1]]);
        if entry == 0 {
            return Err(FsError::Corrupt);
        }
        Ok(entry)
    }

    /// Resolve a full path to an inode number. Returns `Ok(None)` when the
    /// final component does not exist.
    pub fn namei(&self, path: &str) -> Result<Option<u16>, FsError> {
        self.namei_from(ROOT_INO, path)
    }

    /// Resolve a path relative to a starting directory. Absolute paths (`/…`)
    /// resolve from the root; relative paths resolve from `start`.
    pub fn namei_from(&self, start: u16, path: &str) -> Result<Option<u16>, FsError> {
        let path = path.trim();
        let mut inum = if path.starts_with('/') {
            ROOT_INO
        } else {
            start
        };
        for comp in path.split('/').filter(|c| !c.is_empty()) {
            let Some(next) = self.dirlookup(inum, comp)? else {
                return Ok(None);
            };
            inum = next;
        }
        Ok(Some(inum))
    }

    /// Resolve a path to its parent directory and the final component.
    fn nameiparent(&self, path: &str) -> Result<(u16, String), FsError> {
        self.nameiparent_from(ROOT_INO, path)
    }

    /// Like [`Self::nameiparent`] but resolves relative paths from `start`.
    fn nameiparent_from(&self, start: u16, path: &str) -> Result<(u16, String), FsError> {
        let path = path.trim();
        let (parent_path, name) = match path.rsplit_once('/') {
            Some((parent, name)) => (parent, name.to_string()),
            None => ("", path.to_string()),
        };
        if name.is_empty() || name == "." || name == ".." || name.len() > DIRSIZ {
            return Err(FsError::Inval);
        }
        let parent = if parent_path.is_empty() {
            start
        } else {
            self.namei(parent_path)?.ok_or(FsError::NotFound)?
        };
        if self.kind(parent)? != T_DIR {
            return Err(FsError::NotDir);
        }
        Ok((parent, name))
    }

    /// Create `path` as a file or directory. Fails if it exists.
    pub fn create(&mut self, path: &str, kind: u8) -> Result<u16, FsError> {
        self.create_from(ROOT_INO, path, kind)
    }

    /// Like [`Self::create`] but resolves relative paths from `start`.
    pub fn create_from(&mut self, start: u16, path: &str, kind: u8) -> Result<u16, FsError> {
        let (parent, name) = self.nameiparent_from(start, path)?;
        if self.dirlookup(parent, &name)?.is_some() {
            return Err(FsError::Exist);
        }
        let inum = self.ialloc(kind)?;
        if kind == T_DIR {
            // `.` and `..`; bump the parent's nlink.
            let mut block = vec![0u8; BLOCK_SIZE];
            Dirent::new(inum, ".").encode(&mut block[0..DIRENT_SIZE]);
            Dirent::new(parent, "..").encode(&mut block[DIRENT_SIZE..2 * DIRENT_SIZE]);
            self.write(inum, 0, &block[..2 * DIRENT_SIZE])?;
            let (mut pinode, _) = self.inode_mut(parent)?;
            pinode.nlink += 1;
            self.inode_write(parent, &pinode)?;
        }
        self.dirent_add(parent, &name, inum)?;
        Ok(inum)
    }

    fn dirent_add(&mut self, dir: u16, name: &str, inum: u16) -> Result<(), FsError> {
        let (inode, _) = self.inode_mut(dir)?;
        if inode.kind != T_DIR {
            return Err(FsError::NotDir);
        }
        // Reuse a free slot if present, else append.
        let mut buf = vec![0u8; inode.size as usize];
        let read = self.read_raw(&inode, 0, &mut buf)?;
        buf.truncate(read);
        let mut slot = None;
        for (i, chunk) in buf.chunks(DIRENT_SIZE).enumerate() {
            if chunk.len() < DIRENT_SIZE {
                break;
            }
            let dent = Dirent::decode(chunk);
            if dent.inum == 0 {
                slot = Some(i);
                break;
            }
        }
        let entry = Dirent::new(inum, name);
        let mut encoded = vec![0u8; DIRENT_SIZE];
        entry.encode(&mut encoded);
        match slot {
            Some(i) => {
                self.write(dir, (i * DIRENT_SIZE) as u32, &encoded)?;
            }
            None => {
                if inode.size as usize + DIRENT_SIZE > MAX_FILE as usize {
                    return Err(FsError::NoSpace);
                }
                self.write(dir, inode.size, &encoded)?;
            }
        }
        Ok(())
    }

    /// Remove the directory entry `path` and drop a link on its inode.
    pub fn unlink(&mut self, path: &str) -> Result<(), FsError> {
        let (parent, name) = self.nameiparent(path)?;
        let inum = self.dirlookup(parent, &name)?.ok_or(FsError::NotFound)?;
        let (inode, _) = self.inode_mut(inum)?;
        if inode.kind == T_DIR {
            // Only empty directories (`.` and `..` only) may be unlinked.
            let mut buf = vec![0u8; inode.size as usize];
            let read = self.read_raw(&inode, 0, &mut buf)?;
            buf.truncate(read);
            let entries = buf
                .chunks(DIRENT_SIZE)
                .filter(|c| c.len() == DIRENT_SIZE)
                .map(Dirent::decode)
                .filter(|d| d.inum != 0 && d.name_str() != "." && d.name_str() != "..")
                .count();
            if entries > 0 {
                return Err(FsError::Exist);
            }
        }

        // Blank the matching dirent.
        let blank = vec![0u8; DIRENT_SIZE];
        let (pinode, _) = self.inode_mut(parent)?;
        let mut pbuf = vec![0u8; pinode.size as usize];
        let pread = self.read_raw(&pinode, 0, &mut pbuf)?;
        pbuf.truncate(pread);
        for (i, chunk) in pbuf.chunks(DIRENT_SIZE).enumerate() {
            if chunk.len() < DIRENT_SIZE {
                break;
            }
            let dent = Dirent::decode(chunk);
            if dent.inum == inum && dent.name_str() == name {
                self.write(parent, (i * DIRENT_SIZE) as u32, &blank)?;
                break;
            }
        }

        let (mut inode, _) = self.inode_mut(inum)?;
        inode.nlink -= 1;
        if inode.nlink == 0 {
            self.inode_blocks_free(&inode)?;
            let free = DiskInode::free();
            self.inode_write(inum, &free)?;
        } else {
            self.inode_write(inum, &inode)?;
            if inode.kind == T_DIR {
                let (mut pinode, _) = self.inode_mut(parent)?;
                pinode.nlink -= 1;
                self.inode_write(parent, &pinode)?;
            }
        }
        Ok(())
    }

    /// Create `path` if it does not exist (the `touch` primitive).
    pub fn touch(&mut self, path: &str) -> Result<u16, FsError> {
        self.touch_from(ROOT_INO, path)
    }

    /// Like [`Self::touch`] but resolves relative paths from `start`.
    pub fn touch_from(&mut self, start: u16, path: &str) -> Result<u16, FsError> {
        match self.namei_from(start, path)? {
            Some(inum) => Ok(inum),
            None => self.create_from(start, path, T_FILE),
        }
    }

    /// Free data blocks and the inode — used when nlink hits zero via unlink.
    pub fn free_inode(&mut self, inum: u16) -> Result<(), FsError> {
        let (inode, _) = self.inode_mut(inum)?;
        self.inode_blocks_free(&inode)?;
        let free = DiskInode::free();
        self.inode_write(inum, &free)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_fs<T>(f: impl FnOnce(&mut Vfs) -> T) -> T {
        let mut disk = Disk::new(256, BLOCK_SIZE);
        let mut vfs = Vfs::format_if_needed(&mut disk).unwrap();
        f(&mut vfs)
    }

    #[test]
    fn format_and_mount_root() {
        with_fs(|vfs| {
            let root = vfs.namei("/").unwrap().unwrap();
            assert_eq!(root, ROOT_INO);
            assert_eq!(vfs.kind(root).unwrap(), T_DIR);
        });
    }

    #[test]
    fn create_write_read_file() {
        with_fs(|vfs| {
            let inum = vfs.create("/hello", T_FILE).unwrap();
            assert_eq!(vfs.write(inum, 0, b"hello, disk").unwrap(), 11);
            let mut buf = vec![0u8; 32];
            let n = vfs.read(inum, 0, &mut buf).unwrap();
            assert_eq!(&buf[..n], b"hello, disk");
            assert_eq!(vfs.stat(inum).unwrap().size, 11);
        });
    }

    #[test]
    fn lookup_by_path() {
        with_fs(|vfs| {
            let inum = vfs.create("/notes", T_FILE).unwrap();
            assert_eq!(vfs.namei("/notes").unwrap(), Some(inum));
            assert!(vfs.namei("/missing").unwrap().is_none());
        });
    }

    #[test]
    fn nested_directories() {
        with_fs(|vfs| {
            vfs.create("/home", T_DIR).unwrap();
            vfs.create("/home/user", T_DIR).unwrap();
            let file = vfs.create("/home/user/essay.txt", T_FILE).unwrap();
            vfs.write(file, 0, b"typed on a simulated disk").unwrap();
            assert_eq!(vfs.namei("/home/user/essay.txt").unwrap(), Some(file));
            assert_eq!(
                vfs.kind(vfs.namei("/home/user").unwrap().unwrap()).unwrap(),
                T_DIR
            );
        });
    }

    #[test]
    fn unlink_removes_file() {
        with_fs(|vfs| {
            let inum = vfs.create("/tmpfile", T_FILE).unwrap();
            vfs.write(inum, 0, b"bye").unwrap();
            vfs.unlink("/tmpfile").unwrap();
            assert!(vfs.namei("/tmpfile").unwrap().is_none());
        });
    }

    #[test]
    fn mkdir_and_touch() {
        with_fs(|vfs| {
            vfs.create("/work", T_DIR).unwrap();
            let a = vfs.touch("/work/a").unwrap();
            let b = vfs.touch("/work/b").unwrap();
            assert_ne!(a, b);
            // touch on an existing file is a no-op success.
            assert_eq!(vfs.touch("/work/a").unwrap(), a);
        });
    }

    #[test]
    fn files_spanning_multiple_blocks() {
        with_fs(|vfs| {
            let inum = vfs.create("/big", T_FILE).unwrap();
            let payload: Vec<u8> = (0..3000u32).map(|i| (i % 251) as u8).collect();
            assert_eq!(vfs.write(inum, 0, &payload).unwrap(), 3000);
            let mut buf = vec![0u8; 3000];
            let n = vfs.read(inum, 0, &mut buf).unwrap();
            assert_eq!(n, 3000);
            assert_eq!(buf, payload);
            // Partial read in the middle crosses a block boundary.
            let mut mid = vec![0u8; 100];
            let n = vfs.read(inum, 500, &mut mid).unwrap();
            assert_eq!(n, 100);
            assert_eq!(mid, payload[500..600]);
        });
    }

    #[test]
    fn truncate_frees_and_resets() {
        with_fs(|vfs| {
            let inum = vfs.create("/t", T_FILE).unwrap();
            vfs.write(inum, 0, &[b'x'; 4096]).unwrap();
            assert_eq!(vfs.stat(inum).unwrap().size, 4096);
            vfs.truncate(inum).unwrap();
            assert_eq!(vfs.stat(inum).unwrap().size, 0);
            let mut buf = [0u8; 8];
            assert_eq!(vfs.read(inum, 0, &mut buf).unwrap(), 0);
        });
    }

    #[test]
    fn indirect_blocks_for_large_files() {
        with_fs(|vfs| {
            let inum = vfs.create("/huge", T_FILE).unwrap();
            // Larger than NDIRECT * BLOCK_SIZE (12 * 512 = 6144).
            let size = 10_000usize;
            let payload: Vec<u8> = (0..size).map(|i| (i / 7) as u8).collect();
            assert_eq!(vfs.write(inum, 0, &payload).unwrap(), size);
            let mut buf = vec![0u8; size];
            assert_eq!(vfs.read(inum, 0, &mut buf).unwrap(), size);
            assert_eq!(buf, payload);
        });
    }

    #[test]
    fn rmdir_only_when_empty() {
        with_fs(|vfs| {
            vfs.create("/d", T_DIR).unwrap();
            vfs.create("/d/file", T_FILE).unwrap();
            assert_eq!(vfs.unlink("/d"), Err(FsError::Exist));
            vfs.unlink("/d/file").unwrap();
            vfs.unlink("/d").unwrap();
            assert!(vfs.namei("/d").unwrap().is_none());
        });
    }

    #[test]
    fn namei_resolves_relative_to_start() {
        with_fs(|vfs| {
            vfs.create("/home", T_DIR).unwrap();
            let user = vfs.create("/home/user", T_DIR).unwrap();
            let file = vfs.create("/home/user/note", T_FILE).unwrap();
            // Absolute always resolves from root.
            assert_eq!(vfs.namei_from(user, "/home/user/note").unwrap(), Some(file));
            // Relative resolves from `start`.
            assert_eq!(vfs.namei_from(user, "note").unwrap(), Some(file));
            assert_eq!(vfs.namei_from(user, ".").unwrap(), Some(user));
            assert_eq!(
                vfs.namei_from(user, "..").unwrap(),
                Some(vfs.namei("/home").unwrap().unwrap())
            );
            assert!(vfs.namei_from(user, "missing").unwrap().is_none());
        });
    }

    #[test]
    fn path_of_resolves_absolute_paths() {
        with_fs(|vfs| {
            assert_eq!(vfs.path_of(ROOT_INO).unwrap(), "/");
            vfs.create("/home", T_DIR).unwrap();
            let user = vfs.create("/home/user", T_DIR).unwrap();
            vfs.create("/home/user/docs", T_DIR).unwrap();
            let docs = vfs.namei("/home/user/docs").unwrap().unwrap();
            assert_eq!(vfs.path_of(user).unwrap(), "/home/user");
            assert_eq!(vfs.path_of(docs).unwrap(), "/home/user/docs");
        });
    }

    #[test]
    fn rejects_duplicate_create() {
        with_fs(|vfs| {
            vfs.create("/x", T_FILE).unwrap();
            assert_eq!(vfs.create("/x", T_FILE), Err(FsError::Exist));
        });
    }
}
