//! File descriptor and open file table.

pub type FileId = usize;
pub type Fd = i32;

pub const STDIN: Fd = 0;
pub const STDOUT: Fd = 1;
pub const STDERR: Fd = 2;

pub const FILE_STDIN: FileId = 0;
pub const FILE_STDOUT: FileId = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PipeEnd {
    Read,
    Write,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum File {
    ConsoleIn,
    ConsoleOut,
    Inode {
        inum: u16,
        offset: u32,
        readable: bool,
        writable: bool,
    },
    Pipe {
        pipe: usize,
        end: PipeEnd,
    },
}

#[derive(Default)]
pub struct FileTable {
    slots: Vec<Option<(File, usize)>>,
}

impl FileTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Table with the std streams opened.
    pub fn with_std() -> Self {
        let mut table = Self::new();
        let stdin = table.alloc(File::ConsoleIn);
        let stdout = table.alloc(File::ConsoleOut);

        if let Some(slot) = table.slots.get_mut(stdout) {
            slot.as_mut().unwrap().1 = 2;
        }

        debug_assert_eq!(stdin, FILE_STDIN);
        debug_assert_eq!(stdout, FILE_STDOUT);
        table
    }

    /// Allocate a new open file with refcount 1.
    pub fn alloc(&mut self, file: File) -> FileId {
        if let Some(index) = self.slots.iter().position(Option::is_none) {
            self.slots[index] = Some((file, 1));
            index
        } else {
            self.slots.push(Some((file, 1)));
            self.slots.len() - 1
        }
    }

    /// Get file by id if it's open.
    pub fn get(&self, id: FileId) -> Option<File> {
        self.slots
            .get(id)
            .and_then(|slot| slot.as_ref())
            .map(|f| f.0)
    }

    /// Mutable access to an open file.
    pub fn get_mut(&mut self, id: FileId) -> Option<&mut File> {
        self.slots
            .get_mut(id)
            .and_then(|slot| slot.as_mut())
            .map(|f| &mut f.0)
    }

    /// Replace an open file's handle, keeping its refcount.
    pub fn set(&mut self, id: FileId, file: File) -> bool {
        match self.slots.get_mut(id).and_then(Option::as_mut) {
            Some(slot) => {
                slot.0 = file;
                true
            }
            None => false,
        }
    }

    /// Bump the refcount of an open file
    pub fn retain(&mut self, id: FileId) -> bool {
        match self.slots.get_mut(id).and_then(Option::as_mut) {
            Some(slot) => {
                slot.1 += 1;
                true
            }
            None => false,
        }
    }

    /// Drop a reference. Returns true if the file was closed.
    pub fn release(&mut self, id: FileId) -> bool {
        let Some(slot) = self.slots.get_mut(id).and_then(Option::as_mut) else {
            return false;
        };

        slot.1 -= 1;

        if slot.1 == 0 {
            self.slots[id] = None;
            true
        } else {
            false
        }
    }

    pub fn open_count(&self) -> usize {
        self.slots.iter().filter(|slot| slot.is_some()).count()
    }
}

/// File descriptor table.
#[derive(Default)]
pub struct FdTable {
    entries: Vec<Option<FileId>>,
}

