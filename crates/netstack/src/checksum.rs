//! RFC 1071

pub fn internet_checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let (chunks, remainder) = data.as_chunks::<2>();
    for chunk in chunks {
        sum += u16::from_be_bytes(*chunk) as u32;
    }

    if let [last] = remainder {
        sum += (*last as u32) << 8;
    }

    while (sum >> 16) != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

pub fn verify(data: &[u8]) -> bool {
    internet_checksum(data) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc1071_example() {
        let data = [0x00, 0x01, 0xf2, 0x03, 0xf4, 0xf5, 0xf6, 0xf7];
        assert_eq!(internet_checksum(&data), 0x220d);
    }

    #[test]
    fn odd_length() {
        // Same words as the RFC example but with an extra zero byte.
        let data = [0x00, 0x01, 0xf2, 0x03, 0xf4, 0xf5, 0xf6, 0xf7, 0x00];
        assert_eq!(internet_checksum(&data), 0x220d);
    }

    #[test]
    fn checksum_verifies_to_zero() {
        let mut data = vec![0x45, 0x00, 0x00, 0x3c, 0x1c, 0x46, 0x40, 0x00, 0x40, 0x06];
        data.extend_from_slice(&[0, 0]); // checksum placeholder
        data.extend_from_slice(&[192, 168, 1, 10, 192, 168, 1, 20]);
        let sum = internet_checksum(&data);
        data[10] = (sum >> 8) as u8;
        data[11] = (sum & 0xff) as u8;
        assert!(verify(&data));
    }
}
