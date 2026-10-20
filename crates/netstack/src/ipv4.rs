//! IPv4 packet header

use crate::{addr::Ipv4Addr, checksum::internet_checksum};

pub const IPV4_MIN_HEADER_LEN: usize = 20;
pub const IPPROTO_TCP: u8 = 6;
pub const IPPROTO_UDP: u8 = 17;

pub const IPV4_FLAG_DF: u8 = 0x2;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ipv4Header {
    pub version: u8,
    pub ihl: u8,
    pub dscp: u8,
    pub total_length: u16,
    pub identification: u16,
    pub flags: u8,
    pub fragment_offset: u16,
    pub ttl: u8,
    pub protocol: u8,
    pub checksum: u16,
    pub src: Ipv4Addr,
    pub dst: Ipv4Addr,
}

impl Ipv4Header {
    pub fn new(src: Ipv4Addr, dst: Ipv4Addr, protocol: u8, payload_len: usize) -> Self {
        Self {
            version: 4,
            ihl: 5,
            dscp: 0,
            total_length: (IPV4_MIN_HEADER_LEN + payload_len) as u16,
            identification: 0,
            flags: IPV4_FLAG_DF,
            fragment_offset: 0,
            ttl: 64,
            protocol,
            checksum: 0,
            src,
            dst,
        }
    }

    pub fn header_len(&self) -> usize {
        self.ihl as usize * 4
    }

    pub fn parse(bytes: &[u8]) -> Option<(Self, &[u8])> {
        if bytes.len() < IPV4_MIN_HEADER_LEN {
            return None;
        }

        let version = bytes[0] >> 4;
        let ihl = bytes[0] & 0x0f;
        if version != 4 || ihl < 5 {
            return None;
        }

        let header_len = ihl as usize * 4;
        if bytes.len() < header_len {
            return None;
        }

        let total_length = u16::from_be_bytes([bytes[2], bytes[3]]);
        let fragment_offset = ((bytes[6] as u16 & 0x1f) << 8) | bytes[7] as u16;
        let header = Self {
            version,
            ihl,
            dscp: bytes[1],
            total_length,
            identification: u16::from_be_bytes([bytes[4], bytes[5]]),
            flags: bytes[6] >> 5,
            fragment_offset,
            ttl: bytes[8],
            protocol: bytes[9],
            checksum: u16::from_be_bytes([bytes[10], bytes[11]]),
            src: Ipv4Addr(bytes[12..16].try_into().ok()?),
            dst: Ipv4Addr(bytes[16..20].try_into().ok()?),
        };

        let available = bytes.len() - header_len;
        let payload_len = if total_length as usize >= header_len {
            (total_length as usize - header_len).min(available)
        } else {
            available
        };
        Some((header, &bytes[header_len..header_len + payload_len]))
    }

    pub fn serialize_into(&self, payload: &[u8], out: &mut Vec<u8>) {
        let start = out.len();
        let total_length = IPV4_MIN_HEADER_LEN + payload.len();

        out.push((4 << 4) | 5);
        out.push(self.dscp);
        out.extend_from_slice(&(total_length as u16).to_be_bytes());
        out.extend_from_slice(&self.identification.to_be_bytes());
        out.push((self.flags << 5) | ((self.fragment_offset >> 8) as u8 & 0x1f));
        out.push((self.fragment_offset & 0xff) as u8);
        out.push(self.ttl);
        out.push(self.protocol);
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(&self.src.0);
        out.extend_from_slice(&self.dst.0);

        let checksum = internet_checksum(&out[start..start + IPV4_MIN_HEADER_LEN]);
        out[start + 10] = (checksum >> 8) as u8;
        out[start + 11] = (checksum & 0xff) as u8;

        out.extend_from_slice(payload);
    }

    pub fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(IPV4_MIN_HEADER_LEN);
        self.serialize_into(&[], &mut out);
        out
    }
}

pub fn verify_checksum(header_bytes: &[u8]) -> bool {
    crate::checksum::verify(header_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_survive_round_trip() {
        let header = Ipv4Header::new(
            Ipv4Addr::new(10, 0, 1, 10),
            Ipv4Addr::new(10, 0, 2, 20),
            IPPROTO_TCP,
            8,
        );
        let payload = [1, 2, 3, 4, 5, 6, 7, 8];
        let mut bytes = Vec::new();
        header.serialize_into(&payload, &mut bytes);
        let (parsed, rest) = Ipv4Header::parse(&bytes).unwrap();
        assert_eq!(parsed.ttl, 64);
        assert_eq!(parsed.flags, IPV4_FLAG_DF);
        assert_eq!(parsed.fragment_offset, 0);
        assert_eq!(rest, &payload);
    }

    #[test]
    fn parse_serialised_header_and_payload() {
        let header = Ipv4Header::new(
            Ipv4Addr::new(192, 168, 1, 10),
            Ipv4Addr::new(192, 168, 1, 20),
            IPPROTO_TCP,
            4,
        );
        let payload = [0xde, 0xad, 0xbe, 0xef];
        let mut bytes = Vec::new();
        header.serialize_into(&payload, &mut bytes);

        assert_eq!(bytes.len(), IPV4_MIN_HEADER_LEN + 4);
        assert!(verify_checksum(&bytes[..IPV4_MIN_HEADER_LEN]));

        let (parsed, rest) = Ipv4Header::parse(&bytes).unwrap();
        assert_eq!(parsed.src, header.src);
        assert_eq!(parsed.dst, header.dst);
        assert_eq!(parsed.protocol, IPPROTO_TCP);
        assert_eq!(parsed.total_length, 24);
        assert_eq!(rest, &payload);
    }

    #[test]
    fn rejects_non_ipv4() {
        let mut bytes = vec![0u8; 20];
        bytes[0] = 0x60; // version 6
        assert!(Ipv4Header::parse(&bytes).is_none());
    }
}
