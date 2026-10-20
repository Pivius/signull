use crate::{addr::Ipv4Addr, checksum::internet_checksum, ipv4::IPPROTO_TCP};

pub const TCP_MIN_HEADER_LEN: usize = 20;

/// TCP control flag bits
pub mod flags {
    pub const FIN: u8 = 0x01;
    pub const SYN: u8 = 0x02;
    pub const RST: u8 = 0x04;
    pub const PSH: u8 = 0x08;
    pub const ACK: u8 = 0x10;
    pub const URG: u8 = 0x20;
    pub const ECE: u8 = 0x40;
    pub const CWR: u8 = 0x80;

    pub fn describe(byte: u8) -> String {
        let mut names = Vec::new();
        for (bit, name) in [
            (FIN, "FIN"),
            (SYN, "SYN"),
            (RST, "RST"),
            (PSH, "PSH"),
            (ACK, "ACK"),
            (URG, "URG"),
            (ECE, "ECE"),
            (CWR, "CWR"),
        ] {
            if byte & bit != 0 {
                names.push(name);
            }
        }

        names.join(" ")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TcpHeader {
    pub src_port: u16,
    pub dst_port: u16,
    pub seq: u32,
    pub ack: u32,
    pub data_offset: u8,
    pub flags: u8,
    pub window: u16,
    pub checksum: u16,
    pub urgent: u16,
}

impl TcpHeader {
    pub fn new(src_port: u16, dst_port: u16, seq: u32, ack: u32, flags: u8) -> Self {
        Self {
            src_port,
            dst_port,
            seq,
            ack,
            data_offset: 5,
            flags,
            window: 65535,
            checksum: 0,
            urgent: 0,
        }
    }

    pub fn header_len(&self) -> usize {
        self.data_offset as usize * 4
    }

    pub fn parse(bytes: &[u8]) -> Option<(Self, &[u8])> {
        if bytes.len() < TCP_MIN_HEADER_LEN {
            return None;
        }

        let data_offset = bytes[12] >> 4;
        if data_offset < 5 {
            return None;
        }

        let header_len = data_offset as usize * 4;
        if bytes.len() < header_len {
            return None;
        }

        let header = Self {
            src_port: u16::from_be_bytes([bytes[0], bytes[1]]),
            dst_port: u16::from_be_bytes([bytes[2], bytes[3]]),
            seq: u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            ack: u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
            data_offset,
            flags: bytes[13],
            window: u16::from_be_bytes([bytes[14], bytes[15]]),
            checksum: u16::from_be_bytes([bytes[16], bytes[17]]),
            urgent: u16::from_be_bytes([bytes[18], bytes[19]]),
        };
        Some((header, &bytes[header_len..]))
    }

    pub fn serialize_into(&self, payload: &[u8], src: Ipv4Addr, dst: Ipv4Addr, out: &mut Vec<u8>) {
        let start = out.len();
        out.extend_from_slice(&self.src_port.to_be_bytes());
        out.extend_from_slice(&self.dst_port.to_be_bytes());
        out.extend_from_slice(&self.seq.to_be_bytes());
        out.extend_from_slice(&self.ack.to_be_bytes());
        out.push((self.data_offset << 4) & 0xf0);
        out.push(self.flags);
        out.extend_from_slice(&self.window.to_be_bytes());
        out.extend_from_slice(&[0, 0]); // checksum placeholder
        out.extend_from_slice(&self.urgent.to_be_bytes());
        out.extend_from_slice(payload);

        let checksum = tcp_checksum(src, dst, &out[start..]);
        out[start + 16] = (checksum >> 8) as u8;
        out[start + 17] = (checksum & 0xff) as u8;
    }
}

pub fn tcp_checksum(src: Ipv4Addr, dst: Ipv4Addr, segment: &[u8]) -> u16 {
    let mut buf = Vec::with_capacity(12 + segment.len());
    buf.extend_from_slice(&src.0);
    buf.extend_from_slice(&dst.0);
    buf.push(0);
    buf.push(IPPROTO_TCP);
    buf.extend_from_slice(&(segment.len() as u16).to_be_bytes());
    buf.extend_from_slice(segment);
    internet_checksum(&buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_segment() {
        let src = Ipv4Addr::new(10, 0, 1, 10);
        let dst = Ipv4Addr::new(10, 0, 2, 20);
        let header = TcpHeader::new(49152, 80, 1000, 2000, flags::PSH | flags::ACK);
        let payload = b"GET / HTTP/1.0\r\n\r\n";
        let mut bytes = Vec::new();
        header.serialize_into(payload, src, dst, &mut bytes);

        let (parsed, rest) = TcpHeader::parse(&bytes).unwrap();
        assert_eq!(parsed.src_port, 49152);
        assert_eq!(parsed.dst_port, 80);
        assert_eq!(parsed.seq, 1000);
        assert_eq!(parsed.ack, 2000);
        assert_eq!(parsed.flags, flags::PSH | flags::ACK);
        assert_eq!(rest, payload);
    }

    #[test]
    fn checksum_verifies() {
        let src = Ipv4Addr::new(192, 168, 1, 10);
        let dst = Ipv4Addr::new(192, 168, 1, 20);
        let header = TcpHeader::new(1234, 443, 42, 7, flags::SYN);
        let mut bytes = Vec::new();
        header.serialize_into(b"hello", src, dst, &mut bytes);
        assert_eq!(tcp_checksum(src, dst, &bytes), 0);
    }

    #[test]
    fn describe_flags() {
        assert_eq!(flags::describe(flags::SYN | flags::ACK), "SYN ACK");
        assert_eq!(flags::describe(0), "");
    }
}
