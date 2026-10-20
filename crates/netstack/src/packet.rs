//! Complete payload

use crate::{
    addr::{Ipv4Addr, MacAddr},
    ethernet::{ETHERTYPE_IPV4, EthernetFrame},
    ipv4::{IPPROTO_TCP, Ipv4Header},
    tcp::TcpHeader,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum L4 {
    Tcp(TcpHeader),
    Other(u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PayloadKind {
    Text,
    Binary,
    Encrypted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Packet {
    pub ethernet: EthernetFrame,
    pub ipv4: Ipv4Header,
    pub l4: L4,
    pub payload: Vec<u8>,
    pub kind: PayloadKind,
}

impl Packet {
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let (ethernet, ip_bytes) = EthernetFrame::parse(bytes)?;
        if ethernet.ethertype != ETHERTYPE_IPV4 {
            return None;
        }
        let (ipv4, ip_payload) = Ipv4Header::parse(ip_bytes)?;
        let (l4, payload) = match ipv4.protocol {
            IPPROTO_TCP => {
                let (tcp, rest) = TcpHeader::parse(ip_payload)?;
                (L4::Tcp(tcp), rest.to_vec())
            }
            other => (L4::Other(other), ip_payload.to_vec()),
        };
        let kind = classify(&payload);
        Some(Self {
            ethernet,
            ipv4,
            l4,
            payload,
            kind,
        })
    }

    pub fn tcp(
        src_mac: MacAddr,
        dst_mac: MacAddr,
        src: Ipv4Addr,
        dst: Ipv4Addr,
        tcp: TcpHeader,
        payload: &[u8],
    ) -> Self {
        let mut packet = Self {
            ethernet: EthernetFrame::new(dst_mac, src_mac, ETHERTYPE_IPV4),
            ipv4: Ipv4Header::new(src, dst, IPPROTO_TCP, 20 + payload.len()),
            l4: L4::Tcp(tcp),
            payload: payload.to_vec(),
            kind: classify(payload),
        };
        packet.kind = classify(&packet.payload);
        packet
    }

    pub fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.ethernet.serialize_into(&mut out);

        let l4_bytes = match &self.l4 {
            L4::Tcp(tcp) => {
                let mut segment = Vec::new();
                tcp.serialize_into(&self.payload, self.ipv4.src, self.ipv4.dst, &mut segment);
                segment
            }
            L4::Other(_) => self.payload.clone(),
        };
        self.ipv4.serialize_into(&l4_bytes, &mut out);
        out
    }

    pub fn len(&self) -> usize {
        14 + self.ipv4.header_len()
            + match &self.l4 {
                L4::Tcp(_) => self.payload.len() + 20,
                L4::Other(_) => self.payload.len(),
            }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn classify(payload: &[u8]) -> PayloadKind {
    if payload.is_empty() {
        PayloadKind::Binary
    } else if payload
        .iter()
        .all(|b| b.is_ascii_graphic() || b.is_ascii_whitespace())
    {
        PayloadKind::Text
    } else if payload.len() >= 2 && payload[0] == 0x16 && payload[1] == 0x03 {
        // Best-effort TLS-record flavour tag.
        PayloadKind::Encrypted
    } else {
        PayloadKind::Binary
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checksum::verify;
    use crate::tcp::flags;

    fn sample() -> Packet {
        let tcp = TcpHeader::new(49152, 80, 1000, 2000, flags::PSH | flags::ACK);
        Packet::tcp(
            MacAddr::new([0x02, 0, 0, 0, 0, 1]),
            MacAddr::new([0x02, 0, 0, 0, 0, 2]),
            Ipv4Addr::new(10, 0, 1, 10),
            Ipv4Addr::new(10, 0, 2, 20),
            tcp,
            b"GET / HTTP/1.0\r\n",
        )
    }

    #[test]
    fn full_round_trip() {
        let packet = sample();
        let bytes = packet.serialize();
        let parsed = Packet::parse(&bytes).unwrap();
        assert_eq!(parsed.serialize(), bytes, "re-serialisation is canonical");
        assert_eq!(parsed.ethernet, packet.ethernet);
        assert_eq!(parsed.ipv4.src, packet.ipv4.src);
        assert_eq!(parsed.ipv4.dst, packet.ipv4.dst);
        match (&parsed.l4, &packet.l4) {
            (L4::Tcp(a), L4::Tcp(b)) => {
                assert_eq!(
                    (a.src_port, a.dst_port, a.seq, a.ack, a.flags),
                    (b.src_port, b.dst_port, b.seq, b.ack, b.flags)
                );
            }
            _ => panic!("expected tcp both sides"),
        }
        assert_eq!(parsed.payload, packet.payload);
        assert_eq!(parsed.kind, packet.kind);
    }

    #[test]
    fn checksums_are_valid_after_serialise() {
        let packet = sample();
        let bytes = packet.serialize();
        assert!(verify(&bytes[14..34]), "ipv4 checksum");
        assert!(
            TcpHeader::parse(&bytes[34..]).is_some(),
            "tcp header parses"
        );
        assert_eq!(tcp_checksum_for(&packet, &bytes[34..]), 0, "tcp checksum");
    }

    fn tcp_checksum_for(packet: &Packet, segment: &[u8]) -> u16 {
        crate::tcp::tcp_checksum(packet.ipv4.src, packet.ipv4.dst, segment)
    }

    #[test]
    fn payload_classification() {
        assert_eq!(classify(b"hello world").clone(), PayloadKind::Text);
        assert_eq!(classify(&[0x16, 0x03, 0x01]), PayloadKind::Encrypted);
        assert_eq!(classify(&[0xff, 0x00, 0x01]), PayloadKind::Binary);
    }

    #[test]
    fn len_matches_serialised() {
        let packet = sample();
        assert_eq!(packet.len(), packet.serialize().len());
    }
}
