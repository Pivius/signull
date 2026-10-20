//! Link and network addresses.

use std::fmt;

/// 48-bit MAC address
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct MacAddr(pub [u8; 6]);

impl MacAddr {
    pub const BROADCAST: MacAddr = MacAddr([0xff; 6]);
    pub const ZERO: MacAddr = MacAddr([0; 6]);

    pub const fn new(octets: [u8; 6]) -> Self {
        Self(octets)
    }

    pub fn octets(&self) -> [u8; 6] {
        self.0
    }
}

impl fmt::Display for MacAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let o = self.0;
        write!(
            f,
            "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            o[0], o[1], o[2], o[3], o[4], o[5]
        )
    }
}

impl fmt::Debug for MacAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MacAddr({self})")
    }
}

/// 32-bit IPv4 address
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Ipv4Addr(pub [u8; 4]);

impl Ipv4Addr {
    pub const UNSPECIFIED: Ipv4Addr = Ipv4Addr([0, 0, 0, 0]);
    pub const BROADCAST: Ipv4Addr = Ipv4Addr([255, 255, 255, 255]);

    pub const fn new(a: u8, b: u8, c: u8, d: u8) -> Self {
        Self([a, b, c, d])
    }

    pub fn octets(&self) -> [u8; 4] {
        self.0
    }

    pub fn parse(s: &str) -> Option<Self> {
        let mut octets = [0u8; 4];
        let mut parts = 0;

        for (i, part) in s.split('.').enumerate() {
            if i >= 4 {
                return None;
            }

            let value: u16 = part.parse().ok()?;
            if value > 255 {
                return None;
            }

            octets[i] = value as u8;
            parts += 1;
        }

        if parts == 4 { Some(Self(octets)) } else { None }
    }
}

impl fmt::Display for Ipv4Addr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let o = self.0;
        write!(f, "{}.{}.{}.{}", o[0], o[1], o[2], o[3])
    }
}

impl fmt::Debug for Ipv4Addr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Ipv4Addr({self})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_format_ipv4() {
        let addr = Ipv4Addr::parse("192.168.1.20").unwrap();
        assert_eq!(addr.octets(), [192, 168, 1, 20]);
        assert_eq!(addr.to_string(), "192.168.1.20");
    }

    #[test]
    fn reject_bad_ipv4() {
        assert!(Ipv4Addr::parse("256.0.0.1").is_none());
        assert!(Ipv4Addr::parse("10.0.0").is_none());
        assert!(Ipv4Addr::parse("10.0.0.1.2").is_none());
        assert!(Ipv4Addr::parse("hello").is_none());
    }

    #[test]
    fn format_mac() {
        assert_eq!(
            MacAddr::new([0xaa, 0, 1, 2, 3, 0xff]).to_string(),
            "aa:00:01:02:03:ff"
        );
    }
}
