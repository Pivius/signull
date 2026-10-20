//! The standard userland delivered with the kernel.
//!
//! These are the reference `sinu` programs that ship on every machine's disk.
//! The front-end installs them at boot; the kernel's tests run them directly.

/// The userland programs: (name, `sinu` source). Installed to `/bin/<name>`.
pub const USERLAND: &[(&str, &str)] = &[
    ("sh", include_str!("../progs/sh.sn")),
    ("echo", include_str!("../progs/echo.sn")),
    ("touch", include_str!("../progs/touch.sn")),
    ("cat", include_str!("../progs/cat.sn")),
    ("ls", include_str!("../progs/ls.sn")),
    ("rm", include_str!("../progs/rm.sn")),
    ("mkdir", include_str!("../progs/mkdir.sn")),
    ("nano", include_str!("../progs/nano.sn")),
    ("pwd", include_str!("../progs/pwd.sn")),
    ("cp", include_str!("../progs/cp.sn")),
    ("mv", include_str!("../progs/mv.sn")),
    ("clear", include_str!("../progs/clear.sn")),
    ("grep", include_str!("../progs/grep.sn")),
    ("find", include_str!("../progs/find.sn")),
    ("head", include_str!("../progs/head.sn")),
    ("tail", include_str!("../progs/tail.sn")),
    ("whoami", include_str!("../progs/whoami.sn")),
    ("id", include_str!("../progs/id.sn")),
    ("ps", include_str!("../progs/ps.sn")),
    ("kill", include_str!("../progs/kill.sn")),
    ("df", include_str!("../progs/df.sn")),
    ("du", include_str!("../progs/du.sn")),
    ("dmesg", include_str!("../progs/dmesg.sn")),
    ("uptime", include_str!("../progs/uptime.sn")),
    ("date", include_str!("../progs/date.sn")),
    ("uname", include_str!("../progs/uname.sn")),
];
