use crate::addr::MacAddr;

pub const ETHERNET_HEADER_LEN: usize = 14;
pub const ETHERTYPE_IPV4: u16 = 0x0800;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EthernetFrame {
    pub dst: MacAddr,
    pub src: MacAddr,
    pub ethertype: u16,
}

impl EthernetFrame {
    pub fn new(dst: MacAddr, src: MacAddr, ethertype: u16) -> Self {
        Self {
            dst,
            src,
            ethertype,
        }
    }

    pub fn parse(bytes: &[u8]) -> Option<(Self, &[u8])> {
        if bytes.len() < ETHERNET_HEADER_LEN {
            return None;
        }

        let dst = MacAddr(bytes[0..6].try_into().ok()?);
        let src = MacAddr(bytes[6..12].try_into().ok()?);
        let ethertype = u16::from_be_bytes([bytes[12], bytes[13]]);
        Some((
            Self {
                dst,
                src,
                ethertype,
            },
            &bytes[ETHERNET_HEADER_LEN..],
        ))
    }

    pub fn serialize_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.dst.0);
        out.extend_from_slice(&self.src.0);
        out.extend_from_slice(&self.ethertype.to_be_bytes());
    }

    pub fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(ETHERNET_HEADER_LEN);
        self.serialize_into(&mut out);
        out
    }
}
