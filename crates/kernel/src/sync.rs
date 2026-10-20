//! Sleep/wakeup synchronisation primitives.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Chan(pub u64);

impl Chan {
    /// Buffered console input became available.
    pub const CONSOLE: Chan = Chan(1);
    /// A child process exited (used by `wait`).
    pub const WAIT: Chan = Chan(2);
    /// Timed sleepers.
    pub const SLEEP: Chan = Chan(3);
    /// A pending disk request completed.
    pub const DISK: Chan = Chan(4);
}