impl FdTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_std() -> Self {
        Self {
            entries: vec![Some(FILE_STDIN), Some(FILE_STDOUT), Some(FILE_STDOUT)],
        }
    }

    /// Allocate the lowest free descriptor.
    pub fn alloc(&mut self, file: FileId) -> Fd {
        if let Some(index) = self.entries.iter().position(Option::is_none) {
            self.entries[index] = Some(file);
            index as Fd
        } else {
            self.entries.push(Some(file));
            (self.entries.len() - 1) as Fd
        }
    }

    pub fn get(&self, fd: Fd) -> Option<FileId> {
        usize::try_from(fd)
            .ok()
            .and_then(|index| self.entries.get(index))
            .and_then(|slot| *slot)
    }

    /// Duplicate a descriptor to the lowest open slot.
    pub fn dup(&mut self, fd: Fd) -> Option<Fd> {
        let file = self.get(fd)?;
        Some(self.alloc(file))
    }

    /// Clone this table for a forked child, retaining each open file once per
    /// fd entry in the shared [`FileTable`].
    pub fn fork_into(&self, files: &mut FileTable) -> FdTable {
        let mut entries = Vec::with_capacity(self.entries.len());
        for entry in &self.entries {
            match entry {
                Some(id) => {
                    files.retain(*id);
                    entries.push(Some(*id));
                }
                None => entries.push(None),
            }
        }
        FdTable { entries }
    }

    /// Every open [`FileId`] in this table.
    pub fn file_ids(&self) -> impl Iterator<Item = FileId> + '_ {
        self.entries.iter().filter_map(|e| *e)
    }

    /// Place `fileid` at exactly `fd`, closing any occupant first. Returns the
    /// previous occupant's id.
    pub fn alloc_at(&mut self, fd: Fd, fileid: FileId) -> Option<FileId> {
        let index = usize::try_from(fd).ok()?;
        while self.entries.len() <= index {
            self.entries.push(None);
        }
        let prev = self.entries[index].take();
        self.entries[index] = Some(fileid);
        prev
    }

    pub fn close(&mut self, fd: Fd) -> Option<FileId> {
        let index = usize::try_from(fd).ok()?;
        let slot = self.entries.get_mut(index)?;

        slot.take()
    }

    /// Open descriptors.
    pub fn len(&self) -> usize {
        self.entries.iter().filter(|slot| slot.is_some()).count()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.iter().all(Option::is_none)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn std_streams() {
        let table = FileTable::with_std();
        assert_eq!(table.get(FILE_STDIN), Some(File::ConsoleIn));
        assert_eq!(table.get(FILE_STDOUT), Some(File::ConsoleOut));
        assert_eq!(table.open_count(), 2);

        let fds = FdTable::with_std();
        assert_eq!(fds.get(STDIN), Some(FILE_STDIN));
        assert_eq!(fds.get(STDOUT), Some(FILE_STDOUT));
        assert_eq!(fds.get(STDERR), Some(FILE_STDOUT));
        assert_eq!(fds.len(), 3);
    }

    #[test]
    fn open_close_refcounts() {
        let mut table = FileTable::with_std();
        let id = table.alloc(File::ConsoleOut);
        assert_eq!(table.open_count(), 3);
        assert!(table.release(id), "last reference closes the file");
        assert_eq!(table.get(id), None);
        assert_eq!(table.open_count(), 2);
    }

    #[test]
    fn dup() {
        let mut table = FileTable::with_std();
        table.retain(FILE_STDOUT);
        let mut fds = FdTable::with_std();
        let new_fd = fds.dup(STDOUT).unwrap();
        assert_eq!(new_fd, 3);
        assert_eq!(fds.get(new_fd), Some(FILE_STDOUT));
        assert_eq!(fds.len(), 4);
    }

    #[test]
    fn close_frees_the_lowest_slot() {
        let mut fds = FdTable::with_std();
        assert_eq!(fds.close(STDOUT), Some(FILE_STDOUT));
        assert_eq!(fds.get(STDOUT), None);
        assert_eq!(fds.alloc(FILE_STDIN), STDOUT, "lowest free fd is reused");
    }

    #[test]
    fn bad_descriptors() {
        let fds = FdTable::with_std();
        assert_eq!(fds.get(-1), None);
        assert_eq!(fds.get(99), None);
    }

    #[test]
    fn inode_file_roundtrip() {
        let mut table = FileTable::new();
        let id = table.alloc(File::Inode {
            inum: 3,
            offset: 0,
            readable: true,
            writable: false,
        });
        let File::Inode { inum, .. } = table.get(id).unwrap() else {
            panic!()
        };
        assert_eq!(inum, 3);
        assert!(table.set(
            id,
            File::Inode {
                inum: 3,
                offset: 10,
                readable: true,
                writable: false,
            }
        ));
        let File::Inode { offset, .. } = table.get(id).unwrap() else {
            panic!()
        };
        assert_eq!(offset, 10);
    }
}
